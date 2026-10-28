use super::*;

#[derive(Default, Clone, Copy)]
enum Phase {
    #[default]
    Closed,
    Editing,
    Confirming,
}

#[derive(Clone)]
pub(super) struct CommitDialog {
    pub title: String,
    pub title_cursor: usize,
    pub description: String,
    pub description_cursor: usize,
    pub mode: CommitMode,
    phase: Phase,
}

impl Default for CommitDialog {
    fn default() -> Self {
        Self {
            title: String::new(),
            title_cursor: 0,
            description: String::new(),
            description_cursor: 0,
            mode: CommitMode::Normal,
            phase: Phase::Closed,
        }
    }
}

impl CommitDialog {
    pub fn visible(&self) -> bool {
        !matches!(self.phase, Phase::Closed)
    }
    pub fn confirming(&self) -> bool {
        matches!(self.phase, Phase::Confirming)
    }
    pub fn set_open(&mut self, open: bool) {
        self.phase = if open { Phase::Editing } else { Phase::Closed };
    }
    pub fn set_confirming(&mut self, confirming: bool) {
        if self.visible() {
            self.phase = if confirming {
                Phase::Confirming
            } else {
                Phase::Editing
            };
        }
    }
}

impl App {
    pub(super) fn commit_changes(&mut self) {
        match self.commit_dialog.mode.clone() {
            CommitMode::Reword(target) => {
                if self.commit_dialog.title.trim().is_empty() {
                    return;
                }
                self.execute_rebase_action(
                    &target,
                    RebaseAction::Reword {
                        title: self.commit_dialog.title.clone(),
                        description: self.commit_dialog.description.clone(),
                    },
                );
                return;
            }
            CommitMode::PlanReword(target) => {
                if self.commit_dialog.title.trim().is_empty() {
                    return;
                }
                if let Some(plan) = &mut self.rebase_plan {
                    if let Some(entry) = plan_entry_mut(&mut plan.plan, &target) {
                        entry.action = RebaseTodoAction::Reword;
                        entry.title = self.commit_dialog.title.clone();
                        entry.description = self.commit_dialog.description.clone();
                        self.commit_dialog.set_open(false);
                        self.commit_dialog.mode = CommitMode::Normal;
                        self.action_error = None;
                        self.focus = Focus::Graph;
                    }
                }
                return;
            }
            CommitMode::Edit(target) => {
                self.execute_rebase_action(&target, RebaseAction::Edit);
                return;
            }
            CommitMode::RebaseReplacement => {
                self.finish_rebase_edit();
                return;
            }
            _ => {}
        }
        let (amend, fixup) = match &self.commit_dialog.mode {
            CommitMode::Normal => (false, None),
            CommitMode::Amend => (true, None),
            CommitMode::Fixup(target) => (false, Some(target.as_str())),
            CommitMode::Reword(_)
            | CommitMode::PlanReword(_)
            | CommitMode::Edit(_)
            | CommitMode::RebaseReplacement => {
                unreachable!("rebase actions are handled above")
            }
        };
        let title = self.commit_dialog.title.clone();
        let description = self.commit_dialog.description.clone();
        let fixup = fixup.map(str::to_owned);
        self.start_local_job(
            "Commit",
            move |backend| backend.commit(&title, &description, amend, fixup.as_deref()),
            |app, completed| match &completed.result {
                Ok(()) => {
                    completed.refresh(app);
                    app.remote_notice = Some(("Commit created".into(), Instant::now()));
                }
                Err(error) => {
                    app.focus = Focus::Commit;
                    app.action_error = Some(error.clone());
                }
            },
        );
    }
    pub(super) fn finish_rebase_edit(&mut self) {
        let Some(state) = self.rebase_edit.clone() else {
            return;
        };
        let title = self.commit_dialog.title.clone();
        let description = self.commit_dialog.description.clone();
        self.start_local_job(
            "Replace commit",
            move |backend| {
                backend.finish_rebase_edit(&state.target, &state.parents, &title, &description)
            },
            |app, completed| match &completed.result {
                Ok(()) => {
                    completed.refresh(app);
                    app.rebase_edit = None;
                    app.rebase_paused = true;
                }
                Err(error) => app.show_rebase_error(error.clone()),
            },
        );
    }
    pub(super) fn open_commit_popup(
        &mut self,
        mode: CommitMode,
        title: String,
        description: String,
    ) {
        self.pending_fixup = false;
        self.commit_dialog.mode = mode;
        self.action_error = None;
        self.commit_dialog.title = title;
        self.commit_dialog.description = description;
        self.commit_dialog.title_cursor = self.commit_dialog.title.chars().count();
        self.commit_dialog.description_cursor = self.commit_dialog.description.chars().count();
        self.commit_field = CommitField::Title;
        self.commit_dialog.set_open(true);
        self.commit_dialog.set_confirming(false);
        self.focus = Focus::Commit;
    }
    pub(super) fn open_fixup_confirmation(&mut self, target: String) {
        self.commit_dialog.mode = CommitMode::Fixup(target);
        self.commit_dialog.title.clear();
        self.commit_dialog.title_cursor = 0;
        self.commit_dialog.description.clear();
        self.commit_dialog.description_cursor = 0;
        self.commit_field = CommitField::Commit;
        self.commit_dialog.set_open(true);
        self.commit_dialog.set_confirming(true);
        self.focus = Focus::Commit;
    }
    pub(super) fn open_edit_confirmation(&mut self) {
        self.pending_fixup = false;
        let Some(commit) = self.selected_rebase_commit() else {
            self.action_error = Some(self.rebase_selection_error().to_string());
            return;
        };
        self.commit_dialog.mode = CommitMode::Edit(commit.id);
        self.commit_dialog.title.clear();
        self.commit_dialog.title_cursor = 0;
        self.commit_dialog.description.clear();
        self.commit_dialog.description_cursor = 0;
        self.action_error = None;
        self.commit_field = CommitField::Commit;
        self.commit_dialog.set_open(true);
        self.commit_dialog.set_confirming(true);
        self.focus = Focus::Commit;
    }
    pub(super) fn open_rebase_replacement_popup(&mut self) {
        self.pending_fixup = false;
        let Some(state) = self.rebase_edit.as_ref() else {
            return;
        };
        if !self.commit_actions_available() {
            self.action_error = Some("Stage changes before replacing the edited commit".into());
            return;
        }
        self.open_commit_popup(
            CommitMode::RebaseReplacement,
            state.title.clone(),
            state.description.clone(),
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CommitMode {
    Normal,
    Amend,
    Fixup(String),
    Reword(String),
    Edit(String),
    RebaseReplacement,
    PlanReword(String),
}

pub(super) fn split_commit_message(commit: &Commit) -> (String, String) {
    (
        commit.summary.clone(),
        commit.message.trim_end().to_string(),
    )
}

impl App {
    pub(super) fn commit_actions_available(&self) -> bool {
        self.historical_commit.is_none()
            && self
                .local_status
                .iter()
                .any(|entry| !entry.is_conflict() && entry.index != ' ' && entry.index != '?')
    }
}

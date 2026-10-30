//! Interactive rebase planning, editing and execution.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RebaseEditState {
    pub(super) target: String,
    pub(super) parents: Vec<String>,
    pub(super) title: String,
    pub(super) description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RebasePlanState {
    pub(super) plan: RebasePlan,
    pub(super) selected: usize,
}

pub(super) fn rebase_plan_entry_count(plan: &RebasePlan) -> usize {
    plan.blocks
        .iter()
        .map(|block| match block {
            RebasePlanBlock::Control(_) => 0,
            RebasePlanBlock::Commits(entries) => entries.len(),
            RebasePlanBlock::Merge(_) => 1,
        })
        .sum()
}

pub(super) fn plan_entry_location(plan: &RebasePlan, selected: usize) -> Option<(usize, usize)> {
    let mut offset = 0usize;
    for (block_index, block) in plan.blocks.iter().enumerate() {
        let count = match block {
            RebasePlanBlock::Control(_) => 0,
            RebasePlanBlock::Commits(entries) => entries.len(),
            RebasePlanBlock::Merge(_) => 1,
        };
        if selected < offset + count {
            return Some((block_index, selected - offset));
        }
        offset += count;
    }
    None
}

pub(super) fn plan_entry_at(plan: &RebasePlan, selected: usize) -> Option<&RebasePlanEntry> {
    let (block_index, entry_index) = plan_entry_location(plan, selected)?;
    match &plan.blocks[block_index] {
        RebasePlanBlock::Commits(entries) => entries.get(entry_index),
        RebasePlanBlock::Merge(entry) if entry_index == 0 => Some(entry),
        RebasePlanBlock::Control(_) | RebasePlanBlock::Merge(_) => None,
    }
}

pub(super) fn plan_entry_mut<'a>(
    plan: &'a mut RebasePlan,
    id: &str,
) -> Option<&'a mut RebasePlanEntry> {
    for block in &mut plan.blocks {
        match block {
            RebasePlanBlock::Commits(entries) => {
                if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                    return Some(entry);
                }
            }
            RebasePlanBlock::Merge(entry) if entry.id == id => return Some(entry),
            RebasePlanBlock::Control(_) | RebasePlanBlock::Merge(_) => {}
        }
    }
    None
}

pub(super) fn rebase_todo_action_name(action: &RebaseTodoAction) -> &'static str {
    match action {
        RebaseTodoAction::Pick => "pick",
        RebaseTodoAction::Reword => "reword",
        RebaseTodoAction::Edit => "edit",
        RebaseTodoAction::Squash => "squash",
        RebaseTodoAction::Fixup => "fixup",
        RebaseTodoAction::Drop => "drop",
        RebaseTodoAction::Merge => "merge*",
    }
}

impl App {
    pub(super) fn execute_rebase_action(&mut self, target: &str, action: RebaseAction) {
        let target = target.to_owned();
        self.start_rebase_job(move |backend| backend.rebase_commit(&target, &action));
    }

    pub(super) fn start_rebase_job<F>(&mut self, operation: F)
    where
        F: FnOnce(&dyn GitBackend) -> anyhow::Result<RebaseOutcome> + Send + 'static,
    {
        self.start_local_job("Rebase", operation, |app, completed| {
            match &completed.result {
                Ok(outcome) => {
                    let outcome = outcome.clone();
                    let completed_rebase = matches!(&outcome, RebaseOutcome::Completed);
                    completed.refresh(app);
                    app.graph_action = None;
                    app.graph_action_return_focus = None;
                    app.rebase_plan = None;
                    app.apply_rebase_outcome(outcome);
                    if completed_rebase {
                        app.select_head_commit();
                        app.focus = Focus::Graph;
                    }
                }
                Err(error) => {
                    let error = error.clone();
                    let in_progress = app
                        .backend
                        .as_ref()
                        .is_some_and(|backend| backend.rebase_in_progress().unwrap_or(false));
                    if in_progress {
                        completed.refresh(app);
                        app.graph_action = None;
                        app.rebase_plan = None;
                        app.rebase_active = true;
                        app.rebase_paused = true;
                    }
                    app.show_rebase_error(error);
                }
            }
        });
    }

    pub(super) fn apply_rebase_outcome(&mut self, outcome: RebaseOutcome) {
        match outcome {
            RebaseOutcome::Completed => {
                self.rebase_paused = false;
                self.rebase_edit = None;
            }
            RebaseOutcome::StoppedAtCommit {
                target,
                parents,
                title,
                description,
            } => {
                self.rebase_paused = true;
                self.rebase_edit = Some(RebaseEditState {
                    target,
                    parents,
                    title,
                    description,
                });
            }
        }
    }

    pub(super) fn open_abort_rebase_confirmation(&mut self) {
        self.pending_fixup = false;
        self.rebase_active = self
            .backend
            .as_ref()
            .and_then(|backend| backend.rebase_in_progress().ok())
            .unwrap_or(false);
        if !self.rebase_active {
            self.action_error = Some("No Git rebase is in progress".into());
            return;
        }
        self.action_error = None;
        self.abort_rebase_popup = true;
        self.commit_field = CommitField::Commit;
        self.focus = Focus::Commit;
    }

    pub(super) fn abort_rebase(&mut self) {
        self.start_local_job(
            "Abort rebase",
            |backend| backend.abort_rebase(),
            |app, completed| match &completed.result {
                Ok(()) => {
                    completed.refresh(app);
                    app.rebase_edit = None;
                    app.rebase_paused = false;
                    app.select_head_commit();
                    app.focus = Focus::Graph;
                }
                Err(error) => app.show_rebase_error(error.clone()),
            },
        );
    }

    pub(super) fn continue_rebase(&mut self) {
        if self.rebase_edit.is_some() {
            self.action_error =
                Some("Create the replacement commit with c before continuing".into());
            return;
        }
        self.start_rebase_job(|backend| backend.continue_rebase());
    }

    pub(super) fn selected_rebase_commit(&self) -> Option<Commit> {
        self.graph.head_name.as_ref()?;
        let id = self.selected_commit_id()?;
        if !self.current_branch_ids.contains(&id) {
            return None;
        }
        self.graph
            .commits
            .iter()
            .find(|commit| commit.id == id)
            .cloned()
    }

    pub(super) fn rebase_selection_error(&self) -> &'static str {
        if self.graph.head_name.is_none() {
            "Reword/edit requires a checked-out branch"
        } else if self.selected_commit_id().is_none() {
            "Select a commit row in the Graph first"
        } else {
            "Select a commit reachable from the checked-out branch"
        }
    }

    pub(super) fn open_reword_popup(&mut self) {
        let Some(commit) = self.selected_rebase_commit() else {
            self.action_error = Some(self.rebase_selection_error().to_string());
            return;
        };
        let (title, description) = split_commit_message(&commit);
        self.open_commit_popup(CommitMode::Reword(commit.id), title, description);
    }

    pub(super) fn open_rebase_plan(&mut self) {
        self.pending_fixup = false;
        let Some(commit) = self.selected_rebase_commit() else {
            self.action_error = Some(self.rebase_selection_error().to_string());
            return;
        };
        self.start_local_job(
            "Prepare rebase",
            move |backend| backend.prepare_rebase_plan(&commit.id),
            |app, completed| match completed.result {
                Ok(plan) => {
                    app.rebase_plan = Some(RebasePlanState { plan, selected: 0 });
                    app.action_error = None;
                }
                Err(error) => app.show_rebase_error(error),
            },
        );
    }

    pub(super) fn apply_rebase_plan(&mut self) {
        let Some(plan) = self.rebase_plan.as_ref().map(|state| state.plan.clone()) else {
            return;
        };
        self.start_rebase_job(move |backend| backend.execute_rebase_plan(&plan));
    }

    pub(super) fn selected_plan_entry_location(&self) -> Option<(usize, usize)> {
        let plan = &self.rebase_plan.as_ref()?.plan;
        plan_entry_location(plan, self.rebase_plan.as_ref()?.selected)
    }

    pub(super) fn cycle_plan_action(&mut self, forward: bool) {
        let Some(state) = &mut self.rebase_plan else {
            return;
        };
        let Some((block_index, entry_index)) = plan_entry_location(&state.plan, state.selected)
        else {
            return;
        };
        let RebasePlanBlock::Commits(entries) = &mut state.plan.blocks[block_index] else {
            self.action_error = Some("Merge commits are fixed to preserve merge topology".into());
            return;
        };
        let choices: &[RebaseTodoAction] = if entry_index == 0 {
            &[
                RebaseTodoAction::Pick,
                RebaseTodoAction::Reword,
                RebaseTodoAction::Edit,
                RebaseTodoAction::Drop,
            ]
        } else {
            &[
                RebaseTodoAction::Pick,
                RebaseTodoAction::Reword,
                RebaseTodoAction::Edit,
                RebaseTodoAction::Squash,
                RebaseTodoAction::Fixup,
                RebaseTodoAction::Drop,
            ]
        };
        let entry = &mut entries[entry_index];
        let current = choices
            .iter()
            .position(|choice| *choice == entry.action)
            .unwrap_or(0);
        let next = if forward {
            (current + 1) % choices.len()
        } else {
            (current + choices.len() - 1) % choices.len()
        };
        entry.action = choices[next].clone();
        self.action_error = None;
    }

    pub(super) fn set_selected_plan_action(&mut self, action: RebaseTodoAction) {
        let Some(state) = &mut self.rebase_plan else {
            return;
        };
        let Some((block_index, entry_index)) = plan_entry_location(&state.plan, state.selected)
        else {
            return;
        };
        let RebasePlanBlock::Commits(entries) = &mut state.plan.blocks[block_index] else {
            self.action_error = Some("Merge commits are fixed to preserve merge topology".into());
            return;
        };
        if matches!(action, RebaseTodoAction::Squash | RebaseTodoAction::Fixup)
            && (entry_index == 0
                || matches!(&entries[entry_index - 1].action, RebaseTodoAction::Drop))
        {
            self.action_error =
                Some("Squash/fixup needs a preceding kept commit in this sequence".into());
            return;
        }
        entries[entry_index].action = action;
        self.action_error = None;
    }

    pub(super) fn edit_selected_plan_message(&mut self) {
        let Some((block_index, entry_index)) = self.selected_plan_entry_location() else {
            return;
        };
        let Some(state) = &mut self.rebase_plan else {
            return;
        };
        let RebasePlanBlock::Commits(entries) = &mut state.plan.blocks[block_index] else {
            self.action_error = Some("Merge messages are preserved with the merge topology".into());
            return;
        };
        let entry = &mut entries[entry_index];
        entry.action = RebaseTodoAction::Reword;
        let target = entry.id.clone();
        let title = entry.title.clone();
        let description = entry.description.clone();
        self.open_commit_popup(CommitMode::PlanReword(target), title, description);
    }

    pub(super) fn move_plan_selection(&mut self, delta: i32) {
        let Some(state) = &mut self.rebase_plan else {
            return;
        };
        let total = rebase_plan_entry_count(&state.plan);
        if total == 0 {
            return;
        }
        state.selected = if delta < 0 {
            state.selected.saturating_sub(1)
        } else {
            (state.selected + 1).min(total - 1)
        };
        self.action_error = None;
    }

    pub(super) fn reorder_selected_plan_entry(&mut self, delta: i32) {
        let Some(state) = &mut self.rebase_plan else {
            return;
        };
        let Some((block_index, entry_index)) = plan_entry_location(&state.plan, state.selected)
        else {
            return;
        };
        let RebasePlanBlock::Commits(entries) = &mut state.plan.blocks[block_index] else {
            self.action_error =
                Some("Merge commits cannot be moved without changing topology".into());
            return;
        };
        let target = if delta < 0 {
            entry_index.checked_sub(1)
        } else {
            (entry_index + 1 < entries.len()).then_some(entry_index + 1)
        };
        let Some(target) = target else { return };
        entries.swap(entry_index, target);
        state.selected = if delta < 0 {
            state.selected - 1
        } else {
            state.selected + 1
        };
        self.action_error = None;
    }

    pub(super) fn handle_rebase_plan_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.rebase_plan = None;
                self.action_error = None;
                self.focus = Focus::Graph;
            }
            KeyCode::Enter => self.apply_rebase_plan(),
            KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
                self.reorder_selected_plan_entry(-1);
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                self.reorder_selected_plan_entry(1);
            }
            KeyCode::Up => self.move_plan_selection(-1),
            KeyCode::Down => self.move_plan_selection(1),
            KeyCode::Home => {
                if let Some(plan) = &mut self.rebase_plan {
                    plan.selected = 0;
                }
            }
            KeyCode::End => {
                if let Some(plan) = &mut self.rebase_plan {
                    plan.selected = rebase_plan_entry_count(&plan.plan).saturating_sub(1);
                }
            }
            KeyCode::Left => self.cycle_plan_action(false),
            KeyCode::Right | KeyCode::Char(' ') => self.cycle_plan_action(true),
            KeyCode::Char('p') => self.set_selected_plan_action(RebaseTodoAction::Pick),
            KeyCode::Char('w') => self.edit_selected_plan_message(),
            KeyCode::Char('e') => self.set_selected_plan_action(RebaseTodoAction::Edit),
            KeyCode::Char('s') => self.set_selected_plan_action(RebaseTodoAction::Squash),
            KeyCode::Char('f') => self.set_selected_plan_action(RebaseTodoAction::Fixup),
            KeyCode::Char('d') => self.set_selected_plan_action(RebaseTodoAction::Drop),
            _ => {}
        }
    }
}

use super::*;

impl App {
    pub(super) fn commit_changes(&mut self) {
        match self.commit_mode.clone() {
            CommitMode::Reword(target) => {
                if self.commit_title.trim().is_empty() {
                    return;
                }
                self.execute_rebase_action(
                    &target,
                    RebaseAction::Reword {
                        title: self.commit_title.clone(),
                        description: self.commit_description.clone(),
                    },
                );
                return;
            }
            CommitMode::PlanReword(target) => {
                if self.commit_title.trim().is_empty() {
                    return;
                }
                if let Some(plan) = &mut self.rebase_plan {
                    if let Some(entry) = plan_entry_mut(&mut plan.plan, &target) {
                        entry.action = RebaseTodoAction::Reword;
                        entry.title = self.commit_title.clone();
                        entry.description = self.commit_description.clone();
                        self.commit_popup = false;
                        self.commit_mode = CommitMode::Normal;
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
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        let (amend, fixup) = match &self.commit_mode {
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
        if backend
            .commit(&self.commit_title, &self.commit_description, amend, fixup)
            .is_ok()
        {
            self.reload_after_commit();
        }
    }

    pub(super) fn execute_rebase_action(&mut self, target: &str, action: RebaseAction) {
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        match backend.rebase_commit(target, &action) {
            Ok(outcome) => {
                self.commit_popup = false;
                self.commit_confirm = false;
                self.action_error = None;
                self.apply_rebase_outcome(outcome);
            }
            Err(error) => self.show_rebase_error(error.to_string()),
        }
    }

    pub(super) fn apply_rebase_outcome(&mut self, outcome: RebaseOutcome) {
        self.reload_after_commit();
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

    pub(super) fn show_rebase_error(&mut self, error: String) {
        self.action_error = Some(error.clone());
        self.error_title = "Rebase error".into();
        self.error_popup = Some(error);
        self.error_scroll = 0;
    }

    pub(super) fn show_git_error(&mut self, error: String) {
        self.action_error = Some(error.clone());
        self.error_title = "Git operation error".into();
        self.error_popup = Some(error);
        self.error_scroll = 0;
    }

    pub(super) fn open_stash_manager(&mut self) {
        if self.rebase_active || self.three_way_view.is_some() {
            self.show_git_error(
                "Finish or abort the active conflict/rebase before managing stashes.".into(),
            );
            return;
        }
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        match backend.stash_list() {
            Ok(entries) => {
                self.stash_popup = Some(StashUiState {
                    entries,
                    selected: 0,
                    saving: false,
                    message: "WIP".into(),
                    message_cursor: 3,
                    include_untracked: false,
                    confirming_drop: false,
                });
            }
            Err(error) => self.show_git_error(error.to_string()),
        }
    }

    pub(super) fn refresh_stash_entries(&mut self) {
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        match backend.stash_list() {
            Ok(entries) => {
                if let Some(state) = self.stash_popup.as_mut() {
                    state.entries = entries;
                    state.selected = state.selected.min(state.entries.len().saturating_sub(1));
                }
            }
            Err(error) => self.show_git_error(error.to_string()),
        }
    }

    pub(super) fn save_stash(&mut self) {
        let Some(state) = self.stash_popup.as_ref() else {
            return;
        };
        let message = state.message.trim().to_string();
        let message = if message.is_empty() {
            "WIP".to_string()
        } else {
            message
        };
        let include_untracked = state.include_untracked;
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        match backend.stash_push(&message, include_untracked) {
            Ok(()) => {
                self.stash_popup = None;
                self.refresh_status();
                self.refresh_diff_stats();
            }
            Err(error) => self.show_git_error(error.to_string()),
        }
    }

    pub(super) fn apply_selected_stash(&mut self, pop: bool) {
        let Some(state) = self.stash_popup.as_ref() else {
            return;
        };
        let Some(entry) = state.entries.get(state.selected) else {
            return;
        };
        let reference = entry.reference.clone();
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        let result = if pop {
            backend.stash_pop(&reference)
        } else {
            backend.stash_apply(&reference)
        };
        self.stash_popup = None;
        self.refresh_status();
        self.refresh_diff_stats();
        if let Err(error) = result {
            self.show_git_error(error.to_string());
        }
    }

    pub(super) fn drop_selected_stash(&mut self) {
        let Some(state) = self.stash_popup.as_ref() else {
            return;
        };
        let Some(entry) = state.entries.get(state.selected) else {
            return;
        };
        let reference = entry.reference.clone();
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        match backend.stash_drop(&reference) {
            Ok(()) => self.refresh_stash_entries(),
            Err(error) => self.show_git_error(error.to_string()),
        }
    }

    pub(super) fn handle_stash_key(&mut self, key: KeyEvent) -> bool {
        let Some(state) = self.stash_popup.as_mut() else {
            return false;
        };
        if state.saving {
            match key.code {
                KeyCode::Esc => state.saving = false,
                KeyCode::Enter => self.save_stash(),
                KeyCode::Tab => state.include_untracked = !state.include_untracked,
                KeyCode::Left => state.message_cursor = state.message_cursor.saturating_sub(1),
                KeyCode::Right => {
                    state.message_cursor =
                        (state.message_cursor + 1).min(state.message.chars().count())
                }
                KeyCode::Home => state.message_cursor = 0,
                KeyCode::End => state.message_cursor = state.message.chars().count(),
                KeyCode::Backspace => {
                    backspace_at_cursor(&mut state.message, &mut state.message_cursor)
                }
                KeyCode::Delete => delete_at_cursor(&mut state.message, state.message_cursor),
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    insert_at_cursor(&mut state.message, &mut state.message_cursor, character);
                }
                _ => {}
            }
            return false;
        }
        if state.confirming_drop {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    if let Some(state) = self.stash_popup.as_mut() {
                        state.confirming_drop = false;
                    }
                    self.drop_selected_stash();
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    state.confirming_drop = false;
                }
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.stash_popup = None,
            KeyCode::Up | KeyCode::Char('k') => {
                state.selected = state.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.selected = (state.selected + 1).min(state.entries.len().saturating_sub(1));
            }
            KeyCode::Char('u') => state.include_untracked = !state.include_untracked,
            KeyCode::Char('s') => {
                state.saving = true;
                state.message.clear();
                state.message_cursor = 0;
            }
            KeyCode::Char('a') => self.apply_selected_stash(false),
            KeyCode::Char('p') => self.apply_selected_stash(true),
            KeyCode::Char('d') if !state.entries.is_empty() => state.confirming_drop = true,
            KeyCode::Enter => self.apply_selected_stash(false),
            _ => {}
        }
        false
    }

    pub(super) fn finish_rebase_edit(&mut self) {
        let Some(state) = self.rebase_edit.clone() else {
            return;
        };
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        match backend.finish_rebase_edit(
            &state.target,
            &state.parents,
            &self.commit_title,
            &self.commit_description,
        ) {
            Ok(()) => {
                self.rebase_edit = None;
                self.commit_popup = false;
                self.commit_confirm = false;
                self.action_error = None;
                self.reload_after_commit();
                self.rebase_paused = true;
            }
            Err(error) => self.show_rebase_error(error.to_string()),
        }
    }

    pub(super) fn open_commit_popup(
        &mut self,
        mode: CommitMode,
        title: String,
        description: String,
    ) {
        self.commit_mode = mode;
        self.action_error = None;
        self.commit_title = title;
        self.commit_description = description;
        self.commit_title_cursor = self.commit_title.chars().count();
        self.commit_description_cursor = self.commit_description.chars().count();
        self.commit_field = CommitField::Title;
        self.commit_popup = true;
        self.commit_confirm = false;
        self.focus = Focus::Commit;
    }

    pub(super) fn open_fixup_confirmation(&mut self, target: String) {
        self.commit_mode = CommitMode::Fixup(target);
        self.commit_title.clear();
        self.commit_title_cursor = 0;
        self.commit_description.clear();
        self.commit_description_cursor = 0;
        self.commit_field = CommitField::Commit;
        self.commit_popup = true;
        self.commit_confirm = true;
        self.focus = Focus::Commit;
    }

    pub(super) fn open_edit_confirmation(&mut self) {
        let Some(commit) = self.selected_rebase_commit() else {
            self.action_error = Some(self.rebase_selection_error().to_string());
            return;
        };
        self.commit_mode = CommitMode::Edit(commit.id);
        self.commit_title.clear();
        self.commit_title_cursor = 0;
        self.commit_description.clear();
        self.commit_description_cursor = 0;
        self.action_error = None;
        self.commit_field = CommitField::Commit;
        self.commit_popup = true;
        self.commit_confirm = true;
        self.focus = Focus::Commit;
    }

    pub(super) fn open_abort_rebase_confirmation(&mut self) {
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

    pub(super) fn open_rebase_replacement_popup(&mut self) {
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

    pub(super) fn abort_rebase(&mut self) {
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        match backend.abort_rebase() {
            Ok(()) => {
                self.abort_rebase_popup = false;
                self.rebase_edit = None;
                self.rebase_paused = false;
                self.action_error = None;
                self.reload_after_commit();
            }
            Err(error) => self.show_rebase_error(error.to_string()),
        }
    }

    pub(super) fn continue_rebase(&mut self) {
        if self.rebase_edit.is_some() {
            self.action_error =
                Some("Create the replacement commit with c before continuing".into());
            return;
        }
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        match backend.continue_rebase() {
            Ok(outcome) => {
                self.action_error = None;
                self.apply_rebase_outcome(outcome);
            }
            Err(error) => {
                self.rebase_active = backend.rebase_in_progress().unwrap_or(true);
                self.rebase_paused = self.rebase_active;
                self.refresh_status();
                self.show_rebase_error(error.to_string());
            }
        }
    }

    pub(super) fn selected_commit_id(&self) -> Option<String> {
        let has_local = !self.local_status.is_empty();
        if self.selected_graph < usize::from(has_local) {
            return None;
        }
        let row = self.selected_graph - usize::from(has_local);
        self.rows
            .get(row)
            .map(|row| self.graph.commits[row.commit_index].id.clone())
    }

    /// Human-readable label for the selected commit: a branch/tag name when one
    /// points at it, otherwise the short id.
    pub(super) fn selected_commit_label(&self) -> String {
        let Some(id) = self.selected_commit_id() else {
            return String::new();
        };
        let short = id.chars().take(7).collect::<String>();
        let reference = self.graph_display_refs.get(&id).and_then(|refs| {
            refs.iter()
                .find(|reference| matches!(reference.kind, RefKind::LocalBranch | RefKind::Tag))
                .or_else(|| refs.first())
        });
        match reference {
            Some(reference) => format!("{} ({short})", reference.name),
            None => short,
        }
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
        let Some(commit) = self.selected_rebase_commit() else {
            self.action_error = Some(self.rebase_selection_error().to_string());
            return;
        };
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        match backend.prepare_rebase_plan(&commit.id) {
            Ok(plan) => {
                self.rebase_plan = Some(RebasePlanState { plan, selected: 0 });
                self.action_error = None;
            }
            Err(error) => self.show_rebase_error(error.to_string()),
        }
    }

    pub(super) fn apply_rebase_plan(&mut self) {
        let Some(plan) = self.rebase_plan.as_ref().map(|state| state.plan.clone()) else {
            return;
        };
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        match backend.execute_rebase_plan(&plan) {
            Ok(outcome) => {
                self.rebase_plan = None;
                self.action_error = None;
                self.apply_rebase_outcome(outcome);
            }
            Err(error) => {
                let in_progress = backend.rebase_in_progress().unwrap_or(false);
                if in_progress {
                    self.rebase_plan = None;
                    self.rebase_active = true;
                    self.rebase_paused = true;
                    self.reload_after_commit();
                }
                self.show_rebase_error(error.to_string());
            }
        }
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

    pub(super) fn selected_current_branch(&self) -> bool {
        self.graph.head_name.is_some()
            && self
                .graph
                .head_id
                .as_ref()
                .is_some_and(|head| self.selected_commit_id().as_deref() == Some(head.as_str()))
    }

    pub(super) fn selected_local_branch(&self) -> Option<String> {
        let commit = self.selected_commit_id()?;
        let branches = self
            .graph
            .refs
            .iter()
            .filter(|reference| {
                reference.kind == crate::core::model::RefKind::LocalBranch && reference.id == commit
            })
            .collect::<Vec<_>>();
        if self.graph.head_id.as_deref() == Some(commit.as_str()) {
            if let Some(head_name) = self.graph.head_name.as_deref() {
                if branches.iter().any(|branch| branch.name == head_name) {
                    return Some(head_name.to_string());
                }
            }
        }
        branches
            .into_iter()
            .min_by(|left, right| left.name.cmp(&right.name))
            .map(|reference| reference.name.clone())
    }

    /// A tag pointing at the selected commit, used to push releases.
    pub(super) fn selected_tag(&self) -> Option<String> {
        let commit = self.selected_commit_id()?;
        self.graph
            .refs
            .iter()
            .filter(|reference| {
                reference.kind == crate::core::model::RefKind::Tag && reference.id == commit
            })
            .map(|reference| reference.name.clone())
            .min()
    }

    /// Branches and tags at the selected commit, branches first and each group
    /// alphabetical, so the delete popup can target either.
    pub(super) fn deletable_refs(&self) -> Vec<DeleteTarget> {
        let Some(commit) = self.selected_commit_id() else {
            return Vec::new();
        };
        let mut branches = Vec::new();
        let mut tags = Vec::new();
        for reference in &self.graph.refs {
            if reference.id != commit {
                continue;
            }
            match reference.kind {
                crate::core::model::RefKind::LocalBranch => {
                    branches.push(DeleteTarget::Branch(reference.name.clone()))
                }
                crate::core::model::RefKind::Tag => {
                    tags.push(DeleteTarget::Tag(reference.name.clone()))
                }
                _ => {}
            }
        }
        branches.sort_by(|left, right| left.name().cmp(right.name()));
        tags.sort_by(|left, right| left.name().cmp(right.name()));
        branches.extend(tags);
        branches
    }

    pub(super) fn open_delete_action(&mut self) {
        let targets = self.deletable_refs();
        if targets.is_empty() {
            self.show_git_error("Select a commit with a local branch or tag to delete".into());
            return;
        }
        self.delete_targets = targets;
        self.delete_target_index = 0;
        self.open_graph_action(GraphAction::Delete);
    }

    /// Open a graph action that operates on the local branch at the selected
    /// commit. Mirrors the push precondition and reports the same way instead
    /// of silently doing nothing when there is no branch.
    pub(super) fn open_local_branch_action(&mut self, action: GraphAction) {
        if self.selected_local_branch().is_none() {
            self.show_git_error(
                "Select a commit with a local branch before running this action".into(),
            );
            return;
        }
        self.open_graph_action(action);
    }

    pub(super) fn open_graph_action(&mut self, action: GraphAction) {
        if self.selected_commit_id().is_none() {
            self.show_git_error("Select a commit row before running this action".into());
            return;
        }
        self.graph_action_return_focus = Some(self.focus);
        self.graph_action = Some(action);
        self.graph_action_input.clear();
        self.graph_action_cursor = 0;
        self.action_error = None;
        self.reset_mode = 1;
        self.reset_focus = 1;
        self.remote_action = false;
        self.push_mode = 0;
        self.push_focus = 0;
        self.push_after_create = false;
        self.commit_field = match action {
            GraphAction::Branch | GraphAction::Tag | GraphAction::Rename => CommitField::Title,
            _ => CommitField::Commit,
        };
        self.focus = Focus::Commit;
    }

    pub(super) fn open_undo_popup(&mut self) {
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        match backend.undo_options() {
            Ok(options) if !options.is_empty() => {
                self.undo_options = options;
                self.undo_selection = 0;
                self.graph_action = Some(GraphAction::Undo);
                // Undo restores a previous state as staged changes, so land on
                // the Status pane (not the graph) to show them.
                self.graph_action_return_focus = None;
                self.action_error = None;
                self.commit_field = CommitField::Commit;
                self.focus = Focus::Commit;
            }
            Ok(_) => self.show_git_error("The HEAD reflog has no earlier state to restore.".into()),
            Err(error) => self.show_git_error(error.to_string()),
        }
    }

    pub(super) fn open_search_popup(&mut self) {
        self.search_popup = Some(SearchPopupState {
            query: String::new(),
            cursor: 0,
            matches: Vec::new(),
            selected: 0,
            searched: false,
            error: None,
        });
    }

    pub(super) fn handle_search_key(&mut self, key: KeyEvent) {
        let Some(state) = self.search_popup.as_mut() else {
            return;
        };
        let jump_to_result = state.searched
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
            && !state.matches.is_empty();
        let reset_search = state.searched && key.code == KeyCode::Char('/');
        match key.code {
            KeyCode::Esc => {
                self.search_popup = None;
                self.diff_cache = None;
            }
            KeyCode::Enter if state.searched => self.select_search_result(),
            KeyCode::Enter => self.start_commit_search(),
            KeyCode::Char('/') if state.searched => {
                state.query.clear();
                state.cursor = 0;
                state.matches.clear();
                state.selected = 0;
                state.searched = false;
                state.error = None;
            }
            KeyCode::Up if state.searched && !state.matches.is_empty() => {
                state.selected = if state.selected == 0 {
                    state.matches.len() - 1
                } else {
                    state.selected - 1
                };
            }
            KeyCode::Down if state.searched && !state.matches.is_empty() => {
                state.selected = (state.selected + 1) % state.matches.len();
            }
            KeyCode::Left if !state.searched => {
                state.cursor = state.cursor.saturating_sub(1);
            }
            KeyCode::Right if !state.searched => {
                state.cursor = (state.cursor + 1).min(state.query.chars().count());
            }
            KeyCode::Home if !state.searched => state.cursor = 0,
            KeyCode::End if !state.searched => state.cursor = state.query.chars().count(),
            KeyCode::Backspace if !state.searched => {
                backspace_at_cursor(&mut state.query, &mut state.cursor);
            }
            KeyCode::Delete if !state.searched => delete_at_cursor(&mut state.query, state.cursor),
            KeyCode::Char('u')
                if key.modifiers.contains(KeyModifiers::CONTROL) && !state.searched =>
            {
                state.query.clear();
                state.cursor = 0;
            }
            KeyCode::Char(character)
                if !state.searched
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                insert_at_cursor(&mut state.query, &mut state.cursor, character);
            }
            _ => {}
        }
        if jump_to_result {
            self.select_search_result();
        }
        if reset_search {
            self.diff_cache = None;
        }
    }

    pub(super) fn start_commit_search(&mut self) {
        let Some(state) = self.search_popup.as_mut() else {
            return;
        };
        let query = state.query.trim().to_string();
        if query.is_empty() {
            return;
        }
        let folded = query.to_lowercase();
        let position = self
            .graph
            .commits
            .iter()
            .enumerate()
            .filter(|(_, commit)| {
                commit.summary.to_lowercase().contains(&folded)
                    || commit.message.to_lowercase().contains(&folded)
            })
            .map(|(index, commit)| (index, commit.id.clone()))
            .collect::<Vec<_>>();
        state.matches = position.into_iter().map(|(_, id)| id).collect();
        state.selected = 0;
        state.searched = true;
        state.error = None;
        self.select_search_result();
    }

    pub(super) fn select_search_result(&mut self) {
        let Some(id) = self
            .search_popup
            .as_ref()
            .and_then(|state| state.matches.get(state.selected).cloned())
        else {
            return;
        };
        self.action_error = None;
        let local_offset = usize::from(!self.local_status.is_empty());
        if let Some(row) = self
            .rows
            .iter()
            .position(|row| self.graph.commits[row.commit_index].id == id)
        {
            self.focus = Focus::Graph;
            self.select_graph_index(row + local_offset);
        }
    }

    pub(super) fn move_undo_selection(&mut self, delta: i32) {
        if self.undo_options.is_empty() {
            return;
        }
        self.undo_selection = if delta < 0 {
            (self.undo_selection + self.undo_options.len() - 1) % self.undo_options.len()
        } else {
            (self.undo_selection + 1) % self.undo_options.len()
        };
    }

    pub(super) fn open_discard_popup(&mut self) {
        if self.status_section != StatusSection::Unstaged || self.selected_statuses.is_empty() {
            return;
        }
        self.discard_paths = self
            .selected_statuses
            .iter()
            .filter_map(|&index| self.status.get(index).map(|entry| entry.path.clone()))
            .collect();
        if self.discard_paths.is_empty() {
            return;
        }
        self.discard_paths.sort();
        self.action_error = None;
        self.graph_action = Some(GraphAction::Discard);
        self.graph_action_return_focus = None;
        self.commit_field = CommitField::Commit;
        self.focus = Focus::Commit;
    }

    pub(super) fn open_remote_action(&mut self, for_push: bool) {
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        let push_target = if for_push {
            if let Some(branch) = self.selected_local_branch() {
                Some(PushTarget::Branch(branch))
            } else if let Some(tag) = self.selected_tag() {
                Some(PushTarget::Tag(tag))
            } else {
                self.show_git_error(
                    "Select a commit with a local branch or tag before pushing".into(),
                );
                return;
            }
        } else {
            None
        };
        let remotes = match backend.remotes() {
            Ok(remotes) if !remotes.is_empty() => remotes,
            Ok(_) => {
                self.show_git_error("No Git remotes are configured for this repository".into());
                return;
            }
            Err(error) => {
                self.show_git_error(error.to_string());
                return;
            }
        };
        let default_remote = match push_target.as_ref() {
            Some(PushTarget::Branch(branch)) => backend
                .default_remote_for_branch(branch, true)
                .ok()
                .flatten(),
            Some(PushTarget::Tag(_)) => backend.default_remote(true).ok().flatten(),
            None => backend.default_remote(false).ok().flatten(),
        };
        self.remote_selection = default_remote
            .as_ref()
            .and_then(|remote| remotes.iter().position(|name| name == remote))
            .unwrap_or(0);
        if remotes.len() == 1 {
            let remote = remotes[0].clone();
            if for_push {
                self.push_target = push_target;
                self.open_push_mode(remote);
            } else {
                self.start_remote_operation(RemoteOperation::Fetch(remote));
            }
            return;
        }
        self.push_target = push_target;
        self.remote_choices = remotes;
        self.graph_action = Some(if for_push {
            GraphAction::PushRemote
        } else {
            GraphAction::Fetch
        });
        self.action_error = None;
        self.focus = Focus::Commit;
    }

    pub(super) fn open_push_mode(&mut self, remote: String) {
        self.push_remote = Some(remote);
        if self.push_target.is_none() {
            self.push_target = self
                .selected_local_branch()
                .map(PushTarget::Branch)
                .or_else(|| self.selected_tag().map(PushTarget::Tag));
        }
        self.open_graph_action(GraphAction::Push);
    }

    pub(super) fn move_remote_selection(&mut self, delta: i32) {
        if self.remote_choices.is_empty() {
            return;
        }
        self.remote_selection = if delta < 0 {
            (self.remote_selection + self.remote_choices.len() - 1) % self.remote_choices.len()
        } else {
            (self.remote_selection + 1) % self.remote_choices.len()
        };
    }

    pub(super) fn move_graph_popup_option(&mut self, forward: bool) {
        let Some(action) = self.graph_action else {
            return;
        };
        if action == GraphAction::Reset {
            self.reset_focus = if forward {
                (self.reset_focus + 1) % 4
            } else {
                (self.reset_focus + 3) % 4
            };
        } else if action == GraphAction::Push {
            self.push_focus = if forward {
                (self.push_focus + 1) % 4
            } else {
                (self.push_focus + 3) % 4
            };
        } else if action == GraphAction::Delete {
            let len = self.delete_targets.len();
            if len > 1 {
                self.delete_target_index = if forward {
                    (self.delete_target_index + 1) % len
                } else {
                    (self.delete_target_index + len - 1) % len
                };
            }
        } else if !matches!(
            action,
            GraphAction::Branch | GraphAction::Tag | GraphAction::Rename
        ) {
            self.commit_field = match (self.commit_field, forward) {
                (CommitField::Commit, true) | (CommitField::Cancel, false) => CommitField::Cancel,
                _ => CommitField::Commit,
            };
        }
    }

    pub(super) fn run_graph_action(&mut self) {
        let Some(action) = self.graph_action else {
            return;
        };
        if matches!(action, GraphAction::Fetch | GraphAction::PushRemote) {
            if let Some(remote) = self.remote_choices.get(self.remote_selection).cloned() {
                self.remote_choices.clear();
                self.remote_selection = 0;
                if action == GraphAction::Fetch {
                    self.start_remote_operation(RemoteOperation::Fetch(remote));
                } else {
                    self.open_push_mode(remote);
                }
            }
            return;
        }
        let remote_operation =
            match action {
                GraphAction::Pull => Some(RemoteOperation::Pull),
                GraphAction::Push => self.push_remote.clone().zip(self.push_target.clone()).map(
                    |(remote, target)| RemoteOperation::Push {
                        mode: self.push_mode,
                        remote,
                        target,
                    },
                ),
                _ => None,
            };
        if let Some(operation) = remote_operation {
            self.start_remote_operation(operation);
            return;
        }
        if action == GraphAction::Push {
            let message = if self.push_target.is_some() {
                "Select a remote before pushing"
            } else {
                "Select a commit with a local branch or tag before pushing"
            };
            self.show_git_error(message.into());
            return;
        }
        let target = self.selected_commit_id();
        let return_focus = self.graph_action_return_focus;
        let selection = GraphSelectionSnapshot {
            commit_id: target.clone(),
            row: self.selected_graph,
            had_local_row: !self.local_status.is_empty(),
            selected_local_row: !self.local_status.is_empty() && self.selected_graph == 0,
        };
        if !matches!(action, GraphAction::Discard | GraphAction::Undo) && target.is_none() {
            return;
        }
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        if action == GraphAction::RebaseOnto {
            let result = backend.rebase_onto(target.as_deref().unwrap());
            let in_progress = backend.rebase_in_progress().unwrap_or(false);
            match result {
                Ok(outcome) => {
                    self.graph_action = None;
                    self.graph_action_return_focus = None;
                    self.apply_rebase_outcome(outcome);
                }
                Err(error) => {
                    if in_progress {
                        self.graph_action = None;
                        self.graph_action_return_focus = None;
                        self.rebase_active = true;
                        self.rebase_paused = true;
                        self.reload_after_commit();
                    }
                    self.show_rebase_error(error.to_string());
                }
            }
            return;
        }
        let push_after_tag = action == GraphAction::Tag && self.push_after_create;
        let tag_name = self.graph_action_input.clone();
        let tag_remote = if push_after_tag {
            backend.default_remote(true).ok().flatten()
        } else {
            None
        };
        let result = match action {
            GraphAction::Branch => {
                backend.branch(&self.graph_action_input, target.as_deref().unwrap())
            }
            GraphAction::Tag => backend.tag(&self.graph_action_input, target.as_deref().unwrap()),
            GraphAction::Reset => backend.reset(
                target.as_deref().unwrap(),
                ["--soft", "--mixed", "--hard"][self.reset_mode],
            ),
            GraphAction::Checkout => backend.checkout(target.as_deref().unwrap()),
            GraphAction::CheckoutBranch => self.selected_local_branch().map_or_else(
                || Err(anyhow::anyhow!("no local branch at selected commit")),
                |branch| backend.checkout(&branch),
            ),
            GraphAction::CherryPick => backend.cherry_pick(target.as_deref().unwrap()),
            GraphAction::Revert => backend.revert(target.as_deref().unwrap()),
            GraphAction::RebaseOnto => unreachable!("rebase onto is handled before dispatch"),
            GraphAction::Rename => self.selected_local_branch().map_or_else(
                || Err(anyhow::anyhow!("no local branch at selected commit")),
                |old| backend.rename_branch(&old, &self.graph_action_input, self.remote_action),
            ),
            GraphAction::Delete => match self.delete_targets.get(self.delete_target_index) {
                Some(DeleteTarget::Branch(name)) => backend.delete_branch(name, self.remote_action),
                Some(DeleteTarget::Tag(name)) => backend.delete_tag(name, self.remote_action),
                None => Err(anyhow::anyhow!("no ref selected to delete")),
            },
            GraphAction::Pull => backend.pull(),
            GraphAction::Fetch | GraphAction::PushRemote | GraphAction::Push => {
                unreachable!("remote operation is dispatched to the background worker")
            }
            GraphAction::Undo => self.undo_options.get(self.undo_selection).map_or_else(
                || Err(anyhow::anyhow!("undo preview expired; open Undo again")),
                |info| backend.undo_ref_change(info),
            ),
            GraphAction::Discard => {
                let mut result = Ok(());
                for path in &self.discard_paths {
                    if let Err(error) = backend.discard(path) {
                        result = Err(error);
                        break;
                    }
                }
                result
            }
        };
        match result {
            Ok(()) => {
                self.reset_boundary = if action == GraphAction::Reset {
                    target
                } else {
                    None
                };
                self.graph_action = None;
                self.graph_action_return_focus = None;
                self.undo_options.clear();
                self.undo_selection = 0;
                if push_after_tag {
                    // Reload so the new tag is visible, then push it in the
                    // background like any other remote operation.
                    self.push_after_create = false;
                    self.reload_after_graph_action(selection);
                    match tag_remote {
                        Some(remote) => self.start_remote_operation(RemoteOperation::Push {
                            mode: 0,
                            remote,
                            target: PushTarget::Tag(tag_name),
                        }),
                        None => {
                            self.action_error =
                                Some("Tag created, but no remote is configured to push it".into())
                        }
                    }
                    return;
                }
                // Actions opened from the graph return there with the same
                // commit selected (falling forward if it is gone); actions
                // driven from the status/commit panels keep their old focus.
                if return_focus == Some(Focus::Graph) {
                    self.reload_after_graph_action(selection);
                } else {
                    self.reload_after_commit();
                }
            }
            Err(error) => self.action_error = Some(error.to_string()),
        }
    }

    pub(super) fn start_remote_operation(&mut self, operation: RemoteOperation) {
        if self.remote_operation.is_some() {
            return;
        }
        let Some(repository_path) = self.repository_path.clone() else {
            self.action_error =
                Some("Cannot resolve the repository path for the remote operation".into());
            return;
        };
        let sender = self.remote_tx.clone();
        let selection = GraphSelectionSnapshot {
            commit_id: self.selected_commit_id(),
            row: self.selected_graph,
            had_local_row: !self.local_status.is_empty(),
            selected_local_row: !self.local_status.is_empty() && self.selected_graph == 0,
        };
        self.remote_operation = Some(operation.clone());
        self.remote_started = Some(Instant::now());
        self.remote_notice = None;
        self.action_error = None;
        self.graph_action = None;
        self.focus = Focus::Graph;

        let repository_path_for_load = repository_path.clone();
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<_> {
                let backend = crate::backend::open(&repository_path)?;
                match &operation {
                    RemoteOperation::Fetch(remote) => backend.fetch(remote)?,
                    RemoteOperation::Pull => backend.pull()?,
                    RemoteOperation::Push {
                        mode,
                        remote,
                        target,
                    } => match target {
                        PushTarget::Branch(branch) => backend.push(remote, branch, *mode)?,
                        PushTarget::Tag(tag) => backend.push_tag(remote, tag, *mode)?,
                    },
                }
                drop(backend);
                load_repository_snapshot(&repository_path_for_load)
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(RemoteCompletion {
                operation,
                result,
                selection,
            });
        });
    }

    pub(super) fn reload_after_graph_action(&mut self, selection: GraphSelectionSnapshot) {
        let Some(path) = self.repository_path.clone() else {
            return;
        };
        if let Ok(snapshot) = load_repository_snapshot(&path) {
            self.reload_from_snapshot(snapshot);
            self.restore_graph_selection(selection);
        }
    }

    pub(super) fn restore_graph_selection(&mut self, selection: GraphSelectionSnapshot) {
        self.focus = Focus::Graph;
        let has_local = !self.local_status.is_empty();
        let local_offset = usize::from(has_local);
        let selected = if selection.selected_local_row && has_local {
            Some(0)
        } else {
            selection
                .commit_id
                .as_ref()
                .and_then(|id| {
                    self.rows
                        .iter()
                        .position(|row| self.graph.commits[row.commit_index].id == *id)
                })
                .or_else(|| {
                    (!self.rows.is_empty()).then(|| {
                        selection
                            .row
                            .saturating_sub(usize::from(selection.had_local_row))
                            .min(self.rows.len() - 1)
                            + local_offset
                    })
                })
        };
        if let Some(selected) = selected {
            self.select_graph_index(selected);
        } else {
            self.selected_graph = 0;
        }
    }

    pub(super) fn poll_remote_operation(&mut self) {
        loop {
            match self.remote_rx.try_recv() {
                Ok(completion) => {
                    self.remote_operation = None;
                    self.remote_started = None;
                    if matches!(&completion.operation, RemoteOperation::Push { .. }) {
                        self.push_remote = None;
                        self.push_target = None;
                    }
                    match completion.result {
                        Ok(snapshot) => {
                            self.reload_from_snapshot(snapshot);
                            self.restore_graph_selection(completion.selection);
                            self.remote_notice = Some((
                                format!("{} complete", completion.operation.label()),
                                Instant::now(),
                            ));
                        }
                        Err(error) => {
                            self.remote_notice = None;
                            self.show_git_error(format!(
                                "{} failed: {error}",
                                completion.operation.label()
                            ));
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.remote_operation = None;
                    self.remote_started = None;
                    self.show_git_error("Remote operation worker stopped unexpectedly".into());
                    break;
                }
            }
        }
        if self
            .remote_notice
            .as_ref()
            .is_some_and(|(_, since)| since.elapsed() > Duration::from_secs(3))
        {
            self.remote_notice = None;
        }
    }

    pub(super) fn start_update_check(&mut self, manual: bool) {
        if matches!(
            self.update_status,
            UpdateStatus::Checking | UpdateStatus::Installing
        ) {
            return;
        }
        self.update_status = UpdateStatus::Checking;
        self.update_status_since = Some(Instant::now());
        let sender = self.update_tx.clone();
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<UpdateAvailability> {
                if !manual && update_check_is_recent() {
                    return Ok(UpdateAvailability::SkippedRecentlyChecked);
                }
                let mut updater = AxoUpdater::new_for(env!("CARGO_PKG_NAME"));
                // Only cargo-dist installations have a receipt that identifies
                // their release source and install path. Leave source builds
                // and package-manager installs to their own update mechanism.
                if updater.load_receipt().is_err() {
                    return Ok(UpdateAvailability::UnsupportedInstall);
                }
                record_update_check();
                if updater.is_update_needed_sync()? {
                    Ok(UpdateAvailability::Available)
                } else {
                    Ok(UpdateAvailability::Current)
                }
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(UpdateEvent::Check { manual, result });
        });
    }

    pub(super) fn install_update(&mut self) {
        if self.update_status != UpdateStatus::Available {
            return;
        }
        self.update_status = UpdateStatus::Installing;
        self.update_status_since = Some(Instant::now());
        let sender = self.update_tx.clone();
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<bool> {
                let mut updater = AxoUpdater::new_for(env!("CARGO_PKG_NAME"));
                updater.load_receipt()?;
                // Keep the installer subprocess from writing into the TUI's
                // alternate screen; progress is reported in the status bar.
                updater.disable_installer_output();
                Ok(updater.run_sync()?.is_some())
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(UpdateEvent::Install(result));
        });
    }

    pub(super) fn poll_update(&mut self) {
        while let Ok(event) = self.update_rx.try_recv() {
            match event {
                UpdateEvent::Check { manual, result } => {
                    self.update_status = match result {
                        Ok(UpdateAvailability::Available) => UpdateStatus::Available,
                        Ok(UpdateAvailability::Current) if manual => UpdateStatus::Current,
                        Ok(UpdateAvailability::UnsupportedInstall) if manual => {
                            UpdateStatus::UnsupportedInstall
                        }
                        Ok(UpdateAvailability::SkippedRecentlyChecked) => UpdateStatus::Idle,
                        Ok(_) => UpdateStatus::Idle,
                        Err(error) if manual => UpdateStatus::Failed(error),
                        Err(_) => UpdateStatus::Idle,
                    };
                    self.update_status_since = Some(Instant::now());
                }
                UpdateEvent::Install(Ok(true)) => {
                    self.update_status = UpdateStatus::RestartRequired;
                    self.update_status_since = Some(Instant::now());
                }
                UpdateEvent::Install(Ok(false)) => {
                    self.update_status = UpdateStatus::Current;
                    self.update_status_since = Some(Instant::now());
                }
                UpdateEvent::Install(Err(error)) => {
                    self.update_status = UpdateStatus::Failed(error);
                    self.update_status_since = Some(Instant::now());
                }
            }
        }

        let transient = matches!(
            self.update_status,
            UpdateStatus::Current | UpdateStatus::UnsupportedInstall | UpdateStatus::Failed(_)
        );
        if transient
            && self
                .update_status_since
                .is_some_and(|since| since.elapsed() > Duration::from_secs(8))
        {
            self.update_status = UpdateStatus::Idle;
            self.update_status_since = None;
        }
    }

    pub(super) fn poll_repository_refresh(&mut self) {
        let mut watched = false;
        while self.watch_rx.try_recv().is_ok() {
            watched = true;
        }
        if watched {
            // Events produced by our own git writes are coalesced into one
            // refresh once the quiet window ends; external changes that land
            // inside the window are deferred rather than dropped.
            let delay = match self.ignore_watch_until {
                Some(until) if Instant::now() < until => {
                    until.saturating_duration_since(Instant::now()) + Duration::from_millis(50)
                }
                _ => Duration::from_millis(300),
            };
            self.refresh_deadline = Some(Instant::now() + delay);
        }

        while let Ok(completion) = self.refresh_rx.try_recv() {
            let initial_load = self.loading;
            self.refresh_in_progress = false;
            if initial_load {
                self.loading = false;
                self.loading_started = None;
            }
            // A refresh that started before the app's own latest mutation would
            // report stale status (e.g. a file the app just staged still shows
            // as conflicted). Drop it instead of clobbering the fresh state.
            if completion.generation != self.refresh_generation {
                continue;
            }
            match completion.result {
                Ok(snapshot) => {
                    self.watch_error = None;
                    let choose_initial_focus = initial_load;
                    let initial_status_has_changes =
                        choose_initial_focus && !snapshot.status.is_empty();
                    self.reload_external_snapshot(snapshot);
                    if choose_initial_focus {
                        self.focus = if initial_status_has_changes {
                            Focus::Status
                        } else {
                            Focus::Graph
                        };
                        if !initial_status_has_changes {
                            self.select_head_commit();
                        }
                    }
                }
                Err(error) if initial_load => {
                    self.loading = false;
                    self.loading_started = None;
                    self.show_git_error(error);
                }
                Err(error) => self.watch_error = Some(error),
            }
        }

        let ready = self
            .refresh_deadline
            .is_some_and(|deadline| Instant::now() >= deadline);
        if !ready || self.refresh_in_progress || self.remote_operation.is_some() {
            return;
        }
        if self.graph_action.is_some()
            || self.commit_popup
            || self.abort_rebase_popup
            || self.rebase_plan.is_some()
            || self.error_popup.is_some()
            || self.stash_popup.is_some()
        {
            return;
        }
        self.refresh_deadline = None;
        let Some(path) = self.repository_path.clone() else {
            return;
        };
        let sender = self.refresh_tx.clone();
        self.refresh_in_progress = true;
        let generation = self.refresh_generation;
        thread::spawn(move || {
            let result = load_repository_snapshot(&path).map_err(|error| format!("{error:#}"));
            let _ = sender.send(RepositoryRefreshCompletion { generation, result });
        });
    }

    /// Append the next history page when the selection nears the last row.
    pub(super) fn poll_graph_page(&mut self) {
        while let Ok(page) = self.graph_page_rx.try_recv() {
            if page.generation != self.graph_generation {
                continue;
            }
            self.graph_page_in_progress = false;
            if page.skip != self.graph.commits.len() {
                continue; // graph was reloaded; drop the stale page
            }
            let page = match page.result {
                Ok(page) => page,
                Err(error) => {
                    self.show_git_error(error);
                    return;
                }
            };
            if page.commits.is_empty() {
                self.graph_complete = true;
                continue;
            }
            // Take the engine out so the `known` set can borrow the commit ids.
            let mut engine = std::mem::take(&mut self.layout_engine);
            let rows = if page.complete {
                let known: HashSet<&str> = self
                    .graph
                    .commits
                    .iter()
                    .map(|commit| commit.id.as_str())
                    .chain(page.commits.iter().map(|commit| commit.id.as_str()))
                    .collect();
                engine.extend(&page.commits, Some(&known))
            } else {
                engine.extend(&page.commits, None)
            };
            self.layout_engine = engine;
            self.max_lanes = self.max_lanes.max(layout::max_lanes(&rows));
            self.rows.extend(rows);
            self.graph.commits.extend(page.commits);
            self.graph_complete = page.complete;
        }

        // Load the next page once the selection actually reaches the oldest
        // loaded row, so a short first page does not eagerly pull everything.
        let loaded_rows = self.rows.len() + usize::from(!self.local_status.is_empty());
        let at_end = loaded_rows > 0 && self.selected_graph + 1 >= loaded_rows;
        if at_end
            && !self.graph_complete
            && !self.graph_page_in_progress
            && !self.loading
            && self.remote_operation.is_none()
        {
            self.start_graph_page();
        }
    }

    pub(super) fn start_graph_page(&mut self) {
        let Some(path) = self.repository_path.clone() else {
            return;
        };
        let skip = self.graph.commits.len();
        let generation = self.graph_generation;
        let sender = self.graph_page_tx.clone();
        self.graph_page_in_progress = true;
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<GraphWindow> {
                let backend = crate::backend::open(&path)?;
                backend.graph_window(skip, graph_page_size())
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(GraphPageCompletion {
                skip,
                generation,
                result,
            });
        });
    }

    pub(super) fn poll_graph_details(&mut self) {
        while let Ok(completion) = self.graph_details_rx.try_recv() {
            self.graph_details_in_progress = false;
            if self.historical_commit.as_deref() != Some(completion.commit_id.as_str()) {
                continue;
            }
            match completion.result {
                Ok(details) => {
                    self.status = details.files;
                    self.refresh_status_indices();
                    self.selected_status = 0;
                    self.selected_statuses.clear();
                    if !self.status.is_empty() {
                        self.selected_statuses.insert(0);
                    }
                    self.status_section = StatusSection::Unstaged;
                    self.diff_stats.clear();
                    self.diff_hunk_counts.clear();
                    self.diff_stats.extend(
                        details
                            .stats
                            .into_iter()
                            .map(|(path, added, removed)| ((path, false), (added, removed))),
                    );
                    if let Some((path, diff, number_width)) = details.diff {
                        self.diff_path = Some(path);
                        self.diff_number_width = number_width;
                        self.diff_full = false;
                        self.diff_truncated = None;
                        self.apply_diff_text(diff);
                    } else {
                        self.diff_path = None;
                        self.diff_number_width = 3;
                        self.diff_lines.clear();
                    }
                    self.selected_diff_hunk = 0;
                    self.selected_diff_line = None;
                    self.diff_scroll = 0;
                    self.diff_cache = None;
                    self.details_loading = false;
                    self.details_error = None;
                }
                Err(error) => {
                    self.details_loading = false;
                    self.details_error = Some(error);
                }
            }
        }

        let ready = self
            .graph_details_deadline
            .is_some_and(|deadline| Instant::now() >= deadline);
        if !ready || self.graph_details_in_progress || self.remote_operation.is_some() {
            return;
        }
        if self.graph_action.is_some()
            || self.commit_popup
            || self.abort_rebase_popup
            || self.rebase_plan.is_some()
            || self.error_popup.is_some()
            || self.stash_popup.is_some()
        {
            return;
        }
        self.graph_details_deadline = None;
        let Some(commit_id) = self.historical_commit.clone() else {
            return;
        };
        let Some(path) = self.repository_path.clone() else {
            return;
        };
        let sender = self.graph_details_tx.clone();
        self.graph_details_in_progress = true;
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<_> {
                let backend = crate::backend::open(&path)?;
                let files = backend.commit_files(&commit_id)?;
                let stats = backend.commit_diff_stats(&commit_id)?;
                let diff = if let Some(file) = files.first() {
                    Some((
                        file.path.clone(),
                        backend.commit_diff(&commit_id, &file.path)?,
                        backend.file_line_number_width(&file.path, Some(&commit_id))?,
                    ))
                } else {
                    None
                };
                Ok(HistoricalCommitDetails { files, stats, diff })
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(GraphDetailsCompletion { commit_id, result });
        });
    }

    /// Select the checked-out commit so the graph opens where work is happening.
    fn select_head_commit(&mut self) {
        let Some(head) = self.graph.head_id.as_deref() else {
            return;
        };
        let local_offset = usize::from(!self.local_status.is_empty());
        let Some(row) = self
            .rows
            .iter()
            .position(|row| self.graph.commits[row.commit_index].id == head)
        else {
            return;
        };
        self.select_graph_index(row + local_offset);
    }

    pub(super) fn reload_external_snapshot(&mut self, snapshot: RepositorySnapshot) {
        let RepositorySnapshot {
            graph,
            status: local_status,
            rows,
            max_lanes,
            layout: layout_engine,
            complete,
        } = snapshot;
        if graph == self.graph && local_status == self.local_status {
            return;
        }
        let graph_display_refs = graph.display_refs_by_commit();
        let tracking_by_branch = graph
            .branch_tracking
            .iter()
            .cloned()
            .map(|tracking| (tracking.branch.clone(), tracking))
            .collect();
        let branch_ids = current_branch_ids(&graph);
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        self.graph_generation = self.graph_generation.wrapping_add(1);
        self.layout_engine = layout_engine;
        self.graph_complete = complete;
        self.graph_page_in_progress = false;

        let selected_commit = self.selected_commit_id();
        let selected_path = self
            .status
            .get(self.selected_status)
            .map(|entry| entry.path.clone());
        let old_section = self.status_section;
        let old_selected_graph = self.selected_graph;
        let historical = self.historical_commit.clone();
        let focus = self.focus;

        self.conflict_commits = if local_status.iter().any(StatusEntry::is_conflict) {
            backend.conflict_commits().ok().flatten()
        } else {
            None
        };
        self.active_conflict_paths = conflict_paths(&local_status);
        self.graph = graph;
        self.graph_display_refs = graph_display_refs;
        self.tracking_by_branch = tracking_by_branch;
        self.current_branch_ids = branch_ids;
        self.rows = rows;
        self.max_lanes = max_lanes;
        self.local_status = local_status;
        self.status = if let Some(commit) = historical.as_deref() {
            backend
                .commit_files(commit)
                .unwrap_or_else(|_| self.local_status.clone())
        } else {
            self.local_status.clone()
        };
        self.historical_commit = historical;
        self.rebase_active = backend.rebase_in_progress().unwrap_or(self.rebase_active);

        let local_offset = usize::from(!self.local_status.is_empty());
        let selected_row = selected_commit.as_ref().and_then(|id| {
            self.rows
                .iter()
                .position(|row| self.graph.commits[row.commit_index].id == *id)
                .or_else(|| {
                    // An external rewrite (a rebase, amend, ...) can remove the
                    // selected commit. Follow HEAD instead of landing on an
                    // arbitrary row.
                    self.graph.head_id.as_deref().and_then(|head| {
                        self.rows
                            .iter()
                            .position(|row| self.graph.commits[row.commit_index].id == head)
                    })
                })
        });
        self.selected_graph = selected_row
            .map(|row| row + local_offset)
            .unwrap_or_else(|| {
                old_selected_graph.min(self.rows.len().saturating_sub(1) + local_offset)
            });
        self.scroll_to_selected_graph = true;
        self.keep_selected_graph_node_visible();
        self.refresh_status_indices();
        let old_section_indices = self.section_indices(old_section);
        self.status_section = if old_section_indices.is_empty() {
            match old_section {
                StatusSection::Unstaged if !self.staged_indices.is_empty() => StatusSection::Staged,
                StatusSection::Staged if !self.unstaged_indices.is_empty() => {
                    StatusSection::Unstaged
                }
                _ => old_section,
            }
        } else {
            old_section
        };
        let indices = self.section_indices(self.status_section).to_vec();
        self.selected_status = selected_path
            .as_ref()
            .and_then(|path| {
                indices
                    .iter()
                    .copied()
                    .find(|&index| self.status[index].path == *path)
            })
            .or_else(|| indices.first().copied())
            .unwrap_or(0);
        self.selected_statuses.clear();
        if !indices.is_empty() {
            self.selected_statuses.insert(self.selected_status);
        }
        self.focus = focus;
        self.refresh_diff();
        self.refresh_diff_stats();
    }

    pub(super) fn open_amend_popup(&mut self) {
        let id = self
            .graph
            .head_id
            .as_deref()
            .or_else(|| self.graph.commits.first().map(|c| c.id.as_str()));
        let Some(id) = id else { return };
        let Some(commit) = self.graph.commits.iter().find(|commit| commit.id == id) else {
            return;
        };
        self.open_commit_popup(
            CommitMode::Amend,
            commit.summary.clone(),
            commit.message.clone(),
        );
    }

    pub(super) fn refresh_diff_stats(&mut self) {
        self.diff_stats.clear();
        self.diff_hunk_counts.clear();
        let Some(path) = self.repository_path.clone() else {
            return;
        };
        self.diff_stats_generation = self.diff_stats_generation.wrapping_add(1);
        let generation = self.diff_stats_generation;
        let entries = self.status.clone();
        let historical_commit = self.historical_commit.clone();
        let sender = self.diff_stats_tx.clone();
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<DiffStatsSnapshot> {
                let backend = crate::backend::open(&path)?;
                if let Some(commit) = historical_commit {
                    let stats = backend
                        .commit_diff_stats(&commit)?
                        .into_iter()
                        .map(|(path, added, removed)| PathDiffStats {
                            path,
                            staged: false,
                            added,
                            removed,
                        })
                        .collect();
                    return Ok(DiffStatsSnapshot {
                        stats,
                        hunks: Vec::new(),
                    });
                }
                let stats = backend.status_diff_stats(&entries)?;
                let hunks = backend.status_hunk_counts(&entries)?;
                Ok(DiffStatsSnapshot { stats, hunks })
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(DiffStatsCompletion { generation, result });
        });
    }

    pub(super) fn poll_diff_stats(&mut self) {
        while let Ok(completion) = self.diff_stats_rx.try_recv() {
            if completion.generation != self.diff_stats_generation {
                continue;
            }
            let Ok(snapshot) = completion.result else {
                continue;
            };
            self.diff_stats.clear();
            self.diff_hunk_counts.clear();
            for stat in snapshot.stats {
                self.diff_stats
                    .insert((stat.path, stat.staged), (stat.added, stat.removed));
            }
            self.diff_hunk_counts.extend(snapshot.hunks);
        }
    }

    pub(super) fn section_indices(&self, section: StatusSection) -> &[usize] {
        match section {
            StatusSection::Staged => &self.staged_indices,
            StatusSection::Unstaged => &self.unstaged_indices,
        }
    }

    pub(super) fn move_status_selection(&mut self, delta: i32, extend: bool) {
        if self.historical_commit.is_some() {
            if self.status.is_empty() {
                return;
            }
            self.selected_status = if delta < 0 {
                self.selected_status.saturating_sub(1)
            } else {
                (self.selected_status + 1).min(self.status.len() - 1)
            };
            self.selected_statuses.clear();
            self.selected_statuses.insert(self.selected_status);
            self.refresh_diff();
            return;
        }
        let indices = self.section_indices(self.status_section).to_vec();
        if indices.is_empty() {
            return;
        }
        let pos = indices
            .iter()
            .position(|&i| i == self.selected_status)
            .unwrap_or(0);
        let at_boundary = (delta < 0 && pos == 0) || (delta > 0 && pos + 1 >= indices.len());
        if at_boundary {
            let other = match self.status_section {
                StatusSection::Unstaged => StatusSection::Staged,
                StatusSection::Staged => StatusSection::Unstaged,
            };
            let other_indices = self.section_indices(other);
            if !other_indices.is_empty() {
                let next = if delta < 0 {
                    *other_indices.last().unwrap()
                } else {
                    other_indices[0]
                };
                self.status_section = other;
                self.selected_status = next;
                self.selected_statuses.clear();
                self.selected_statuses.insert(next);
                self.refresh_diff();
                return;
            }
        }
        let next = if delta < 0 {
            pos.saturating_sub(1)
        } else {
            (pos + 1).min(indices.len() - 1)
        };
        self.selected_status = indices[next];
        if extend {
            let start = pos.min(next);
            let end = pos.max(next);
            for &index in &indices[start..=end] {
                self.selected_statuses.insert(index);
            }
        } else {
            self.selected_statuses.clear();
            self.selected_statuses.insert(self.selected_status);
        }
        self.refresh_diff();
    }

    pub(super) fn ensure_status_selection(&mut self) {
        let indices = self.section_indices(self.status_section);
        if !indices.is_empty() && !indices.contains(&self.selected_status) {
            self.selected_status = indices[0];
            self.selected_statuses.clear();
            self.selected_statuses.insert(self.selected_status);
            self.refresh_diff();
        }
    }

    pub(super) fn selected_conflict_path(&self) -> Option<GitPath> {
        if let Some(view) = &self.three_way_view {
            let still_unresolved = self
                .backend
                .as_ref()
                .and_then(|backend| backend.status().ok())
                .is_some_and(|status| {
                    status
                        .iter()
                        .any(|entry| entry.path == view.path && entry.is_conflict())
                });
            if still_unresolved {
                return Some(view.path.clone());
            }
        }
        if !matches!(self.focus, Focus::Status | Focus::Diff)
            || self.status_section != StatusSection::Unstaged
        {
            return None;
        }
        self.status
            .get(self.selected_status)
            .filter(|entry| entry.is_conflict())
            .map(|entry| entry.path.clone())
    }

    pub(super) fn choose_selected_conflict_side(&mut self, side: ConflictSide) {
        let Some(path) = self.selected_conflict_path() else {
            return;
        };
        let hunk_choice = self
            .three_way_view
            .as_ref()
            .filter(|view| view.path == path)
            .and_then(|view| {
                let working = String::from_utf8_lossy(&view.working);
                let hunks = conflict_hunks(&working);
                (!hunks.is_empty() && side != ConflictSide::Base).then(|| {
                    replace_conflict_hunk(&working, view.hunk_index, side).map(String::into_bytes)
                })
            });
        if let Some(candidate) = hunk_choice {
            match candidate {
                Some(candidate) => {
                    if let Some(view) = self
                        .three_way_view
                        .as_mut()
                        .filter(|view| view.path == path)
                    {
                        view.pending_working = Some(candidate);
                        view.result_preview = true;
                        view.chosen_side = Some(side);
                        view.scroll_y = match side {
                            ConflictSide::Ours => {
                                diff_hunk_scroll(&view.ours_diff, view.hunk_index)
                            }
                            ConflictSide::Theirs => {
                                diff_hunk_scroll(&view.theirs_diff, view.hunk_index)
                            }
                            ConflictSide::Base => None,
                        }
                        .unwrap_or(0);
                    }
                    self.refresh_diff();
                }
                None => self.action_error = Some("No conflict hunk is selected".into()),
            }
            return;
        }

        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        match backend.choose_conflict_side(&path, side) {
            Ok(()) => {
                if let Some(view) = self
                    .three_way_view
                    .as_mut()
                    .filter(|view| view.path == path)
                {
                    view.pending_working = None;
                    view.result_preview = true;
                    view.chosen_side = Some(side);
                }
                self.refresh_diff();
            }
            Err(error) => self.show_rebase_error(error.to_string()),
        }
    }

    pub(super) fn confirm_selected_conflict_result(&mut self) {
        let Some(view) = self
            .three_way_view
            .as_ref()
            .filter(|view| view.result_preview)
        else {
            return;
        };
        let path = view.path.clone();
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        let pending_working = view.pending_working.clone();
        if let Some(working) = pending_working.as_deref() {
            if let Err(error) = backend.write_conflict_result(&path, working) {
                self.show_rebase_error(error.to_string());
                return;
            }
        }
        let current_working = backend
            .conflict_versions(&path)
            .ok()
            .flatten()
            .and_then(|versions| versions.working)
            .unwrap_or_default();
        let remaining_hunks = conflict_hunks(&String::from_utf8_lossy(&current_working)).len();
        if remaining_hunks > 0 {
            if let Some(view) = self
                .three_way_view
                .as_mut()
                .filter(|view| view.path == path)
            {
                view.working = current_working;
                view.pending_working = None;
                view.hunk_index = view.hunk_index.min(remaining_hunks - 1);
                view.result_preview = false;
                view.chosen_side = None;
                view.scroll_y = diff_hunk_scroll(&view.ours_diff, view.hunk_index)
                    .or_else(|| diff_hunk_scroll(&view.theirs_diff, view.hunk_index))
                    .unwrap_or(0);
            }
            self.refresh_diff();
        } else {
            match backend.stage(&path) {
                Ok(()) => self.focus_after_conflict_resolution(&path),
                Err(error) => self.show_rebase_error(error.to_string()),
            }
        }
    }

    pub(super) fn move_conflict_hunk(&mut self, delta: i32) -> bool {
        let Some(view) = self.three_way_view.as_mut() else {
            return false;
        };
        let count = conflict_hunks(&String::from_utf8_lossy(&view.working)).len();
        if count == 0 {
            return false;
        }
        view.hunk_index = if delta < 0 {
            (view.hunk_index + count - 1) % count
        } else {
            (view.hunk_index + 1) % count
        };
        view.pending_working = None;
        view.result_preview = false;
        view.chosen_side = None;
        view.scroll_y = diff_hunk_scroll(&view.ours_diff, view.hunk_index)
            .or_else(|| diff_hunk_scroll(&view.theirs_diff, view.hunk_index))
            .unwrap_or(0);
        true
    }

    /// The rendered row index of hunk `hunk`'s `@@` header.
    fn hunk_header_row(&self, hunk: usize) -> Option<usize> {
        let target = *self.hunk_header_indices.get(hunk)?;
        let rows = &self.diff_cache.as_ref()?.1;
        rows.iter().position(|row| {
            let (old, new) = row.input_indices();
            old == Some(target) || new == Some(target)
        })
    }

    pub(super) fn diff_row_count(&self) -> usize {
        self.diff_cache.as_ref().map_or(0, |cache| cache.1.len())
    }

    pub(super) fn move_diff_hunk(&mut self, delta: i32) {
        let headers = self.hunk_header_indices.len();
        if headers == 0 {
            return;
        }
        self.selected_diff_hunk = if delta < 0 {
            (self.selected_diff_hunk + headers - 1) % headers
        } else {
            (self.selected_diff_hunk + 1) % headers
        };
        self.ensure_diff_layout();
        let header_row = self.hunk_header_row(self.selected_diff_hunk).unwrap_or(0);
        let viewport = usize::from(self.diff_area.height.saturating_sub(2));
        let max_scroll = self.diff_row_count().saturating_sub(viewport);
        self.diff_scroll = header_row.min(max_scroll).min(u16::MAX as usize) as u16;
    }

    /// Scroll the diff to the previous/next hunk based on the current scroll
    /// position, without changing the staging selection. This lets `[`/`]`
    /// navigate hunks in a read-only commit diff as well.
    pub(super) fn jump_diff_hunk(&mut self, delta: i32) {
        if self.hunk_header_indices.is_empty() {
            return;
        }
        self.ensure_diff_layout();
        let headers = (0..self.hunk_header_indices.len())
            .filter_map(|hunk| self.hunk_header_row(hunk))
            .collect::<Vec<_>>();
        let Some(&first) = headers.first() else {
            return;
        };
        let last = *headers.last().expect("headers is non-empty");
        let current = self.diff_scroll as usize;
        let target = if delta < 0 {
            headers
                .iter()
                .rev()
                .copied()
                .find(|&header| header < current)
                .unwrap_or(last)
        } else {
            headers
                .iter()
                .copied()
                .find(|&header| header > current)
                .unwrap_or(first)
        };
        let viewport = usize::from(self.diff_area.height.saturating_sub(2));
        let max_scroll = self.diff_row_count().saturating_sub(viewport);
        self.diff_scroll = target.min(max_scroll).min(u16::MAX as usize) as u16;
    }

    /// Move to the previous/next hunk of the focused diff. In hunk mode this
    /// also moves the staging selection; otherwise it only scrolls. Diffs
    /// without hunks fall back to single-line scrolling.
    pub(super) fn navigate_diff_hunk(&mut self, delta: i32) {
        if self.hunk_header_indices.is_empty() {
            if delta < 0 {
                self.diff_scroll = self.diff_scroll.saturating_sub(1);
            } else {
                self.diff_scroll = self.diff_scroll.saturating_add(1);
            }
            return;
        }
        if self.hunk_mode {
            self.move_diff_hunk(delta);
        } else {
            self.jump_diff_hunk(delta);
        }
    }

    /// The furthest the focused diff can be panned horizontally, based on its
    /// widest line and the current pane width.
    fn diff_max_scroll_x(&self) -> u16 {
        let width = usize::from(self.diff_area.width.saturating_sub(2));
        if width == 0 || self.diff_lines.is_empty() {
            return 0;
        }
        let widest = self
            .diff_lines
            .iter()
            .map(|line| line.text.chars().count().saturating_sub(1))
            .max()
            .unwrap_or(0);
        let content = match self.diff_mode {
            DiffMode::Unified => width.saturating_sub(self.diff_number_width * 2 + 3),
            DiffMode::SideBySide => {
                let half = width.saturating_sub(3) / 2;
                half.saturating_sub(self.diff_number_width + 1)
            }
        };
        widest.saturating_sub(content).min(u16::MAX as usize) as u16
    }

    /// Pan the focused diff horizontally by `delta` display columns.
    pub(super) fn pan_diff_horizontal(&mut self, delta: i32) {
        let step = delta.unsigned_abs() as u16;
        self.diff_scroll_x = if delta < 0 {
            self.diff_scroll_x.saturating_sub(step)
        } else {
            self.diff_scroll_x
                .saturating_add(step)
                .min(self.diff_max_scroll_x())
        };
    }

    /// Pages the focused panel up (-1) or down (1) by a screen.
    pub(super) fn page_focus(&mut self, delta: i32) {
        let page = 10;
        match self.focus {
            Focus::Graph => {
                let total = self.rows.len() + usize::from(!self.local_status.is_empty());
                if total == 0 {
                    return;
                }
                let selected = if delta < 0 {
                    self.selected_graph.saturating_sub(page)
                } else {
                    (self.selected_graph + page).min(total - 1)
                };
                self.select_graph_index(selected);
            }
            Focus::Status => self.move_status_selection(delta * page as i32, false),
            Focus::Diff => {
                if let Some(view) = &mut self.three_way_view {
                    view.scroll_y = if delta < 0 {
                        view.scroll_y.saturating_sub(page)
                    } else {
                        view.scroll_y.saturating_add(page)
                    };
                } else if delta < 0 {
                    self.diff_scroll = self.diff_scroll.saturating_sub(page as u16);
                } else {
                    self.diff_scroll = self.diff_scroll.saturating_add(page as u16);
                }
            }
            Focus::Details => {
                self.details_scroll = if delta < 0 {
                    self.details_scroll.saturating_sub(page as u16)
                } else {
                    self.details_scroll.saturating_add(page as u16)
                };
            }
            Focus::Commit => {}
        }
    }

    pub(super) fn toggle_hunk_mode(&mut self) {
        if self.historical_commit.is_some() || self.three_way_view.is_some() {
            return;
        }
        if self.line_mode {
            self.line_mode = false;
            self.selected_diff_line = None;
            if let Some(mode) = self.diff_mode_before_line.take() {
                self.diff_mode = mode;
            }
        }
        if !self.hunk_mode && self.hunk_header_indices.is_empty() {
            self.show_git_error("This file has no text hunks; use file mode to stage it.".into());
            return;
        }
        self.hunk_mode = !self.hunk_mode;
        self.action_error = None;
        self.diff_cache = None;
        if self.hunk_mode {
            self.selected_diff_hunk = 0;
            self.diff_scroll = 0;
            self.focus = Focus::Diff;
        } else {
            // File mode has no partial-diff selection highlight.
            self.selected_diff_hunk = 0;
            if self.focus == Focus::Diff {
                self.focus = Focus::Status;
            }
        }
    }

    pub(super) fn toggle_line_mode(&mut self) {
        if self.historical_commit.is_some() || self.three_way_view.is_some() {
            return;
        }
        if self.line_mode {
            self.line_mode = false;
            self.selected_diff_line = None;
            if let Some(mode) = self.diff_mode_before_line.take() {
                self.diff_mode = mode;
            }
            self.diff_cache = None;
            if self.focus == Focus::Diff {
                self.focus = Focus::Status;
            }
            return;
        }
        let Some(selected) = self
            .diff_lines
            .iter()
            .position(|line| matches!(line.kind, DiffKind::Added | DiffKind::Removed))
        else {
            self.show_git_error(
                "This diff has no added or removed lines to stage individually.".into(),
            );
            return;
        };
        self.diff_mode_before_line = Some(self.diff_mode);
        self.diff_mode = DiffMode::Unified;
        self.hunk_mode = false;
        self.line_mode = true;
        self.selected_diff_line = Some(selected);
        self.diff_scroll = selected.min(u16::MAX as usize) as u16;
        self.diff_cache = None;
        self.focus = Focus::Diff;
    }

    pub(super) fn select_file_mode(&mut self) {
        if self.line_mode {
            if let Some(mode) = self.diff_mode_before_line.take() {
                self.diff_mode = mode;
            }
        }
        self.line_mode = false;
        self.hunk_mode = false;
        self.selected_diff_line = None;
        self.selected_diff_hunk = 0;
        self.action_error = None;
        self.diff_cache = None;
    }

    pub(super) fn move_diff_line(&mut self, delta: i32) {
        let changed = self
            .diff_lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| {
                matches!(line.kind, DiffKind::Added | DiffKind::Removed).then_some(index)
            })
            .collect::<Vec<_>>();
        if changed.is_empty() {
            return;
        }
        let position = self
            .selected_diff_line
            .and_then(|selected| changed.iter().position(|&index| index == selected))
            .unwrap_or(0);
        let next = if delta < 0 {
            (position + changed.len() - 1) % changed.len()
        } else {
            (position + 1) % changed.len()
        };
        let selected = changed[next];
        self.selected_diff_line = Some(selected);
        let viewport = usize::from(self.diff_area.height.saturating_sub(2));
        self.diff_scroll = selected
            .min(self.diff_lines.len().saturating_sub(viewport))
            .min(u16::MAX as usize) as u16;
        self.diff_cache = None;
    }

    pub(super) fn apply_selected_diff_hunk(&mut self) {
        if self.historical_commit.is_some() || self.three_way_view.is_some() {
            return;
        }
        let Some(path) = self.diff_path.clone() else {
            return;
        };
        if self.hunk_header_indices.is_empty() {
            self.apply_selected_file_changes();
            return;
        }
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        if let Err(error) = backend.apply_hunk(&path, self.diff_target, self.selected_diff_hunk) {
            self.show_git_error(error.to_string());
            return;
        }

        let source_section = self.status_section;
        let destination_section = match source_section {
            StatusSection::Unstaged => StatusSection::Staged,
            StatusSection::Staged => StatusSection::Unstaged,
        };
        self.refresh_status();
        let source_index = self
            .section_indices(source_section)
            .iter()
            .copied()
            .find(|&index| self.status[index].path == path);
        let destination_index = self
            .section_indices(destination_section)
            .iter()
            .copied()
            .find(|&index| self.status[index].path == path);
        if let Some(index) = source_index.or(destination_index) {
            self.status_section = if source_index.is_some() {
                source_section
            } else {
                destination_section
            };
            self.selected_status = index;
            self.selected_statuses.clear();
            self.selected_statuses.insert(index);
        }
        self.refresh_diff();
    }

    pub(super) fn apply_selected_diff_line(&mut self) {
        if !self.line_mode || self.historical_commit.is_some() || self.three_way_view.is_some() {
            return;
        }
        let Some(path) = self.diff_path.clone() else {
            return;
        };
        let Some(line) = self
            .selected_diff_line
            .and_then(|index| self.diff_lines.get(index))
            .filter(|line| matches!(line.kind, DiffKind::Added | DiffKind::Removed))
        else {
            self.apply_selected_file_changes();
            return;
        };
        let selection = DiffLineSelection {
            old_line: line.old_no,
            new_line: line.new_no,
            text: line.text.clone(),
        };
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        if let Err(error) = backend.apply_diff_line(&path, self.diff_target, &selection) {
            self.show_git_error(error.to_string());
            return;
        }

        let source_section = self.status_section;
        let destination_section = match source_section {
            StatusSection::Unstaged => StatusSection::Staged,
            StatusSection::Staged => StatusSection::Unstaged,
        };
        self.selected_diff_line = None;
        self.refresh_status();
        let source_index = self
            .section_indices(source_section)
            .iter()
            .copied()
            .find(|&index| self.status[index].path == path);
        let destination_index = self
            .section_indices(destination_section)
            .iter()
            .copied()
            .find(|&index| self.status[index].path == path);
        if let Some(index) = source_index.or(destination_index) {
            self.status_section = if source_index.is_some() {
                source_section
            } else {
                destination_section
            };
            self.selected_status = index;
            self.selected_statuses.clear();
            self.selected_statuses.insert(index);
        }
        self.refresh_diff();
    }

    pub(super) fn apply_selected_file_changes(&mut self) {
        if self.historical_commit.is_some() || self.three_way_view.is_some() {
            return;
        }
        let indices = self.section_indices(self.status_section);
        let Some(&current) = indices.iter().find(|&&index| index == self.selected_status) else {
            return;
        };
        let selected = self
            .selected_statuses
            .iter()
            .copied()
            .filter(|index| indices.contains(index))
            .collect::<Vec<_>>();
        let paths = if selected.is_empty() {
            vec![self.status[current].path.clone()]
        } else {
            selected
                .iter()
                .map(|&index| self.status[index].path.clone())
                .collect()
        };
        let source_section = self.status_section;
        let current_position = indices
            .iter()
            .position(|&index| index == current)
            .unwrap_or(0);
        let next_source_paths = indices
            .iter()
            .copied()
            .skip(current_position + 1)
            .chain(indices.iter().copied().take(current_position))
            .map(|index| self.status[index].path.clone())
            .filter(|path| !paths.contains(path))
            .collect::<Vec<_>>();
        let destination_section = match source_section {
            StatusSection::Unstaged => StatusSection::Staged,
            StatusSection::Staged => StatusSection::Unstaged,
        };
        let result = if let Some(backend) = self.backend.as_ref() {
            let mut result = Ok(());
            for path in &paths {
                let current_result = match source_section {
                    StatusSection::Unstaged => backend.stage(path),
                    StatusSection::Staged => backend.unstage(path),
                };
                if let Err(error) = current_result {
                    result = Err(error);
                    break;
                }
            }
            result
        } else {
            Ok(())
        };
        if let Err(error) = result {
            self.show_git_error(error.to_string());
            return;
        }

        self.refresh_status();
        let remaining_source = self.section_indices(source_section).to_vec();
        let next_source = next_source_paths.iter().find_map(|path| {
            remaining_source
                .iter()
                .copied()
                .find(|&index| self.status[index].path == *path)
        });
        if let Some(index) = next_source {
            self.status_section = source_section;
            self.selected_status = index;
            self.selected_statuses.clear();
            self.selected_statuses.insert(index);
        } else {
            let destination = self.section_indices(destination_section).to_vec();
            let next = destination
                .iter()
                .copied()
                .find(|&index| paths.contains(&self.status[index].path))
                .or_else(|| destination.first().copied());
            self.status_section = destination_section;
            if let Some(index) = next {
                self.selected_status = index;
                self.selected_statuses.clear();
                self.selected_statuses.insert(index);
            } else {
                self.selected_status = 0;
                self.selected_statuses.clear();
            }
        }
        self.refresh_diff();
    }

    pub(super) fn open_selected_conflict_editor(&mut self) {
        if let Some(path) = self.selected_conflict_path() {
            self.pending_conflict_editor = Some(path);
        }
    }

    pub(super) fn focus_after_conflict_resolution(&mut self, resolved_path: &GitPath) {
        self.historical_commit = None;
        self.refresh_status();
        self.focus = Focus::Status;
        if let Some(conflict) = self.status.iter().position(StatusEntry::is_conflict) {
            self.status_section = StatusSection::Unstaged;
            self.selected_status = conflict;
        } else if let Some(index) = self
            .status
            .iter()
            .position(|entry| entry.path == *resolved_path)
        {
            self.status_section = StatusSection::Staged;
            self.selected_status = index;
        } else if let Some(&index) = self.staged_indices.first() {
            self.status_section = StatusSection::Staged;
            self.selected_status = index;
        } else if let Some(&index) = self.unstaged_indices.first() {
            self.status_section = StatusSection::Unstaged;
            self.selected_status = index;
        } else {
            self.selected_status = 0;
        }
        self.selected_statuses.clear();
        if !self.status.is_empty() {
            self.selected_statuses.insert(self.selected_status);
        }
        self.refresh_diff();
    }

    pub(super) fn run_conflict_editor(
        &mut self,
        terminal: &mut ratatui::DefaultTerminal,
        path: &GitPath,
    ) -> Result<()> {
        terminal.hide_cursor()?;
        crossterm::terminal::disable_raw_mode()?;
        execute!(
            std::io::stdout(),
            DisableMouseCapture,
            crossterm::terminal::LeaveAlternateScreen
        )?;

        let edit_result = self
            .backend
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("repository backend is unavailable"))?
            .edit_conflict_file(path);

        let restore_result = (|| -> Result<()> {
            execute!(
                std::io::stdout(),
                crossterm::terminal::EnterAlternateScreen,
                EnableMouseCapture
            )?;
            crossterm::terminal::enable_raw_mode()?;
            terminal.clear()?;
            Ok(())
        })();
        restore_result?;
        edit_result?;

        if let Some(view) = self
            .three_way_view
            .as_mut()
            .filter(|view| view.path == *path)
        {
            view.result_preview = true;
            view.chosen_side = None;
        }
        self.refresh_diff();
        Ok(())
    }
}

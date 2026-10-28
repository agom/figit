use super::*;

impl App {
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
        self.pending_fixup = false;
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
        self.pending_fixup = false;
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
        if action == GraphAction::RebaseOnto {
            let target = target.unwrap();
            self.start_rebase_job(move |backend| backend.rebase_onto(&target));
            return;
        }
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        let push_after_tag = action == GraphAction::Tag && self.push_after_create;
        let tag_name = self.graph_action_input.clone();
        let tag_remote = if push_after_tag {
            backend.default_remote(true).ok().flatten()
        } else {
            None
        };
        let input = self.graph_action_input.clone();
        let reset_mode = self.reset_mode;
        let remote_action = self.remote_action;
        let branch = self.selected_local_branch();
        let delete = self.delete_targets.get(self.delete_target_index).cloned();
        let undo = self.undo_options.get(self.undo_selection).cloned();
        let discard_paths = self.discard_paths.clone();
        let completed_target = target.clone();
        self.start_local_job(
            action.label(),
            move |backend| match action {
                GraphAction::Branch => backend.branch(&input, target.as_deref().unwrap()),
                GraphAction::Tag => backend.tag(&input, target.as_deref().unwrap()),
                GraphAction::Reset => backend.reset(
                    target.as_deref().unwrap(),
                    ["--soft", "--mixed", "--hard"][reset_mode],
                ),
                GraphAction::Checkout => backend.checkout(target.as_deref().unwrap()),
                GraphAction::CheckoutBranch => branch.map_or_else(
                    || Err(anyhow::anyhow!("no local branch at selected commit")),
                    |branch| backend.checkout(&branch),
                ),
                GraphAction::CherryPick => backend.cherry_pick(target.as_deref().unwrap()),
                GraphAction::Revert => backend.revert(target.as_deref().unwrap()),
                GraphAction::RebaseOnto => unreachable!("rebase onto is handled before dispatch"),
                GraphAction::Rename => branch.map_or_else(
                    || Err(anyhow::anyhow!("no local branch at selected commit")),
                    |old| backend.rename_branch(&old, &input, remote_action),
                ),
                GraphAction::Delete => match delete.as_ref() {
                    Some(DeleteTarget::Branch(name)) => backend.delete_branch(name, remote_action),
                    Some(DeleteTarget::Tag(name)) => backend.delete_tag(name, remote_action),
                    None => Err(anyhow::anyhow!("no ref selected to delete")),
                },
                GraphAction::Pull => backend.pull(),
                GraphAction::Fetch | GraphAction::PushRemote | GraphAction::Push => {
                    unreachable!("remote operation is dispatched to the background worker")
                }
                GraphAction::Undo => undo.as_ref().map_or_else(
                    || Err(anyhow::anyhow!("undo preview expired; open Undo again")),
                    |info| backend.undo_ref_change(info),
                ),
                GraphAction::Discard => {
                    let mut result = Ok(());
                    for path in &discard_paths {
                        if let Err(error) = backend.discard(path) {
                            result = Err(error);
                            break;
                        }
                    }
                    result
                }
            },
            move |app, completed| match &completed.result {
                Ok(()) => {
                    app.reset_boundary = if action == GraphAction::Reset {
                        completed_target
                    } else {
                        None
                    };
                    completed.refresh(app);
                    app.graph_action = None;
                    app.graph_action_return_focus = None;
                    app.undo_options.clear();
                    app.undo_selection = 0;
                    if push_after_tag || return_focus == Some(Focus::Graph) {
                        app.restore_graph_selection(selection);
                    }
                    if push_after_tag {
                        app.push_after_create = false;
                        match tag_remote {
                            Some(remote) => app.start_remote_operation(RemoteOperation::Push {
                                mode: 0,
                                remote,
                                target: PushTarget::Tag(tag_name),
                            }),
                            None => {
                                app.action_error = Some(
                                    "Tag created, but no remote is configured to push it".into(),
                                )
                            }
                        }
                    }
                }
                Err(error) => app.action_error = Some(error.clone()),
            },
        );
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

    pub(super) fn section_indices(&self, section: StatusSection) -> &[usize] {
        match section {
            StatusSection::Staged => &self.staged_indices,
            StatusSection::Unstaged => &self.unstaged_indices,
        }
    }

    pub(super) fn status_list_scroll(&self, indices: &[usize], area: Rect) -> usize {
        let viewport = area.height.saturating_sub(2) as usize;
        indices
            .iter()
            .position(|&i| i == self.selected_status)
            .unwrap_or(0)
            .saturating_sub(viewport.saturating_sub(1))
            .min(u16::MAX as usize)
    }

    /// Refresh working contents without rebuilding graph selection or jumping
    /// away from the user's current viewport on every save.
    pub(super) fn refresh_visible_content(&mut self) {
        if self.historical_commit.is_some() && self.three_way_view.is_none() {
            return;
        }
        let scroll = self.diff_scroll;
        let horizontal = self.diff_scroll_x;
        self.refresh_diff();
        self.ensure_diff_layout();
        let rows = self.diff_cache.as_ref().map_or(0, |(_, rows)| rows.len());
        let viewport = self.diff_area.height.saturating_sub(2) as usize;
        self.diff_scroll =
            scroll.min(rows.saturating_sub(viewport.max(1)).min(u16::MAX as usize) as u16);
        self.diff_scroll_x = horizontal.min(self.diff_max_scroll_x());
        if let Some(view) = self.three_way_view.as_mut() {
            view.clamp_scroll();
        }
        self.refresh_diff_stats();
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
        if self.focus == Focus::Diff {
            return self
                .three_way_view
                .as_ref()
                .filter(|view| self.active_conflict_paths.contains(&view.path))
                .map(|view| view.path.clone());
        }
        if self.focus != Focus::Status || self.status_section != StatusSection::Unstaged {
            return None;
        }
        self.status
            .get(self.selected_status)
            .filter(|entry| entry.is_conflict())
            .map(|entry| entry.path.clone())
    }

    pub(super) fn cancel_conflict_preview(&mut self) {
        if let Some(view) = self.three_way_view.as_mut() {
            view.cancel_preview();
        }
    }

    pub(super) fn choose_selected_conflict_side(&mut self, side: ConflictSide) {
        let Some(path) = self.selected_conflict_path() else {
            return;
        };
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        let versions = match backend.conflict_versions(&path) {
            Ok(Some(versions)) => versions,
            Ok(None) => {
                self.show_rebase_error(
                    "Conflict versions are unavailable; refresh or use an external editor.".into(),
                );
                return;
            }
            Err(error) => {
                self.show_rebase_error(error.to_string());
                return;
            }
        };
        if self
            .three_way_view
            .as_ref()
            .is_some_and(|view| view.versions != versions)
        {
            self.refresh_diff();
        }
        let Some(view) = self
            .three_way_view
            .as_mut()
            .filter(|view| view.path == path)
        else {
            return;
        };
        let selected = match side {
            ConflictSide::Base => versions.base,
            ConflictSide::Ours => versions.ours,
            ConflictSide::Theirs => versions.theirs,
        };
        let mut resolution =
            selected.map_or(ConflictResolution::Delete, ConflictResolution::Content);
        // Missing stages mean deletion. Base always restores the whole file;
        // ours/theirs replace just the selected text hunk when one is present.
        if side != ConflictSide::Base && resolution.content().is_some() {
            if let Some(text) = versions
                .working
                .as_deref()
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
            {
                if !conflict_hunks(text).is_empty() {
                    let Some(candidate) = replace_conflict_hunk(text, view.hunk_index, side) else {
                        self.action_error = Some("No conflict hunk is selected".into());
                        return;
                    };
                    resolution = ConflictResolution::Content(candidate.into_bytes());
                }
            }
        }
        view.preview(resolution, side);
        self.focus = Focus::Diff;
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
        let result = (|| -> Result<Vec<u8>> {
            let current = backend.conflict_versions(&path)?.ok_or_else(|| {
                anyhow::anyhow!("Conflict versions are unavailable; resolution was not staged.")
            })?;
            let expected = (!view.result_missing).then_some(view.working.as_slice());
            if current.working.as_deref() != expected {
                anyhow::bail!("Conflict file changed; preview cancelled. Review the current contents before confirming.");
            }
            if let Some(resolution) = view.pending_resolution.as_ref() {
                let whole_file_side = view.chosen_side.filter(|side| {
                    *side != ConflictSide::Base
                        && std::str::from_utf8(&view.working)
                            .map_or(true, |text| conflict_hunks(text).is_empty())
                });
                if let Some(side) = whole_file_side {
                    backend.choose_conflict_side(&path, side)?;
                } else {
                    backend.write_conflict_result(&path, resolution)?;
                }
            }
            let updated = backend.conflict_versions(&path)?.ok_or_else(|| {
                anyhow::anyhow!("Could not verify the conflict result; resolution was not staged.")
            })?;
            Ok(updated.working.unwrap_or_default())
        })();
        let current_working = match result {
            Ok(working) => working,
            Err(error) => {
                self.cancel_conflict_preview();
                self.refresh_diff();
                self.show_rebase_error(error.to_string());
                return;
            }
        };
        let remaining_hunks = conflict_hunks(&String::from_utf8_lossy(&current_working)).len();
        if remaining_hunks > 0 {
            if let Some(view) = self
                .three_way_view
                .as_mut()
                .filter(|view| view.path == path)
            {
                view.working = current_working;
                view.pending_resolution = None;
                view.hunk_index = view.hunk_index.min(remaining_hunks - 1);
                view.result_preview = false;
                view.chosen_side = None;
            }
            self.refresh_diff();
            if let Some(view) = self.three_way_view.as_mut() {
                view.jump_to_hunk();
            }
        } else if let Some(backend) = self.backend.as_ref() {
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
        let count = view.hunks.len();
        if count == 0 {
            return false;
        }
        view.cancel_preview();
        view.hunk_index = if delta < 0 {
            (view.hunk_index + count - 1) % count
        } else {
            (view.hunk_index + 1) % count
        };
        view.jump_to_hunk();
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
        let width = usize::from(self.diff_area.width.saturating_sub(
            if self.hunk_mode || self.line_mode {
                4
            } else {
                2
            },
        ));
        if width == 0 || self.diff_lines.is_empty() {
            return 0;
        }
        let widest = self
            .diff_lines
            .iter()
            .map(|line| Line::from(line.text.as_str()).width().saturating_sub(1))
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
                    view.scroll_by(
                        delta as isize * view.viewport_height.saturating_sub(1).max(1) as isize,
                    );
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
        self.pending_fixup = false;
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GraphAction {
    Branch,
    Tag,
    Reset,
    Checkout,
    CheckoutBranch,
    CherryPick,
    Revert,
    RebaseOnto,
    Discard,
    Rename,
    Delete,
    Pull,
    Fetch,
    PushRemote,
    Push,
    Undo,
}

impl GraphAction {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Branch => "Create branch",
            Self::Tag => "Create tag",
            Self::Reset => "Reset",
            Self::Checkout | Self::CheckoutBranch => "Checkout",
            Self::CherryPick => "Cherry-pick",
            Self::Revert => "Revert",
            Self::RebaseOnto => "Rebase",
            Self::Discard => "Discard",
            Self::Rename => "Rename branch",
            Self::Delete => "Delete ref",
            Self::Pull => "Pull",
            Self::Fetch => "Fetch",
            Self::PushRemote | Self::Push => "Push",
            Self::Undo => "Undo",
        }
    }
}

/// A branch or tag at the selected commit that can be deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DeleteTarget {
    Branch(String),
    Tag(String),
}

impl DeleteTarget {
    pub(super) fn name(&self) -> &str {
        match self {
            Self::Branch(name) | Self::Tag(name) => name,
        }
    }

    pub(super) fn is_tag(&self) -> bool {
        matches!(self, Self::Tag(_))
    }
}

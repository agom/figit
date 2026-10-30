use super::*;

impl App {
    pub(super) fn collect_event(
        &mut self,
        event: Event,
        wheel: &mut Option<(u16, u16, i32)>,
    ) -> bool {
        self.redraw = true;
        if let Event::Mouse(mouse) = &event {
            let delta = match mouse.kind {
                MouseEventKind::ScrollUp | MouseEventKind::ScrollLeft => Some(-1),
                MouseEventKind::ScrollDown | MouseEventKind::ScrollRight => Some(1),
                _ => None,
            };
            if let Some(delta) = delta {
                if !self.modal_closed() {
                    *wheel = None;
                    return false;
                }
                match wheel {
                    Some((x, y, total)) if *x == mouse.column && *y == mouse.row => {
                        *total += delta;
                    }
                    Some((x, y, total)) => {
                        *x = mouse.column;
                        *y = mouse.row;
                        *total = delta;
                    }
                    None => *wheel = Some((mouse.column, mouse.row, delta)),
                }
                return false;
            }
        }
        self.handle_event(event)
    }

    pub(super) fn handle_event(&mut self, event: Event) -> bool {
        if !self.modal_closed() {
            return match event {
                Event::Key(key) => self.handle_key(key),
                // Popups are modal: mouse input must not affect panels behind them.
                _ => false,
            };
        }
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Mouse(mouse) => {
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        self.handle_mouse(mouse.column, mouse.row)
                    }
                    MouseEventKind::ScrollUp => {
                        self.handle_mouse_scroll(mouse.column, mouse.row, -1)
                    }
                    MouseEventKind::ScrollDown => {
                        self.handle_mouse_scroll(mouse.column, mouse.row, 1)
                    }
                    MouseEventKind::ScrollLeft => {
                        self.handle_mouse_scroll(mouse.column, mouse.row, -1)
                    }
                    MouseEventKind::ScrollRight => {
                        self.handle_mouse_scroll(mouse.column, mouse.row, 1)
                    }
                    _ => {}
                }
                false
            }
            _ => false,
        }
    }

    /// True when no modal popup is capturing input.
    pub(super) fn modal_closed(&self) -> bool {
        self.graph_action.is_none()
            && !self.commit_dialog.visible()
            && !self.abort_rebase_popup
            && self.rebase_plan.is_none()
            && self.error_popup.is_none()
            && self.stash_popup.is_none()
            && !self.help_popup
            && self.search_popup.is_none()
            && self.command_picker.is_none()
    }

    /// Returns `true` when the app should quit.
    pub(super) fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.local_job.is_some() {
            return self.handle_remote_key(key);
        }
        if self.command_picker.is_some() {
            self.handle_command_picker_key(key);
            return false;
        }
        if key.code == KeyCode::Char(':')
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
            && self.modal_closed()
            && !self.loading
        {
            self.open_command_picker();
            return false;
        }
        if key.code == KeyCode::Char('H')
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
            && self.modal_closed()
        {
            self.panel_hints_visible = !self.panel_hints_visible;
            return false;
        }
        if self.help_popup {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                    self.help_popup = false;
                    self.help_scroll = 0;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.help_scroll = self.help_scroll.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.help_scroll = self.help_scroll.saturating_add(1);
                }
                KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(10),
                KeyCode::PageDown => self.help_scroll = self.help_scroll.saturating_add(10),
                _ => {}
            }
            return false;
        }
        if self.search_popup.is_some() {
            self.handle_search_key(key);
            return false;
        }
        if self.error_popup.is_some() {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    self.error_popup = None;
                    self.error_scroll = 0;
                    self.action_error = None;
                }
                KeyCode::Up => self.error_scroll = self.error_scroll.saturating_sub(1),
                KeyCode::Down => self.error_scroll = self.error_scroll.saturating_add(1),
                KeyCode::PageUp => self.error_scroll = self.error_scroll.saturating_sub(5),
                KeyCode::PageDown => self.error_scroll = self.error_scroll.saturating_add(5),
                _ => {}
            }
            return false;
        }
        if self.loading {
            if !self.rows.is_empty() || !self.local_status.is_empty() {
                return self.handle_remote_key(key);
            }
            return key.code == KeyCode::Char('q');
        }
        if self.git_busy() {
            return self.handle_remote_key(key);
        }
        if key.code == KeyCode::Char('U')
            && self.graph_action.is_none()
            && !self.commit_dialog.visible()
            && !self.abort_rebase_popup
            && self.stash_popup.is_none()
            && self.rebase_plan.is_none()
        {
            match self.update_status.clone() {
                UpdateStatus::Available(_) => self.install_update(),
                UpdateStatus::Checking | UpdateStatus::Installing => {}
                _ => self.start_update_check(true),
            }
            return false;
        }
        if key.code == KeyCode::Char('?')
            && self.graph_action.is_none()
            && !self.commit_dialog.visible()
            && !self.abort_rebase_popup
            && self.stash_popup.is_none()
            && self.rebase_plan.is_none()
        {
            self.help_popup = true;
            return false;
        }
        if self.stash_popup.is_some() {
            return self.handle_stash_key(key);
        }
        if self.rebase_plan.is_some() && !self.commit_dialog.visible() && !self.abort_rebase_popup {
            self.handle_rebase_plan_key(key);
            return false;
        }
        if self.modal_closed() && self.handle_panel_shortcut(key) {
            return false;
        }
        if key.code == KeyCode::Esc && self.modal_closed() {
            if self.pending_fixup {
                self.pending_fixup = false;
                if !self.local_status.is_empty() {
                    self.select_graph_index(0);
                }
                self.focus = Focus::Status;
                self.status_section = StatusSection::Staged;
                self.ensure_status_selection();
                return false;
            }
            if self
                .three_way_view
                .as_ref()
                .is_some_and(|view| view.result_preview)
            {
                self.cancel_conflict_preview();
                return false;
            }
        }
        if key.code == KeyCode::Char('f')
            && matches!(self.focus, Focus::Status | Focus::Diff)
            && (self.hunk_mode || self.line_mode)
        {
            self.execute_panel_command(CommandId::FileMode);
            return false;
        }
        if matches!(self.focus, Focus::Status | Focus::Diff)
            && (key.code == KeyCode::Char('F')
                || (key.code == KeyCode::Char('f') && key.modifiers.contains(KeyModifiers::SHIFT)))
        {
            self.execute_panel_command(CommandId::ApplyFile);
            return false;
        }
        if matches!(key.code, KeyCode::Char('s' | 'S'))
            && self.graph_action.is_none()
            && !self.commit_dialog.visible()
            && !self.abort_rebase_popup
        {
            self.execute_panel_command(CommandId::Stash);
            return false;
        }
        if self.graph_action.is_none() && !self.commit_dialog.visible() && !self.abort_rebase_popup
        {
            match key.code {
                KeyCode::Char('[')
                    if self.three_way_view.is_some() && self.move_conflict_hunk(-1) =>
                {
                    self.focus = Focus::Diff;
                    return false;
                }
                KeyCode::Char(']')
                    if self.three_way_view.is_some() && self.move_conflict_hunk(1) =>
                {
                    self.focus = Focus::Diff;
                    return false;
                }
                KeyCode::Char('[')
                    if self.three_way_view.is_none() && !self.hunk_header_indices.is_empty() =>
                {
                    self.navigate_diff_hunk(-1);
                    self.focus = Focus::Diff;
                    return false;
                }
                KeyCode::Char(']')
                    if self.three_way_view.is_none() && !self.hunk_header_indices.is_empty() =>
                {
                    self.navigate_diff_hunk(1);
                    self.focus = Focus::Diff;
                    return false;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Char('q')
                if self.graph_action.is_none()
                    && !self.commit_dialog.visible()
                    && !self.abort_rebase_popup =>
            {
                return true
            }
            KeyCode::Char('c')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.rebase_active
                    && self.graph_action.is_none()
                    && !self.commit_dialog.visible()
                    && !self.abort_rebase_popup =>
            {
                self.continue_rebase()
            }
            KeyCode::Char('c')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.graph_action.is_none()
                    && !self.commit_dialog.visible()
                    && !self.abort_rebase_popup =>
            {
                return true
            }
            KeyCode::Char('a')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.rebase_active
                    && self.graph_action.is_none()
                    && !self.commit_dialog.visible()
                    && !self.abort_rebase_popup =>
            {
                self.open_abort_rebase_confirmation()
            }
            KeyCode::Char('c')
                if self.rebase_active
                    && self.rebase_edit.is_some()
                    && self.graph_action.is_none()
                    && !self.commit_dialog.visible()
                    && !self.abort_rebase_popup =>
            {
                self.open_rebase_replacement_popup()
            }
            KeyCode::Esc if self.abort_rebase_popup => {
                self.abort_rebase_popup = false;
                self.action_error = None;
                self.focus = Focus::Graph;
            }
            KeyCode::Left
                if matches!(
                    self.graph_action,
                    Some(GraphAction::Branch | GraphAction::Tag | GraphAction::Rename)
                ) && self.commit_field == CommitField::Title =>
            {
                self.graph_action_cursor = self.graph_action_cursor.saturating_sub(1);
            }
            KeyCode::Right
                if matches!(
                    self.graph_action,
                    Some(GraphAction::Branch | GraphAction::Tag | GraphAction::Rename)
                ) && self.commit_field == CommitField::Title =>
            {
                self.graph_action_cursor =
                    (self.graph_action_cursor + 1).min(self.graph_action_input.chars().count());
            }
            KeyCode::Home
                if matches!(
                    self.graph_action,
                    Some(GraphAction::Branch | GraphAction::Tag | GraphAction::Rename)
                ) && self.commit_field == CommitField::Title =>
            {
                self.graph_action_cursor = 0
            }
            KeyCode::End
                if matches!(
                    self.graph_action,
                    Some(GraphAction::Branch | GraphAction::Tag | GraphAction::Rename)
                ) && self.commit_field == CommitField::Title =>
            {
                self.graph_action_cursor = self.graph_action_input.chars().count();
            }
            KeyCode::Up | KeyCode::Down
                if matches!(
                    self.graph_action,
                    Some(GraphAction::Branch | GraphAction::Tag | GraphAction::Rename)
                ) && self.commit_field == CommitField::Title => {}
            KeyCode::Left
                if self.commit_dialog.visible()
                    && !self.commit_dialog.confirming()
                    && self.commit_field == CommitField::Title =>
            {
                self.commit_dialog.title_cursor = self.commit_dialog.title_cursor.saturating_sub(1);
            }
            KeyCode::Right
                if self.commit_dialog.visible()
                    && !self.commit_dialog.confirming()
                    && self.commit_field == CommitField::Title =>
            {
                self.commit_dialog.title_cursor = (self.commit_dialog.title_cursor + 1)
                    .min(self.commit_dialog.title.chars().count());
            }
            KeyCode::Left
                if self.commit_dialog.visible()
                    && !self.commit_dialog.confirming()
                    && self.commit_field == CommitField::Description =>
            {
                self.commit_dialog.description_cursor =
                    self.commit_dialog.description_cursor.saturating_sub(1);
            }
            KeyCode::Right
                if self.commit_dialog.visible()
                    && !self.commit_dialog.confirming()
                    && self.commit_field == CommitField::Description =>
            {
                self.commit_dialog.description_cursor = (self.commit_dialog.description_cursor + 1)
                    .min(self.commit_dialog.description.chars().count());
            }
            KeyCode::Up
                if self.commit_dialog.visible()
                    && !self.commit_dialog.confirming()
                    && self.commit_field == CommitField::Description =>
            {
                move_cursor_vertical(
                    &self.commit_dialog.description,
                    &mut self.commit_dialog.description_cursor,
                    -1,
                );
            }
            KeyCode::Down
                if self.commit_dialog.visible()
                    && !self.commit_dialog.confirming()
                    && self.commit_field == CommitField::Description =>
            {
                move_cursor_vertical(
                    &self.commit_dialog.description,
                    &mut self.commit_dialog.description_cursor,
                    1,
                );
            }
            KeyCode::Up | KeyCode::Down
                if self.commit_dialog.visible()
                    && !self.commit_dialog.confirming()
                    && self.commit_field == CommitField::Title => {}
            KeyCode::Home
                if self.commit_dialog.visible()
                    && !self.commit_dialog.confirming()
                    && self.commit_field == CommitField::Title =>
            {
                self.commit_dialog.title_cursor = 0;
            }
            KeyCode::End
                if self.commit_dialog.visible()
                    && !self.commit_dialog.confirming()
                    && self.commit_field == CommitField::Title =>
            {
                self.commit_dialog.title_cursor = self.commit_dialog.title.chars().count();
            }
            KeyCode::Home
                if self.commit_dialog.visible()
                    && !self.commit_dialog.confirming()
                    && self.commit_field == CommitField::Description =>
            {
                move_cursor_to_line_edge(
                    &self.commit_dialog.description,
                    &mut self.commit_dialog.description_cursor,
                    false,
                );
            }
            KeyCode::End
                if self.commit_dialog.visible()
                    && !self.commit_dialog.confirming()
                    && self.commit_field == CommitField::Description =>
            {
                move_cursor_to_line_edge(
                    &self.commit_dialog.description,
                    &mut self.commit_dialog.description_cursor,
                    true,
                );
            }
            KeyCode::Delete
                if matches!(
                    self.graph_action,
                    Some(GraphAction::Branch | GraphAction::Tag | GraphAction::Rename)
                ) && self.commit_field == CommitField::Title =>
            {
                delete_at_cursor(&mut self.graph_action_input, self.graph_action_cursor);
            }
            KeyCode::Delete
                if self.commit_dialog.visible()
                    && !self.commit_dialog.confirming()
                    && self.commit_field == CommitField::Title =>
            {
                delete_at_cursor(
                    &mut self.commit_dialog.title,
                    self.commit_dialog.title_cursor,
                );
            }
            KeyCode::Delete
                if self.commit_dialog.visible()
                    && !self.commit_dialog.confirming()
                    && self.commit_field == CommitField::Description =>
            {
                delete_at_cursor(
                    &mut self.commit_dialog.description,
                    self.commit_dialog.description_cursor,
                );
            }
            KeyCode::Up
                if matches!(
                    self.graph_action,
                    Some(GraphAction::Fetch | GraphAction::PushRemote)
                ) =>
            {
                self.move_remote_selection(-1);
            }
            KeyCode::Down
                if matches!(
                    self.graph_action,
                    Some(GraphAction::Fetch | GraphAction::PushRemote)
                ) =>
            {
                self.move_remote_selection(1);
            }
            KeyCode::Up if self.graph_action == Some(GraphAction::Undo) => {
                self.move_undo_selection(-1);
            }
            KeyCode::Down if self.graph_action == Some(GraphAction::Undo) => {
                self.move_undo_selection(1);
            }
            KeyCode::Left | KeyCode::Up
                if self.graph_action.is_some()
                    && !matches!(
                        self.graph_action,
                        Some(GraphAction::Branch | GraphAction::Tag | GraphAction::Rename)
                    ) =>
            {
                self.move_graph_popup_option(false)
            }
            KeyCode::Right | KeyCode::Down
                if self.graph_action.is_some()
                    && !matches!(
                        self.graph_action,
                        Some(GraphAction::Branch | GraphAction::Tag | GraphAction::Rename)
                    ) =>
            {
                self.move_graph_popup_option(true)
            }
            KeyCode::BackTab if self.modal_closed() => self.cycle_panel_focus(false),
            KeyCode::Tab => {
                if self.abort_rebase_popup {
                    self.commit_field = if self.commit_field == CommitField::Commit {
                        CommitField::Cancel
                    } else {
                        CommitField::Commit
                    };
                    return false;
                }
                if self.graph_action.is_some() {
                    if self.graph_action == Some(GraphAction::Reset) {
                        self.reset_focus = (self.reset_focus + 1) % 4;
                    } else if self.graph_action == Some(GraphAction::Push) {
                        self.push_focus = (self.push_focus + 1) % 4;
                    } else if matches!(
                        self.graph_action,
                        Some(GraphAction::Branch | GraphAction::Tag | GraphAction::Rename)
                    ) {
                        self.commit_field = match self.commit_field {
                            CommitField::Title => CommitField::Commit,
                            CommitField::Commit => CommitField::Cancel,
                            CommitField::Cancel | CommitField::Description => CommitField::Title,
                        };
                    } else {
                        self.commit_field = if self.commit_field == CommitField::Cancel {
                            CommitField::Commit
                        } else {
                            CommitField::Cancel
                        };
                    }
                    return false;
                }
                if self.focus != Focus::Commit {
                    self.cycle_panel_focus(true);
                    return false;
                }
                self.commit_field = if self.commit_dialog.confirming() {
                    match self.commit_field {
                        CommitField::Commit => CommitField::Cancel,
                        _ => CommitField::Commit,
                    }
                } else {
                    match self.commit_field {
                        CommitField::Title => CommitField::Description,
                        CommitField::Description => CommitField::Commit,
                        CommitField::Commit => CommitField::Cancel,
                        CommitField::Cancel => CommitField::Title,
                    }
                };
            }
            KeyCode::Enter if self.abort_rebase_popup => {
                if self.commit_field == CommitField::Cancel {
                    self.abort_rebase_popup = false;
                    self.action_error = None;
                    self.focus = Focus::Graph;
                } else {
                    self.abort_rebase();
                }
            }
            _ if self.abort_rebase_popup => {}
            KeyCode::Char('b')
                if self.focus == Focus::Graph && self.selected_conflict_path().is_none() =>
            {
                self.execute_panel_command(CommandId::Graph(GraphAction::Branch))
            }
            KeyCode::Char('t')
                if self.focus == Focus::Graph && self.selected_conflict_path().is_none() =>
            {
                self.execute_panel_command(CommandId::Graph(GraphAction::Tag))
            }
            KeyCode::Char('r') if self.focus == Focus::Graph => {
                self.execute_panel_command(CommandId::Graph(GraphAction::Reset))
            }
            KeyCode::Char('c') if self.focus == Focus::Graph => {
                self.execute_panel_command(CommandId::Graph(GraphAction::Checkout))
            }
            KeyCode::Char('w') if self.focus == Focus::Graph => {
                self.execute_panel_command(CommandId::Reword)
            }
            KeyCode::Char('e')
                if self.focus == Focus::Graph && self.selected_conflict_path().is_none() =>
            {
                self.execute_panel_command(CommandId::Edit)
            }
            KeyCode::Char('i') if self.focus == Focus::Graph => {
                self.execute_panel_command(CommandId::RebasePlan)
            }
            KeyCode::Char('R') if self.focus == Focus::Graph => {
                self.execute_panel_command(CommandId::Graph(GraphAction::RebaseOnto))
            }
            KeyCode::Char('o')
                if self.selected_conflict_path().is_some() && self.modal_closed() =>
            {
                self.execute_panel_command(CommandId::Conflict(ConflictSide::Ours));
            }
            KeyCode::Char('t')
                if self.selected_conflict_path().is_some() && self.modal_closed() =>
            {
                self.execute_panel_command(CommandId::Conflict(ConflictSide::Theirs));
            }
            KeyCode::Char('b')
                if self.selected_conflict_path().is_some() && self.modal_closed() =>
            {
                self.execute_panel_command(CommandId::Conflict(ConflictSide::Base));
            }
            KeyCode::Char('e')
                if self.selected_conflict_path().is_some() && self.modal_closed() =>
            {
                self.execute_panel_command(CommandId::Editor);
            }
            KeyCode::Char('w') if self.graph_action.is_none() && !self.commit_dialog.visible() => {
                self.action_error =
                    Some("Focus the Graph with Tab and select a commit first".into());
            }
            KeyCode::Char('p') if self.focus == Focus::Graph && !self.selected_current_branch() => {
                self.execute_panel_command(CommandId::Graph(GraphAction::CherryPick))
            }
            KeyCode::Char('x') if self.focus == Focus::Graph => {
                self.execute_panel_command(CommandId::Graph(GraphAction::Revert))
            }
            KeyCode::Char('u') if self.modal_closed() => {
                self.execute_panel_command(CommandId::Undo)
            }
            KeyCode::Char('/') if self.modal_closed() => {
                self.execute_panel_command(CommandId::Search)
            }
            KeyCode::Char('L') if self.diff_truncated.is_some() && self.modal_closed() => {
                self.execute_panel_command(CommandId::LoadFull)
            }
            KeyCode::Char('x')
                if self.focus == Focus::Diff && (self.hunk_mode || self.line_mode) =>
            {
                self.execute_panel_command(CommandId::DiscardSelection)
            }
            KeyCode::Char('x')
                if self.focus == Focus::Status
                    && self.status_section == StatusSection::Unstaged =>
            {
                self.execute_panel_command(CommandId::Discard)
            }
            KeyCode::Char('n') if self.focus == Focus::Graph => {
                self.execute_panel_command(CommandId::Graph(GraphAction::Rename))
            }
            KeyCode::Char('d') if self.focus == Focus::Graph => {
                self.execute_panel_command(CommandId::Graph(GraphAction::Delete))
            }
            KeyCode::Char('p') if self.focus == Focus::Graph && self.selected_current_branch() => {
                self.execute_panel_command(CommandId::Graph(GraphAction::Pull))
            }
            KeyCode::Char('F') if self.focus == Focus::Graph => {
                self.execute_panel_command(CommandId::Fetch)
            }
            KeyCode::Char('+') if self.focus == Focus::Graph => {
                self.execute_panel_command(CommandId::Push)
            }
            KeyCode::Esc if self.graph_action == Some(GraphAction::Discard) => {
                self.graph_action = None;
                self.graph_action_return_focus = None;
                self.discard_paths.clear();
                self.focus = Focus::Status;
            }
            KeyCode::Esc if self.graph_action.is_some() => {
                if self.graph_action == Some(GraphAction::Push) {
                    self.push_remote = None;
                }
                self.push_target = None;
                self.graph_action = None;
                self.graph_action_return_focus = None;
                self.undo_options.clear();
                self.undo_selection = 0;
                self.remote_choices.clear();
                self.remote_selection = 0;
                self.focus = Focus::Graph;
            }
            KeyCode::Char('r') if self.graph_action == Some(GraphAction::Delete) => {
                self.remote_action = !self.remote_action;
            }
            KeyCode::Char('r')
                if self.graph_action == Some(GraphAction::Rename)
                    && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.remote_action = !self.remote_action;
            }
            KeyCode::Char('p')
                if self.graph_action == Some(GraphAction::Tag)
                    && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.push_after_create = !self.push_after_create;
            }
            KeyCode::Enter if self.graph_action.is_some() => {
                if matches!(
                    self.graph_action,
                    Some(GraphAction::Fetch | GraphAction::PushRemote)
                ) {
                    if let Some(remote) = self.remote_choices.get(self.remote_selection).cloned() {
                        self.remote_choices.clear();
                        self.remote_selection = 0;
                        if self.graph_action == Some(GraphAction::Fetch) {
                            self.start_remote_operation(RemoteOperation::Fetch(remote));
                        } else {
                            self.open_push_mode(remote);
                        }
                    }
                } else if self.graph_action == Some(GraphAction::Discard)
                    && self.commit_field == CommitField::Cancel
                {
                    self.graph_action = None;
                    self.graph_action_return_focus = None;
                    self.discard_paths.clear();
                    self.focus = Focus::Status;
                } else if self.graph_action == Some(GraphAction::Reset) {
                    if self.reset_focus == 3 {
                        self.graph_action = None;
                        self.graph_action_return_focus = None;
                        self.focus = Focus::Graph;
                    } else {
                        self.reset_mode = self.reset_focus;
                        self.run_graph_action();
                    }
                } else if self.graph_action == Some(GraphAction::Push) {
                    if self.push_focus == 3 {
                        self.graph_action = None;
                        self.graph_action_return_focus = None;
                        self.push_remote = None;
                        self.focus = Focus::Graph;
                    } else {
                        self.push_mode = self.push_focus;
                        self.run_graph_action();
                    }
                } else if self.commit_field == CommitField::Cancel {
                    self.graph_action = None;
                    self.graph_action_return_focus = None;
                    self.undo_options.clear();
                    self.undo_selection = 0;
                    self.focus = Focus::Graph;
                } else {
                    self.run_graph_action();
                }
            }
            KeyCode::Enter
                if self
                    .three_way_view
                    .as_ref()
                    .is_some_and(|view| view.result_preview)
                    && self.selected_conflict_path().is_some() =>
            {
                self.execute_panel_command(CommandId::ConfirmConflict);
            }
            KeyCode::Backspace
                if matches!(
                    self.graph_action,
                    Some(GraphAction::Branch | GraphAction::Tag | GraphAction::Rename)
                ) =>
            {
                backspace_at_cursor(&mut self.graph_action_input, &mut self.graph_action_cursor);
            }
            KeyCode::Char(c)
                if matches!(
                    self.graph_action,
                    Some(GraphAction::Branch | GraphAction::Tag | GraphAction::Rename)
                ) && self.commit_field == CommitField::Title
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                insert_at_cursor(
                    &mut self.graph_action_input,
                    &mut self.graph_action_cursor,
                    c,
                );
            }
            KeyCode::Char('s')
                if self.commit_dialog.visible()
                    && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.commit_changes();
            }
            KeyCode::Enter if self.focus == Focus::Graph && self.pending_fixup => {
                let has_local = !self.local_status.is_empty();
                if self.selected_graph >= usize::from(has_local) {
                    let row = self.selected_graph - usize::from(has_local);
                    let target = self.graph.commits[self.rows[row].commit_index].id.clone();
                    self.pending_fixup = false;
                    self.open_fixup_confirmation(target);
                }
            }
            KeyCode::Enter if self.focus == Focus::Graph => {
                self.execute_panel_command(CommandId::Graph(GraphAction::CheckoutBranch));
            }
            KeyCode::F(2) if self.commit_dialog.visible() => {
                self.commit_changes();
            }
            KeyCode::Esc if self.commit_dialog.visible() => {
                let return_to_graph = matches!(
                    self.commit_dialog.mode,
                    CommitMode::Reword(_) | CommitMode::Edit(_) | CommitMode::PlanReword(_)
                );
                self.commit_dialog.set_open(false);
                self.commit_dialog.set_confirming(false);
                self.commit_dialog.mode = CommitMode::Normal;
                self.focus = if return_to_graph {
                    Focus::Graph
                } else {
                    Focus::Status
                };
            }
            KeyCode::Backspace if self.commit_dialog.visible() => match self.commit_field {
                CommitField::Title => backspace_at_cursor(
                    &mut self.commit_dialog.title,
                    &mut self.commit_dialog.title_cursor,
                ),
                CommitField::Description => backspace_at_cursor(
                    &mut self.commit_dialog.description,
                    &mut self.commit_dialog.description_cursor,
                ),
                CommitField::Commit | CommitField::Cancel => {}
            },
            KeyCode::Enter if self.commit_dialog.visible() => match self.commit_field {
                CommitField::Title => self.commit_field = CommitField::Description,
                CommitField::Description => self.commit_field = CommitField::Commit,
                CommitField::Commit => self.commit_changes(),
                CommitField::Cancel => {
                    self.commit_dialog.set_open(false);
                    self.commit_dialog.set_confirming(false);
                    self.focus = Focus::Status;
                }
            },
            KeyCode::Char(c)
                if self.commit_dialog.visible()
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                match self.commit_field {
                    CommitField::Title => insert_at_cursor(
                        &mut self.commit_dialog.title,
                        &mut self.commit_dialog.title_cursor,
                        c,
                    ),
                    CommitField::Description => insert_at_cursor(
                        &mut self.commit_dialog.description,
                        &mut self.commit_dialog.description_cursor,
                        c,
                    ),
                    CommitField::Commit | CommitField::Cancel => {}
                }
            }
            _ if self.graph_action.is_some()
                || self.commit_dialog.visible()
                || self.abort_rebase_popup => {}
            KeyCode::Char(' ') if self.selected_conflict_path().is_some() => {}
            KeyCode::Char(' ') if self.focus == Focus::Diff => {
                self.execute_panel_command(CommandId::ApplySelection);
            }
            KeyCode::Char(' ') if self.focus == Focus::Status => {
                if self.historical_commit.is_some() {
                    return false;
                }
                self.execute_panel_command(CommandId::ApplySelection);
            }
            KeyCode::Char('c')
                if matches!(self.focus, Focus::Status | Focus::Graph)
                    && self.commit_actions_available() =>
            {
                if self.rebase_edit.is_some() {
                    self.open_rebase_replacement_popup();
                } else {
                    self.open_commit_popup(CommitMode::Normal, String::new(), String::new());
                }
            }
            KeyCode::Char('a')
                if matches!(self.focus, Focus::Status | Focus::Graph)
                    && self.commit_actions_available()
                    && self.rebase_edit.is_none() =>
            {
                self.execute_panel_command(CommandId::Amend);
            }
            KeyCode::Char('f')
                if matches!(self.focus, Focus::Status | Focus::Graph)
                    && self.commit_actions_available()
                    && self.rebase_edit.is_none() =>
            {
                self.execute_panel_command(CommandId::Fixup);
            }
            KeyCode::Char('v') if !self.line_mode => {
                self.execute_panel_command(CommandId::DiffMode);
            }
            KeyCode::Char('h')
                if matches!(self.focus, Focus::Status | Focus::Diff)
                    && self.three_way_view.is_none() =>
            {
                self.execute_panel_command(CommandId::HunkMode);
            }
            KeyCode::Char('l')
                if matches!(self.focus, Focus::Status | Focus::Diff)
                    && self.three_way_view.is_none() =>
            {
                self.execute_panel_command(CommandId::LineMode);
            }
            KeyCode::Char('h') | KeyCode::Left
                if self.focus == Focus::Diff && self.three_way_view.is_some() =>
            {
                if let Some(view) = &mut self.three_way_view {
                    view.scroll_x = view.scroll_x.saturating_sub(4);
                }
            }
            KeyCode::Char('l') | KeyCode::Right
                if self.focus == Focus::Diff && self.three_way_view.is_some() =>
            {
                if let Some(view) = &mut self.three_way_view {
                    view.scroll_x = view.scroll_x.saturating_add(4);
                    view.clamp_scroll();
                }
            }
            KeyCode::Char('j') | KeyCode::Down if self.focus == Focus::Status => {
                self.move_status_selection(1, key.modifiers.contains(KeyModifiers::SHIFT));
            }
            KeyCode::Char('k') | KeyCode::Up if self.focus == Focus::Status => {
                self.move_status_selection(-1, key.modifiers.contains(KeyModifiers::SHIFT));
            }
            KeyCode::Char('j') | KeyCode::Down if self.focus == Focus::Diff && self.hunk_mode => {
                self.navigate_diff_hunk(1);
            }
            KeyCode::Char('k') | KeyCode::Up if self.focus == Focus::Diff && self.hunk_mode => {
                self.navigate_diff_hunk(-1);
            }
            KeyCode::Char('j') | KeyCode::Down if self.focus == Focus::Diff && self.line_mode => {
                self.move_diff_line(1);
            }
            KeyCode::Char('k') | KeyCode::Up if self.focus == Focus::Diff && self.line_mode => {
                self.move_diff_line(-1);
            }
            KeyCode::Left
                if matches!(self.focus, Focus::Status | Focus::Diff)
                    && self.three_way_view.is_none() =>
            {
                self.pan_diff_horizontal(-4);
            }
            KeyCode::Right
                if matches!(self.focus, Focus::Status | Focus::Diff)
                    && self.three_way_view.is_none() =>
            {
                self.pan_diff_horizontal(4);
            }
            KeyCode::Char('j') | KeyCode::Down if self.focus == Focus::Diff => {
                if let Some(view) = &mut self.three_way_view {
                    view.scroll_by(1);
                } else {
                    self.diff_scroll = self.diff_scroll.saturating_add(1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up if self.focus == Focus::Diff => {
                if let Some(view) = &mut self.three_way_view {
                    view.scroll_by(-1);
                } else {
                    self.diff_scroll = self.diff_scroll.saturating_sub(1);
                }
            }
            KeyCode::Char('j') | KeyCode::Down if self.focus == Focus::Details => {
                self.details_scroll = self.details_scroll.saturating_add(1);
            }
            KeyCode::Char('k') | KeyCode::Up if self.focus == Focus::Details => {
                self.details_scroll = self.details_scroll.saturating_sub(1);
            }
            KeyCode::Char('j') | KeyCode::Down if self.focus == Focus::Graph => {
                self.select_graph_entry(1);
            }
            KeyCode::Char('k') | KeyCode::Up if self.focus == Focus::Graph => {
                self.select_graph_entry(-1);
            }
            KeyCode::Char('j') | KeyCode::Down => self.scroll_y = self.scroll_y.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_y = self.scroll_y.saturating_sub(1),
            KeyCode::Char('z') if self.modal_closed() => {
                self.execute_panel_command(CommandId::Maximize)
            }
            KeyCode::Char('g') => {
                self.maximized = false;
                self.graph_visible = !self.graph_visible;
                if !self.graph_visible && self.focus == Focus::Graph {
                    self.focus = Focus::Status;
                }
            }
            KeyCode::Char('G') if self.focus == Focus::Graph => {
                let total = self.rows.len() + usize::from(!self.local_status.is_empty());
                if total > 0 {
                    self.select_graph_index(total - 1);
                }
            }
            KeyCode::PageDown => self.page_focus(1),
            KeyCode::PageUp => self.page_focus(-1),
            KeyCode::Char('h') | KeyCode::Left => self.scroll_x = self.scroll_x.saturating_sub(2),
            KeyCode::Char('l') | KeyCode::Right => {
                let max_scroll = self.selected_graph_node_col().unwrap_or(0);
                self.scroll_x = self.scroll_x.saturating_add(2).min(max_scroll);
            }
            _ => {}
        }
        false
    }

    pub(super) fn handle_remote_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') => true,
            KeyCode::Tab | KeyCode::BackTab => {
                self.cycle_panel_focus(key.code == KeyCode::Tab);
                false
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_selection_while_remote(1, key.modifiers.contains(KeyModifiers::SHIFT));
                false
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_selection_while_remote(-1, key.modifiers.contains(KeyModifiers::SHIFT));
                false
            }
            KeyCode::Left
                if matches!(self.focus, Focus::Status | Focus::Diff)
                    && self.three_way_view.is_none() =>
            {
                self.pan_diff_horizontal(-4);
                false
            }
            KeyCode::Right
                if matches!(self.focus, Focus::Status | Focus::Diff)
                    && self.three_way_view.is_none() =>
            {
                self.pan_diff_horizontal(4);
                false
            }
            KeyCode::Char('[') if self.focus == Focus::Diff && self.three_way_view.is_none() => {
                self.navigate_diff_hunk(-1);
                false
            }
            KeyCode::Char(']') if self.focus == Focus::Diff && self.three_way_view.is_none() => {
                self.navigate_diff_hunk(1);
                false
            }
            KeyCode::PageDown if self.focus == Focus::Graph => {
                self.scroll_y = self.scroll_y.saturating_add(20);
                false
            }
            KeyCode::PageUp if self.focus == Focus::Graph => {
                self.scroll_y = self.scroll_y.saturating_sub(20);
                false
            }
            _ => false,
        }
    }

    pub(super) fn move_selection_while_remote(&mut self, delta: i32, extend: bool) {
        match self.focus {
            Focus::Graph => self.select_graph_entry(delta),
            Focus::Status => self.move_status_selection(delta, extend),
            Focus::Diff if self.line_mode => self.move_diff_line(delta),
            Focus::Diff => {
                if delta < 0 {
                    self.diff_scroll = self.diff_scroll.saturating_sub(1);
                } else {
                    self.diff_scroll = self.diff_scroll.saturating_add(1);
                }
            }
            Focus::Details => {
                if delta < 0 {
                    self.details_scroll = self.details_scroll.saturating_sub(1);
                } else {
                    self.details_scroll = self.details_scroll.saturating_add(1);
                }
            }
            Focus::Commit => {}
        }
    }

    pub(super) fn handle_mouse(&mut self, x: u16, y: u16) {
        let position = Position { x, y };
        if self.graph_area.contains(position) {
            self.focus = Focus::Graph;
            let row = y.saturating_sub(self.graph_area.y + 1) as usize;
            let total = self.rows.len() + usize::from(!self.local_status.is_empty());
            let index = self.scroll_y + row;
            if index < total {
                self.select_graph_index(index);
            }
            return;
        }
        if self.historical_commit.is_some() && self.details_area.contains(position) {
            self.focus = Focus::Details;
            return;
        }
        if self.diff_area.contains(position) {
            self.focus = Focus::Diff;
            return;
        }
        if !self.staging_area.contains(position) {
            return;
        }
        let (section, pane, indices) = if self.historical_commit.is_some() {
            (
                StatusSection::Unstaged,
                self.staging_area,
                (0..self.status.len()).collect::<Vec<_>>(),
            )
        } else {
            let panes = self.staging_panes(self.staging_area);
            let (section, pane) = if panes[0].contains(position) {
                (StatusSection::Unstaged, panes[0])
            } else {
                (StatusSection::Staged, panes[1])
            };
            (section, pane, self.section_indices(section).to_vec())
        };
        let inner = pane.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        if !inner.contains(position) {
            return;
        }
        let row = (y - inner.y) as usize + self.status_list_scroll(&indices, pane);
        if let Some(&index) = indices.get(row) {
            self.focus = Focus::Status;
            self.status_section = section;
            self.selected_status = index;
            self.selected_statuses.clear();
            self.selected_statuses.insert(index);
            self.refresh_diff();
        }
    }

    pub(super) fn handle_mouse_scroll(&mut self, x: u16, y: u16, delta: i32) {
        if !self.modal_closed() {
            return;
        }
        let position = Position { x, y };
        if self.diff_area.contains(position) {
            if let Some(view) = &mut self.three_way_view {
                view.scroll_by(delta as isize);
            } else if delta < 0 {
                self.diff_scroll = self.diff_scroll.saturating_sub(1);
            } else {
                self.diff_scroll = self.diff_scroll.saturating_add(1);
            }
        } else if self.historical_commit.is_some() && self.details_area.contains(position) {
            self.focus = Focus::Details;
            if delta < 0 {
                self.details_scroll = self.details_scroll.saturating_sub(1);
            } else {
                self.details_scroll = self.details_scroll.saturating_add(1);
            }
        } else if self.graph_area.contains(position) && self.focus == Focus::Graph {
            if delta < 0 {
                self.scroll_y = self.scroll_y.saturating_sub(3);
            } else {
                self.scroll_y = self.scroll_y.saturating_add(3);
            }
        }
    }
}

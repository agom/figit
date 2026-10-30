use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandId {
    Graph(GraphAction),
    Fetch,
    Push,
    Search,
    Undo,
    Reword,
    Edit,
    RebasePlan,
    Commit,
    Amend,
    Fixup,
    ConfirmFixup,
    Stash,
    ApplySelection,
    ApplyFile,
    Discard,
    DiscardSelection,
    FileMode,
    HunkMode,
    LineMode,
    DiffMode,
    PreviousHunk,
    NextHunk,
    LoadFull,
    Conflict(ConflictSide),
    Editor,
    ConfirmConflict,
    CancelConflict,
    ContinueRebase,
    AbortRebase,
    Maximize,
}

#[derive(Clone, Debug)]
pub(super) struct CommandEntry {
    pub id: CommandId,
    pub label: &'static str,
    pub key: &'static str,
    aliases: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
struct CommandAnchor {
    focus: Focus,
    section: Option<StatusSection>,
    commit: Option<String>,
    head: (Option<String>, Option<String>),
    rebase: (bool, Option<String>),
    files: Vec<GitPath>,
    diff_path: Option<GitPath>,
    diff_target: Option<DiffTarget>,
    hunk: Option<usize>,
    line: Option<usize>,
}

pub(super) struct CommandPickerState {
    pub query: String,
    pub cursor: usize,
    pub selected: usize,
    pub commands: Vec<CommandEntry>,
    anchor: CommandAnchor,
}

impl App {
    /// Availability uses loaded state only. Remote configuration and Git
    /// validation remain in the existing action routines, not in rendering.
    pub(super) fn panel_commands(&self) -> Vec<CommandEntry> {
        use CommandId::*;
        let mut entries = Vec::new();
        let mut add = |id, label, key, aliases| {
            entries.push(CommandEntry {
                id,
                label,
                key,
                aliases,
            })
        };
        let busy = self.loading || self.git_busy();
        let historical = self.historical_commit.is_some()
            && !(self.focus == Focus::Diff && self.three_way_view.is_some());
        let file = self.status.get(self.selected_status).filter(|_| {
            self.section_indices(self.status_section)
                .contains(&self.selected_status)
        });
        let conflict = self.three_way_view.is_some() || file.is_some_and(StatusEntry::is_conflict);
        let hunks = !self.hunk_header_indices.is_empty();
        let changes = self
            .diff_lines
            .iter()
            .any(|line| matches!(line.kind, DiffKind::Added | DiffKind::Removed));
        match self.focus {
            Focus::Graph if !busy => {
                if self.pending_fixup {
                    if self.selected_commit_id().is_some() {
                        add(ConfirmFixup, "Confirm fixup target", "Enter", "squash");
                    }
                } else {
                    add(Fetch, "Fetch", "F", "download|remote");
                    if self.selected_current_branch() {
                        add(Graph(GraphAction::Pull), "Pull", "p", "update branch");
                    }
                    if self.selected_local_branch().is_some()
                        || self.selected_remote_branch().is_some()
                    {
                        add(
                            Graph(GraphAction::CheckoutBranch),
                            "Checkout branch",
                            "Enter",
                            "switch branch",
                        );
                    }
                    if self.selected_commit_id().is_some() && self.rebase_edit.is_none() {
                        add(
                            Graph(GraphAction::Checkout),
                            "Checkout commit",
                            "c",
                            "switch commit|detach",
                        );
                    }
                    add(
                        Search,
                        "Search loaded commit messages",
                        "/",
                        "find|search history",
                    );
                    if self.selected_local_branch().is_some() || self.selected_tag().is_some() {
                        add(Push, "Push", "+", "upload|remote");
                    }
                    if self.selected_commit_id().is_some() {
                        add(
                            Graph(GraphAction::Branch),
                            "Create branch",
                            "b",
                            "new branch",
                        );
                        add(Graph(GraphAction::Tag), "Create tag", "t", "new tag");
                        if self.selected_local_branch().is_some() {
                            add(Graph(GraphAction::Rename), "Rename branch", "n", "");
                        }
                        if !self.deletable_refs().is_empty() {
                            add(
                                Graph(GraphAction::Delete),
                                "Delete branch or tag",
                                "d",
                                "remove ref",
                            );
                        }
                        add(
                            Graph(GraphAction::Reset),
                            "Reset to commit",
                            "r",
                            "reset branch",
                        );
                        add(
                            Graph(GraphAction::Revert),
                            "Revert commit",
                            "x",
                            "undo commit",
                        );
                        if !self.selected_current_branch() {
                            add(
                                Graph(GraphAction::CherryPick),
                                "Cherry-pick commit",
                                "p",
                                "pick|apply commit",
                            );
                        }
                        if self.selected_rebase_commit().is_some() {
                            add(Reword, "Reword commit", "w", "rename commit|message");
                            add(Edit, "Edit commit", "e", "split commit");
                            add(RebasePlan, "Interactive rebase", "i", "reorder|squash");
                        }
                        add(
                            Graph(GraphAction::RebaseOnto),
                            "Rebase onto commit",
                            "R",
                            "move branch",
                        );
                    }
                    if self.commit_actions_available() && self.rebase_edit.is_none() {
                        add(Amend, "Amend commit", "a", "");
                        add(Fixup, "Create fixup commit", "f", "squash");
                    }
                    add(Undo, "Undo from reflog", "u", "restore history");
                }
            }
            Focus::Status | Focus::Diff
                if !busy
                    && !(self.focus == Focus::Diff
                        && self.three_way_view.is_none()
                        && (self.details_loading || self.diff_path.is_none())) =>
            {
                if conflict && !historical {
                    add(
                        Conflict(ConflictSide::Ours),
                        if self
                            .three_way_view
                            .as_ref()
                            .is_some_and(|view| view.resolves_hunks)
                        {
                            "Preview ours for this conflict"
                        } else {
                            "Preview ours (whole file)"
                        },
                        "o",
                        "resolve conflict",
                    );
                    add(
                        Conflict(ConflictSide::Theirs),
                        if self
                            .three_way_view
                            .as_ref()
                            .is_some_and(|view| view.resolves_hunks)
                        {
                            "Preview theirs for this conflict"
                        } else {
                            "Preview theirs (whole file)"
                        },
                        "t",
                        "resolve conflict",
                    );
                    add(
                        Conflict(ConflictSide::Base),
                        "Restore entire file from Base",
                        "b",
                        "restore base",
                    );
                    add(
                        Editor,
                        "Open conflict editor",
                        "e",
                        "resolve|external editor",
                    );
                    if let Some(view) = &self.three_way_view {
                        if !view.hunks.is_empty() {
                            add(PreviousHunk, "Previous conflict hunk", "[", "");
                            add(NextHunk, "Next conflict hunk", "]", "");
                        }
                        if view.result_preview {
                            add(
                                ConfirmConflict,
                                "Confirm conflict result",
                                "Enter",
                                "resolve",
                            );
                            add(CancelConflict, "Cancel conflict preview", "Esc", "");
                        }
                    }
                } else {
                    if !historical && file.is_some() {
                        let staged = self.status_section == StatusSection::Staged;
                        let label = match (staged, self.hunk_mode, self.line_mode) {
                            (true, _, true) => "Unstage selected line",
                            (false, _, true) => "Stage selected line",
                            (true, true, _) => "Unstage selected hunk",
                            (false, true, _) => "Stage selected hunk",
                            (true, _, _) => "Unstage selected files",
                            _ => "Stage selected files",
                        };
                        if (!self.hunk_mode && !self.line_mode)
                            || (self.hunk_mode && hunks)
                            || (self.line_mode && self.selected_diff_line.is_some())
                        {
                            add(ApplySelection, label, "Space", "index|selection");
                        }
                        if self.hunk_mode || self.line_mode {
                            add(
                                ApplyFile,
                                if staged {
                                    "Unstage whole files"
                                } else {
                                    "Stage whole files"
                                },
                                "F",
                                "entire file",
                            );
                        }
                        if self.focus == Focus::Status
                            && !staged
                            && !self.selected_statuses.is_empty()
                        {
                            add(Discard, "Discard selected files", "x", "reset worktree");
                        }
                    }
                    if !historical
                        && file.is_some()
                        && self.diff_path.is_some()
                        && !self.details_loading
                    {
                        if self.hunk_mode || self.line_mode {
                            add(FileMode, "Use file selection mode", "f", "whole file");
                        }
                        if hunks {
                            add(
                                HunkMode,
                                if self.hunk_mode {
                                    "Leave hunk selection mode"
                                } else {
                                    "Use hunk selection mode"
                                },
                                "h",
                                "partial stage",
                            );
                        }
                        if changes {
                            add(
                                LineMode,
                                if self.line_mode {
                                    "Leave line selection mode"
                                } else {
                                    "Use line selection mode"
                                },
                                "l",
                                "partial stage",
                            );
                        }
                    }
                    if self.focus == Focus::Diff
                        && self.diff_path.is_some()
                        && !self.details_loading
                    {
                        if !historical
                            && self.diff_target == DiffTarget::Worktree
                            && (self.hunk_mode || self.line_mode)
                        {
                            add(
                                DiscardSelection,
                                "Discard selected change",
                                "x",
                                "revert|worktree",
                            );
                        }
                        if hunks && !self.line_mode {
                            if self.hunk_mode {
                                add(PreviousHunk, "Previous hunk", "k", "up|j/k");
                                add(NextHunk, "Next hunk", "j", "down|j/k");
                            } else {
                                add(PreviousHunk, "Previous hunk", "[", "up");
                                add(NextHunk, "Next hunk", "]", "down");
                            }
                        }
                        if !self.line_mode {
                            add(DiffMode, "Change diff view", "v", "unified|side by side");
                        }
                        if self.diff_truncated.is_some() {
                            add(LoadFull, "Load full diff", "L", "truncated|all lines");
                        }
                    }
                }
                if !historical {
                    if self.focus == Focus::Status {
                        if self.commit_actions_available() {
                            add(
                                Commit,
                                if self.rebase_edit.is_some() {
                                    "Replace edited commit"
                                } else {
                                    "Commit staged changes"
                                },
                                "c",
                                "save commit",
                            );
                            if self.rebase_edit.is_none() {
                                add(Amend, "Amend commit", "a", "");
                                // In partial-selection modes, f returns to
                                // file mode. Fixup remains available by name.
                                add(
                                    Fixup,
                                    "Create fixup commit",
                                    if self.hunk_mode || self.line_mode {
                                        ""
                                    } else {
                                        "f"
                                    },
                                    "squash",
                                );
                            }
                        }
                        if !self.rebase_active && !conflict {
                            add(Stash, "Manage stashes", "s", "save worktree");
                        }
                    }
                    if self.rebase_active {
                        if self.rebase_edit.is_none() {
                            add(ContinueRebase, "Continue rebase", "Ctrl+C", "resume");
                        }
                        add(AbortRebase, "Abort rebase", "Ctrl+A", "cancel rebase");
                    }
                }
            }
            _ => {}
        }
        if busy
            && !self.loading
            && self.focus == Focus::Diff
            && self.three_way_view.is_none()
            && hunks
        {
            add(PreviousHunk, "Previous hunk", "[", "");
            add(NextHunk, "Next hunk", "]", "");
        }
        // z is disabled during loading/remote operations in the key handler.
        if !busy {
            add(
                Maximize,
                if self.maximized {
                    "Restore panel"
                } else {
                    "Maximize panel"
                },
                "z",
                "zoom|expand",
            );
        }
        entries
    }

    /// The same entries drive the picker, panel legends and keyboard dispatch.
    pub(super) fn handle_panel_shortcut(&mut self, key: KeyEvent) -> bool {
        let command = self.panel_commands().into_iter().find(|command| {
            let control = key.modifiers.contains(KeyModifiers::CONTROL);
            if key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
            {
                return false;
            }
            match command.key {
                "Ctrl+C" => control && key.code == KeyCode::Char('c'),
                "Ctrl+A" => control && key.code == KeyCode::Char('a'),
                _ if control => false,
                "Enter" => key.code == KeyCode::Enter,
                "Esc" => key.code == KeyCode::Esc,
                "Space" => key.code == KeyCode::Char(' '),
                value => {
                    value.chars().count() == 1
                        && key.code == KeyCode::Char(value.chars().next().unwrap())
                }
            }
        });
        if let Some(command) = command {
            self.execute_panel_command(command.id);
            true
        } else {
            false
        }
    }

    pub(super) fn execute_panel_command(&mut self, id: CommandId) {
        use CommandId::*;
        match id {
            Graph(GraphAction::CheckoutBranch | GraphAction::Rename) => {
                if let Graph(action) = id {
                    self.open_branch_ref_action(action);
                }
            }
            Graph(GraphAction::Delete) => self.open_delete_action(),
            Graph(action) => self.open_graph_action(action),
            Fetch => self.open_remote_action(false),
            Push => self.open_remote_action(true),
            Search => self.open_search_popup(),
            Undo => self.open_undo_popup(),
            Reword => self.open_reword_popup(),
            Edit => self.open_edit_confirmation(),
            RebasePlan => self.open_rebase_plan(),
            Commit => {
                if self.rebase_edit.is_some() {
                    self.open_rebase_replacement_popup();
                } else {
                    self.open_commit_popup(CommitMode::Normal, String::new(), String::new());
                }
            }
            Amend => self.open_amend_popup(),
            Fixup => {
                self.graph_visible = true;
                self.maximized = false;
                self.pending_fixup = true;
                self.fixup_origin = self.selected_graph;
                self.focus = Focus::Graph;
            }
            ConfirmFixup => {
                if let Some(target) = self.selected_commit_id() {
                    self.pending_fixup = false;
                    self.open_fixup_confirmation(target);
                }
            }
            Stash => self.open_stash_manager(),
            ApplySelection => {
                if self.line_mode {
                    self.apply_selected_diff_line();
                } else if self.hunk_mode {
                    self.apply_selected_diff_hunk();
                } else {
                    self.apply_selected_file_changes();
                }
            }
            ApplyFile => self.apply_selected_file_changes(),
            Discard => self.open_discard_popup(),
            DiscardSelection => self.discard_selected_diff_change(),
            FileMode => self.select_file_mode(),
            HunkMode => self.toggle_hunk_mode(),
            LineMode => self.toggle_line_mode(),
            DiffMode => {
                self.diff_mode = match self.diff_mode {
                    super::DiffMode::Unified => super::DiffMode::SideBySide,
                    super::DiffMode::SideBySide => super::DiffMode::Unified,
                };
                self.diff_scroll = 0;
                self.diff_scroll_x = 0;
            }
            PreviousHunk | NextHunk => {
                let delta = if id == PreviousHunk { -1 } else { 1 };
                if self.three_way_view.is_some() {
                    self.move_conflict_hunk(delta);
                } else {
                    self.navigate_diff_hunk(delta);
                }
                self.focus = Focus::Diff;
            }
            LoadFull => self.load_full_diff(),
            Conflict(side) => self.choose_selected_conflict_side(side),
            Editor => self.open_selected_conflict_editor(),
            ConfirmConflict => self.confirm_selected_conflict_result(),
            CancelConflict => self.cancel_conflict_preview(),
            ContinueRebase => self.continue_rebase(),
            AbortRebase => self.open_abort_rebase_confirmation(),
            Maximize => self.maximized = !self.maximized,
        }
    }

    fn command_anchor(&self) -> CommandAnchor {
        let files_panel = matches!(self.focus, Focus::Status | Focus::Diff);
        let resolver = (self.focus == Focus::Diff)
            .then_some(self.three_way_view.as_ref())
            .flatten();
        let mut files = if let Some(view) = resolver {
            vec![view.path.clone()]
        } else if files_panel {
            self.selected_statuses
                .iter()
                .chain(std::iter::once(&self.selected_status))
                .filter_map(|&index| self.status.get(index).map(|entry| entry.path.clone()))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        files.sort();
        files.dedup();
        CommandAnchor {
            focus: self.focus,
            section: files_panel.then_some(self.status_section),
            commit: if resolver.is_some() {
                None
            } else if files_panel {
                self.historical_commit.clone()
            } else {
                self.selected_commit_id()
            },
            head: (self.graph.head_id.clone(), self.graph.head_name.clone()),
            rebase: (
                self.rebase_active,
                self.rebase_edit.as_ref().map(|edit| edit.target.clone()),
            ),
            files,
            diff_path: if let Some(view) = resolver {
                Some(view.path.clone())
            } else {
                (self.focus == Focus::Diff)
                    .then(|| self.diff_path.clone())
                    .flatten()
            },
            diff_target: (self.focus == Focus::Diff).then_some(if resolver.is_some() {
                DiffTarget::Worktree
            } else {
                self.diff_target
            }),
            hunk: if let Some(view) = resolver {
                Some(view.hunk_index)
            } else {
                (files_panel && self.hunk_mode).then_some(self.selected_diff_hunk)
            },
            line: if files_panel && self.line_mode {
                self.selected_diff_line
            } else {
                None
            },
        }
    }

    pub(super) fn command_picker_area(&self, count: usize) -> Option<Rect> {
        let area = match self.focus {
            Focus::Graph => self.graph_area,
            Focus::Details => self.details_area,
            Focus::Diff => self.diff_area,
            Focus::Status if self.historical_commit.is_some() => self.staging_area,
            Focus::Status => self.staging_panes(self.staging_area)
                [usize::from(self.status_section == StatusSection::Staged)],
            Focus::Commit => return None,
        };
        let bottom = area.bottom().saturating_sub(1);
        let height = (count.clamp(1, 10) as u16 + 3).min(bottom.saturating_sub(1));
        let width = area.width.saturating_sub(2);
        (height >= 4 && width >= 12).then(|| Rect::new(area.x + 1, bottom - height, width, height))
    }

    pub(super) fn open_command_picker(&mut self) {
        let commands = self.panel_commands();
        if self.command_picker_area(commands.len()).is_none() {
            self.action_error = Some("Panel too small for commands; maximize it with z".into());
            return;
        }
        self.command_picker = Some(CommandPickerState {
            query: String::new(),
            cursor: 0,
            selected: 0,
            commands,
            anchor: self.command_anchor(),
        });
    }

    pub(super) fn handle_command_picker_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.command_picker = None;
            return;
        }
        if key.code == KeyCode::Enter {
            let Some(picker) = self.command_picker.take() else {
                return;
            };
            let matches = filtered_commands(&picker.commands, &picker.query);
            let Some(&index) = matches.get(picker.selected) else {
                self.command_picker = Some(picker);
                return;
            };
            let id = picker.commands[index].id;
            if picker.anchor != self.command_anchor()
                || !self.panel_commands().iter().any(|entry| entry.id == id)
            {
                self.action_error = Some("Selection changed; reopen commands".into());
                return;
            }
            self.execute_panel_command(id);
            return;
        }
        let Some(picker) = self.command_picker.as_mut() else {
            return;
        };
        let old_query = picker.query.clone();
        let count = filtered_commands(&picker.commands, &picker.query).len();
        match key.code {
            KeyCode::Up if count > 0 => picker.selected = (picker.selected + count - 1) % count,
            KeyCode::Down if count > 0 => picker.selected = (picker.selected + 1) % count,
            KeyCode::Left => picker.cursor = picker.cursor.saturating_sub(1),
            KeyCode::Right => picker.cursor = (picker.cursor + 1).min(picker.query.chars().count()),
            KeyCode::Home => picker.cursor = 0,
            KeyCode::End => picker.cursor = picker.query.chars().count(),
            KeyCode::Backspace => backspace_at_cursor(&mut picker.query, &mut picker.cursor),
            KeyCode::Delete => delete_at_cursor(&mut picker.query, picker.cursor),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                picker.query.clear();
                picker.cursor = 0;
            }
            KeyCode::Char(c)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                insert_at_cursor(&mut picker.query, &mut picker.cursor, c)
            }
            _ => {}
        }
        if old_query != picker.query {
            picker.selected = 0;
        }
    }

    pub(super) fn render_command_picker(&self, frame: &mut Frame) {
        let Some(picker) = &self.command_picker else {
            return;
        };
        let Some(area) = self.command_picker_area(picker.commands.len()) else {
            return;
        };
        let context = match self.focus {
            Focus::Graph => "Graph",
            Focus::Details => "Commit Details",
            Focus::Diff if self.three_way_view.is_some() => "Conflict resolver",
            Focus::Diff => "Diff",
            Focus::Status if self.historical_commit.is_some() => "Changes",
            Focus::Status if self.status_section == StatusSection::Staged => "Staged",
            _ => "Unstaged",
        };
        let matches = filtered_commands(&picker.commands, &picker.query);
        let count = format!(
            " {}/{} ",
            if matches.is_empty() {
                0
            } else {
                picker.selected + 1
            },
            matches.len()
        );
        let title_width = area.width.saturating_sub(2) as usize;
        let show_count = title_width >= count.len() + 12;
        let mut block = Block::default().borders(Borders::ALL).title(truncate(
            &format!("Commands — {context}"),
            title_width.saturating_sub(if show_count { count.len() + 1 } else { 0 }),
        ));
        if show_count {
            block = block.title(Line::from(count).right_aligned());
        }
        frame.render_widget(Clear, area);
        frame.render_widget(
            block
                .title_bottom(styled_key_hints_fitting(
                    &[("Enter", "run"), ("Esc", "close")],
                    Color::LightCyan,
                    area.width.saturating_sub(2) as usize,
                ))
                .border_style(Style::default().fg(Color::LightCyan)),
            area,
        );
        let inner = area.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        let list_area = Rect::new(inner.x, inner.y, inner.width, inner.height - 1);
        let offset = picker
            .selected
            .saturating_sub(list_area.height.saturating_sub(1) as usize);
        let width = inner.width as usize;
        let mut lines = Vec::new();
        if matches.is_empty() {
            lines.push(Line::from(Span::styled(
                "No matching commands",
                Style::default().fg(Color::DarkGray),
            )));
        }
        for (rank, &index) in matches
            .iter()
            .enumerate()
            .skip(offset)
            .take(list_area.height as usize)
        {
            let command = &picker.commands[index];
            let selected = rank == picker.selected;
            let style = if selected {
                self.selection_style(true)
            } else {
                Style::default()
            };
            let key_width = Line::from(command.key).width();
            let show_key = width >= key_width + 12;
            let label_width = width.saturating_sub(if show_key { key_width + 2 } else { 0 });
            let label = truncate(command.label, label_width);
            let positions = fuzzy_match(&picker.query, command.label)
                .map_or_else(Vec::new, |(_, positions)| positions);
            let mut line = Line::default().style(style);
            for (position, character) in label.chars().enumerate() {
                let mut text_style = if selected {
                    self.selection_text_style(true)
                } else {
                    Style::default()
                };
                if positions.contains(&position) {
                    text_style = text_style.fg(Color::LightCyan).add_modifier(Modifier::BOLD);
                }
                line.push_span(Span::styled(character.to_string(), text_style));
            }
            if show_key {
                line.push_span(Span::raw(
                    " ".repeat(width.saturating_sub(line.width() + key_width)),
                ));
                line.push_span(Span::styled(
                    command.key,
                    Style::default().fg(Color::DarkGray),
                ));
            }
            pad_colored_lines(std::slice::from_mut(&mut line), width);
            lines.push(line);
        }
        frame.render_widget(Paragraph::new(lines), list_area);
        let input = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
        let (cursor, scroll) = single_line_cursor(&picker.query, 2, picker.cursor, input);
        frame.render_widget(
            Paragraph::new(format!("> {}", picker.query))
                .scroll((0, scroll))
                .style(Style::default().fg(Color::LightCyan)),
            input,
        );
        frame.set_cursor_position(cursor);
    }
}

type FuzzyScore = (bool, bool, usize, usize, usize);

/// Ordered subsequence matching. Try every possible first character so a later
/// contiguous word can outrank a scattered match at the start of the label.
fn fuzzy_match(query: &str, candidate: &str) -> Option<(FuzzyScore, Vec<usize>)> {
    let query: Vec<char> = query
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|c| !c.is_whitespace())
        .collect();
    if query.is_empty() {
        return Some(((false, false, 0, 0, 0), Vec::new()));
    }
    let text: Vec<char> = candidate.chars().flat_map(char::to_lowercase).collect();
    let mut best = None;
    for start in 0..text.len() {
        if text[start] != query[0] {
            continue;
        }
        let mut positions = vec![start];
        let mut next = start + 1;
        for character in &query[1..] {
            let Some(offset) = text[next..].iter().position(|c| c == character) else {
                break;
            };
            next += offset;
            positions.push(next);
            next += 1;
        }
        if positions.len() != query.len() {
            continue;
        }
        let mut run = 1;
        let mut longest = 1;
        for pair in positions.windows(2) {
            run = if pair[1] == pair[0] + 1 { run + 1 } else { 1 };
            longest = longest.max(run);
        }
        let boundaries = positions
            .iter()
            .filter(|&&p| p == 0 || !text[p - 1].is_alphanumeric())
            .count();
        let gaps = positions.last().unwrap() - start + 1 - query.len();
        let normalized: Vec<char> = text
            .iter()
            .copied()
            .filter(|c| !c.is_whitespace())
            .collect();
        let score = (
            normalized == query,
            start == 0 && normalized.starts_with(&query),
            longest,
            boundaries,
            usize::MAX - gaps,
        );
        if best.as_ref().is_none_or(|(old, _)| score > *old) {
            best = Some((score, positions));
        }
    }
    best
}

pub(super) fn filtered_commands(commands: &[CommandEntry], query: &str) -> Vec<usize> {
    let mut matches = commands
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let score = std::iter::once(entry.label)
                .chain(entry.aliases.split('|').filter(|alias| !alias.is_empty()))
                .filter_map(|candidate| fuzzy_match(query, candidate).map(|(score, _)| score))
                .max()?;
            Some((index, score))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|(a, score_a), (b, score_b)| score_b.cmp(score_a).then(a.cmp(b)));
    matches.into_iter().map(|(index, _)| index).collect()
}

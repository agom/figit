use super::*;

impl App {
    pub(super) fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        self.viewport_width = area.width;
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);
        let body_area = chunks[1];
        let hidden = Rect::default();
        let (graph_area, details_area, staging_area, diff_area) = if self.maximized {
            match self.focus {
                Focus::Graph => (body_area, hidden, hidden, hidden),
                Focus::Details => (hidden, body_area, hidden, hidden),
                Focus::Status => (hidden, hidden, body_area, hidden),
                Focus::Diff | Focus::Commit => (hidden, hidden, hidden, body_area),
            }
        } else if !self.graph_visible {
            // Keep the diff boundary stable as files move between the lists.
            let top_height = (body_area.height / 3).max(5).min(body_area.height / 2);
            let body = Layout::vertical([Constraint::Length(top_height), Constraint::Min(0)])
                .split(body_area);
            if self.historical_commit.is_some() && self.three_way_view.is_none() {
                let top = Layout::horizontal([Constraint::Percentage(50), Constraint::Min(0)])
                    .split(body[0]);
                (hidden, top[1], top[0], body[1])
            } else {
                (hidden, hidden, body[0], body[1])
            }
        } else if self.three_way_view.is_some() {
            let vertical =
                Layout::vertical([Constraint::Percentage(40), Constraint::Min(0)]).split(body_area);
            let upper = Layout::horizontal([Constraint::Percentage(45), Constraint::Min(0)])
                .split(vertical[0]);
            (upper[0], hidden, upper[1], vertical[1])
        } else {
            let body = Layout::horizontal([Constraint::Percentage(45), Constraint::Min(0)])
                .split(body_area);
            let right =
                Layout::vertical([Constraint::Percentage(45), Constraint::Min(0)]).split(body[1]);
            let left = if self.historical_commit.is_some() {
                Layout::vertical([Constraint::Percentage(80), Constraint::Min(0)]).split(body[0])
            } else {
                Layout::vertical([Constraint::Min(0), Constraint::Length(0)]).split(body[0])
            };
            (left[0], left[1], right[0], right[1])
        };
        let status_area = chunks[2];
        f.render_widget(
            Paragraph::new(self.repository_header(area.width as usize)),
            chunks[0],
        );

        self.staging_area = staging_area;
        self.graph_area = graph_area;
        self.details_area = details_area;
        self.diff_area = diff_area;
        self.render_staging(f, staging_area);

        if !graph_area.is_empty() {
            let graph_focused = self.focus == Focus::Graph;
            let height = graph_area.height.saturating_sub(2) as usize;
            self.clamp_scroll(height);
            if self.scroll_to_selected_graph {
                let viewport = height.max(1);
                if self.selected_graph < self.scroll_y {
                    self.scroll_y = self.selected_graph;
                } else if self.selected_graph >= self.scroll_y + viewport {
                    self.scroll_y = self.selected_graph + 1 - viewport;
                }
                self.scroll_to_selected_graph = false;
            }
            let width = graph_area.width.saturating_sub(2) as usize;

            let mut lines: Vec<Line> = Vec::with_capacity(height);
            let has_local = !self.local_status.is_empty();
            if has_local && self.scroll_y == 0 && lines.len() < height {
                let mut local_line = Line::from(Span::styled(
                    format!("✎ Local changes ({})", self.local_status.len()),
                    Style::default()
                        .fg(Color::LightYellow)
                        .patch(if self.selected_graph == 0 {
                            self.selection_text_style(graph_focused)
                        } else {
                            Style::default()
                        }),
                ));
                pad_colored_lines(std::slice::from_mut(&mut local_line), width);
                if self.selected_graph == 0 {
                    self.style_graph_selection(&mut local_line, graph_focused);
                }
                lines.push(local_line);
            }
            if self.loading && self.rows.is_empty() && lines.is_empty() && height > 0 {
                let mut loading_line = Line::from(Span::styled(
                    "Loading commit graph…",
                    Style::default().fg(Color::LightYellow),
                ));
                pad_colored_lines(std::slice::from_mut(&mut loading_line), width);
                lines.push(loading_line);
            }
            let skip = self.scroll_y.saturating_sub(usize::from(has_local));
            for (offset, row) in self
                .rows
                .iter()
                .skip(skip)
                .take(height.saturating_sub(lines.len()))
                .enumerate()
            {
                let graph_index = skip + offset + usize::from(has_local);
                let mut line = self.render_row(row, width);
                let commit_id = &self.graph.commits[row.commit_index].id;
                if !self.current_branch_ids.contains(commit_id)
                    || (self.graph_action == Some(GraphAction::Reset)
                        && graph_index < self.selected_graph)
                {
                    let visible_lanes = row
                        .cells
                        .len()
                        .min(self.max_lanes)
                        .saturating_sub(self.scroll_x);
                    let lane_columns = visible_lanes.saturating_mul(2).saturating_sub(1);
                    dim_graph_line(&mut line, 0, lane_columns, self.bg, &self.terminal_palette);
                }
                if let (Some(color), Some(sha)) =
                    (self.conflict_commit_color(commit_id), line.spans.last_mut())
                {
                    sha.style = sha.style.fg(color);
                }
                pad_colored_lines(std::slice::from_mut(&mut line), width);
                if self.selected_graph == graph_index {
                    self.style_graph_selection(&mut line, graph_focused);
                }
                lines.push(line);
            }
            f.render_widget(
                Paragraph::new(lines).block(self.panel_footer(
                    self.panel_block("Graph", graph_focused),
                    graph_area,
                    graph_focused,
                    None,
                )),
                graph_area,
            );
            let total_graph = self.rows.len() + usize::from(has_local);
            if total_graph > height {
                let mut state = ScrollbarState::new(total_graph)
                    .position(scrollbar_position(self.scroll_y, total_graph, height))
                    .viewport_content_length(height);
                f.render_stateful_widget(
                    minimal_scrollbar(if self.focus == Focus::Graph {
                        Color::LightCyan
                    } else {
                        Color::DarkGray
                    }),
                    border_scrollbar_area(Rect::new(
                        graph_area.x,
                        graph_area.y,
                        graph_area.width,
                        height as u16 + 2,
                    )),
                    &mut state,
                );
            }
        }
        if !details_area.is_empty() {
            self.render_commit_details(f, details_area);
        }
        if diff_area.is_empty() {
            // A maximized neighboring pane owns the body.
        } else if self.three_way_view.is_some() {
            self.render_three_way_view(f, diff_area);
        } else {
            let viewport = diff_area.height.saturating_sub(2) as usize;
            let width = diff_area.width.saturating_sub(2) as usize;
            let path = self
                .diff_path
                .as_ref()
                .map(GitPath::display)
                .unwrap_or(Cow::Borrowed(""));
            let rendered_len;
            let diff_widget = if self.diff_lines.is_empty() {
                rendered_len = 0;
                Paragraph::new(if self.details_loading {
                    "Loading commit diff…"
                } else if self.diff_path.is_some() {
                    "No text changes to show for this file"
                } else if self.historical_commit.is_some() {
                    "This commit has no file changes"
                } else if self.local_status.is_empty() {
                    if self.graph_visible {
                        "Working tree clean\nSelect a commit in the graph to inspect its changes."
                    } else {
                        "Working tree clean\nPress g to show history and select a commit."
                    }
                } else {
                    "Select a file above to view its changes"
                })
            } else if let Some((ours, theirs)) = self.selected_conflict_colors() {
                let rendered = render_conflict_diff(
                    &self.diff_lines,
                    path.as_ref(),
                    self.diff_number_width,
                    ours,
                    theirs,
                    true,
                );
                rendered_len = rendered.len();
                let max_scroll = rendered_len.saturating_sub(viewport);
                self.diff_scroll = self
                    .diff_scroll
                    .min(max_scroll.min(u16::MAX as usize) as u16);
                let start = self.diff_scroll as usize;
                let end = (start + viewport).min(rendered_len);
                Paragraph::new(Text::from(&rendered[start..end]))
            } else {
                // Only the visible rows are materialized, so syntax
                // highlighting stays cheap even for very large diffs.
                self.ensure_diff_layout();
                let total = self.diff_row_count();
                rendered_len = total;
                let max_scroll = total.saturating_sub(viewport);
                self.diff_scroll = self
                    .diff_scroll
                    .min(max_scroll.min(u16::MAX as usize) as u16);
                let start = self.diff_scroll as usize;
                let end = (start + viewport).min(total);
                let rows = &self.diff_cache.as_ref().unwrap().1;
                let lines: Vec<Line> = rows[start..end]
                    .iter()
                    .map(|row| self.materialize_diff_row(*row, width))
                    .collect();
                Paragraph::new(Text::from(lines))
            };
            let diff_title = match (&self.diff_path, self.diff_truncated) {
                (Some(path), Some((shown, total))) => {
                    let notice = format!(" · showing {shown} of {total} lines (L loads all)");
                    let available = usize::from(diff_area.width.saturating_sub(2))
                        .saturating_sub("Diff — ".chars().count() + notice.chars().count());
                    format!("Diff — {}{notice}", truncate(&path.display(), available))
                }
                (Some(path), None) => {
                    let available = usize::from(diff_area.width.saturating_sub(2))
                        .saturating_sub("Diff — ".chars().count());
                    format!("Diff — {}", truncate(&path.display(), available))
                }
                (None, _) => "Diff".to_string(),
            };
            let mut diff_block = self.panel_block(&diff_title, self.focus == Focus::Diff);
            let metadata = if self.diff_path.is_some() {
                let scope = if self.historical_commit.is_some() {
                    "read-only"
                } else if self.diff_target == DiffTarget::Index {
                    "staged"
                } else {
                    "unstaged"
                };
                let mode = if self.line_mode {
                    "line selection"
                } else if self.hunk_mode {
                    "hunk selection"
                } else {
                    match self.diff_mode {
                        DiffMode::Unified => "unified",
                        DiffMode::SideBySide => "side-by-side",
                    }
                };
                Some(format!("{scope} · {mode}"))
            } else {
                None
            };
            diff_block = self.panel_footer(
                diff_block,
                diff_area,
                self.focus == Focus::Diff,
                metadata.as_deref(),
            );
            f.render_widget(diff_widget.block(diff_block), diff_area);
            if rendered_len > viewport {
                let mut scrollbar_state = ScrollbarState::new(rendered_len)
                    .position(scrollbar_position(
                        self.diff_scroll as usize,
                        rendered_len,
                        viewport,
                    ))
                    .viewport_content_length(viewport);
                f.render_stateful_widget(
                    minimal_scrollbar(if self.focus == Focus::Diff {
                        Color::LightCyan
                    } else {
                        Color::DarkGray
                    }),
                    border_scrollbar_area(diff_area),
                    &mut scrollbar_state,
                );
            }
        }
        f.render_widget(Paragraph::new(self.status_line()), status_area);
        if let Some(search) = self.search_popup.as_ref().filter(|search| !search.searched) {
            let last_column = status_area.x + status_area.width.saturating_sub(1);
            let column = status_area
                .x
                .saturating_add(8)
                .saturating_add(search.cursor.min(u16::MAX as usize) as u16)
                .min(last_column);
            f.set_cursor_position(Position {
                x: column,
                y: status_area.y,
            });
        }
        if self.rebase_plan.is_some() {
            self.render_rebase_plan_popup(f, area);
        }
        if self.commit_popup {
            self.render_commit_popup(f, area);
        }
        if self.graph_action.is_some() {
            self.render_graph_action_popup(f, area);
        }
        if self.abort_rebase_popup {
            self.render_abort_rebase_popup(f, area);
        }
        if self.stash_popup.is_some() {
            self.render_stash_popup(f, area);
        }
        if self.error_popup.is_some() {
            self.render_rebase_error_popup(f, area);
        }
        if self.help_popup {
            self.render_help_popup(f, area);
        }
        self.apply_terminal_theme(f.buffer_mut());
    }

    fn render_help_popup(&self, f: &mut Frame, area: Rect) {
        const SECTIONS: &[(&str, &[(&str, &str)])] = &[
            (
                "Global",
                &[
                    ("Tab / Shift+Tab", "next / previous pane"),
                    ("z", "maximize / restore focused pane"),
                    ("g", "toggle graph pane"),
                    ("Shift+H", "show/hide focused-panel shortcuts"),
                    ("j / k", "move selection"),
                    ("/", "search commits"),
                    ("u", "undo from reflog"),
                    ("?", "toggle this help"),
                    ("U", "check/install update"),
                    ("q", "quit"),
                    ("Esc", "close popup / cancel selection"),
                ],
            ),
            (
                "Graph",
                &[
                    ("Enter", "checkout branch"),
                    ("c", "checkout commit"),
                    ("b / t", "new branch / tag"),
                    ("r", "reset"),
                    ("n / d", "rename / delete ref"),
                    ("p", "cherry-pick or pull"),
                    ("x", "revert"),
                    ("w / e", "reword / edit commit"),
                    ("i", "interactive rebase"),
                    ("R", "rebase onto selected"),
                    ("F / +", "fetch / push"),
                ],
            ),
            (
                "Staging",
                &[
                    ("Space", "stage/unstage selection"),
                    ("f / h / l", "file / hunk / line mode"),
                    ("F", "apply whole file"),
                    ("x", "discard unstaged"),
                    ("s", "stash manager"),
                    ("c / a", "commit / amend"),
                ],
            ),
            (
                "Diff",
                &[
                    ("v", "unified / side-by-side"),
                    ("[ / ]", "previous / next hunk"),
                    ("j / k", "scroll a line"),
                    ("← / →", "pan horizontally"),
                    ("L", "load full truncated diff"),
                ],
            ),
            (
                "Conflicts",
                &[
                    ("o / t", "preview ours / theirs"),
                    ("b", "preview entire file from Base"),
                    ("e", "open in editor"),
                    ("Enter", "confirm result"),
                    ("Esc", "cancel preview"),
                    ("[ / ]", "switch hunk"),
                    ("h / l", "pan"),
                ],
            ),
            (
                "Stash (s)",
                &[
                    ("↑ / ↓", "select a stash"),
                    ("u", "include untracked"),
                    ("s", "save a stash"),
                    ("a", "apply the stash"),
                    ("p", "pop the stash"),
                    ("d", "drop the stash"),
                    ("Esc / q", "close"),
                ],
            ),
            (
                "Rebase",
                &[
                    ("↑ / ↓", "move between commits"),
                    ("Alt+↑ / Alt+↓", "reorder the commit"),
                    ("← / → / Space", "cycle the action"),
                    (
                        "p / w / e / s / f / d",
                        "pick / reword / edit / squash / fixup / drop",
                    ),
                    ("Enter", "run the plan"),
                    ("Ctrl+C / Ctrl+A", "continue / abort"),
                ],
            ),
        ];

        // Blank the whole screen so the surrounding panes don't show through.
        f.render_widget(Clear, area);
        let width = 74.min(area.width);
        let height = area.height.min(30);
        let popup = Rect::new(
            area.width.saturating_sub(width) / 2,
            area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        f.render_widget(Clear, popup);
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title("Help — keys")
                .title_bottom(styled_key_hints(
                    &[("Esc / ? / q", "close")],
                    Color::LightCyan,
                ))
                .border_style(Color::LightCyan),
            popup,
        );
        let inner = popup.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        let mut lines: Vec<Line> = Vec::new();
        for (index, (title, entries)) in SECTIONS.iter().enumerate() {
            if index > 0 {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled(
                *title,
                Style::default()
                    .fg(Color::LightMagenta)
                    .add_modifier(Modifier::BOLD),
            )));
            for (key, description) in *entries {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {key:<20}"),
                        Style::default().fg(Color::LightCyan),
                    ),
                    Span::styled(*description, Style::default().fg(Color::White)),
                ]));
            }
        }
        f.render_widget(
            Paragraph::new(lines)
                .scroll((self.help_scroll, 0))
                .wrap(Wrap { trim: false }),
            inner,
        );
    }

    fn render_abort_rebase_popup(&self, f: &mut Frame, area: Rect) {
        let popup_height = area
            .height
            .min(if self.action_error.is_some() { 9 } else { 8 });
        let popup = Rect::new(
            area.width / 5,
            area.height.saturating_sub(popup_height) / 2,
            area.width * 3 / 5,
            popup_height,
        );
        f.render_widget(Clear, popup);
        let hints = styled_key_hints(
            &[("Tab", "choose"), ("Enter", "confirm"), ("Esc", "close")],
            Color::LightCyan,
        );
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title("Abort rebase")
                .title_bottom(hints)
                .border_style(Color::LightRed),
            popup,
        );
        let inner = popup.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        let content_height = if self.action_error.is_some() { 4 } else { 3 };
        let panes = Layout::vertical([Constraint::Length(content_height), Constraint::Length(3)])
            .split(inner);
        let message = match &self.action_error {
            Some(error) => format!(
                "Abort the in-progress Git rebase and return to its original branch state?\n\nError: {error}"
            ),
            None => "Abort the in-progress Git rebase and return to its original branch state?"
                .to_string(),
        };
        f.render_widget(Paragraph::new(message).wrap(Wrap { trim: false }), panes[0]);
        let abort_style = if self.commit_field == CommitField::Commit {
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightRed)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::LightRed)
        };
        let keep_style = if self.commit_field == CommitField::Cancel {
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightGreen)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::LightGreen)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Abort rebase ", abort_style),
                Span::raw("  "),
                Span::styled(" Keep rebase ", keep_style),
            ]))
            .block(Block::default().borders(Borders::ALL)),
            panes[1],
        );
    }

    fn render_three_way_view(&self, f: &mut Frame, area: Rect) {
        let Some(view) = &self.three_way_view else {
            return;
        };
        let mut actions = vec![("o/t/b", "choose"), ("[/]", "hunk"), ("e", "edit")];
        if view.result_preview {
            actions.push(("Enter", "confirm result"));
            actions.push(("Esc", "cancel preview"));
        }
        // Panning and scrolling only work while the diff pane is focused.
        if self.focus == Focus::Diff {
            actions.push(("h/l", "pan"));
        }
        let hints = styled_key_hints(&actions, Color::LightCyan);
        let border = Style::default()
            .fg(Color::Rgb(255, 105, 180))
            .add_modifier(Modifier::BOLD);
        let working = String::from_utf8_lossy(&view.working);
        let hunk_count = conflict_hunks(&working).len();
        let title = if hunk_count > 0 {
            format!(
                "Three-way merge — {} · Hunk {}/{}",
                view.path,
                view.hunk_index + 1,
                hunk_count
            )
        } else {
            format!("Three-way merge — {}", view.path)
        };
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .title_bottom(hints)
                .border_style(border),
            area,
        );
        let inner = area.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        if inner.width < 24 || inner.height < 7 {
            f.render_widget(
                Paragraph::new("Terminal too small for the conflict resolver")
                    .wrap(Wrap { trim: false }),
                inner,
            );
            return;
        }
        let sections =
            Layout::vertical([Constraint::Percentage(50), Constraint::Min(0)]).split(inner);
        let sides =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Min(0)]).split(sections[0]);
        let (ours_color, theirs_color) = self
            .conflict_colors()
            .unwrap_or((Color::LightBlue, Color::LightYellow));
        let ours_selected = view.chosen_side == Some(ConflictSide::Ours);
        let theirs_selected = view.chosen_side == Some(ConflictSide::Theirs);
        let short_id = |id: &str| id.chars().take(7).collect::<String>();
        let ids = self.conflict_commits.as_ref();
        let ours_title = format!(
            "Ours {}{}",
            ids.map_or(String::new(), |commits| short_id(&commits.ours)),
            if view.ours_missing { " (deleted)" } else { "" }
        );
        let theirs_title = format!(
            "Theirs {}{}",
            ids.map_or(String::new(), |commits| short_id(&commits.theirs)),
            if view.theirs_missing {
                " (deleted)"
            } else {
                ""
            }
        );
        let ours_block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(ours_title, Style::default().fg(ours_color)))
            .border_style(conflict_side_border_style(
                ours_color,
                ours_selected,
                &self.terminal_palette,
            ));
        let theirs_block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(
                theirs_title,
                Style::default().fg(theirs_color),
            ))
            .border_style(conflict_side_border_style(
                theirs_color,
                theirs_selected,
                &self.terminal_palette,
            ));
        let ours_area = sides[0].inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        let theirs_area = sides[1].inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        let side_visible = usize::from(ours_area.height.min(theirs_area.height));
        let vertical_scroll = view.scroll_y.min(u16::MAX as usize) as u16;
        let horizontal_scroll = view.scroll_x.min(u16::MAX as usize) as u16;
        let display_path = view.path.display();
        let ours_rows = render_conflict_side_diff(
            &view.ours_diff,
            display_path.as_ref(),
            self.diff_number_width,
            usize::from(ours_area.width),
            self.diff_mode,
            ours_color,
            &self.terminal_palette,
        );
        let theirs_rows = render_conflict_side_diff(
            &view.theirs_diff,
            display_path.as_ref(),
            self.diff_number_width,
            usize::from(theirs_area.width),
            self.diff_mode,
            theirs_color,
            &self.terminal_palette,
        );
        f.render_widget(
            Paragraph::new(ours_rows)
                .scroll((vertical_scroll, horizontal_scroll))
                .block(ours_block),
            sides[0],
        );
        f.render_widget(
            Paragraph::new(theirs_rows)
                .scroll((vertical_scroll, horizontal_scroll))
                .block(theirs_block),
            sides[1],
        );

        let result_title =
            if view.result_preview && view.pending_resolution == Some(ConflictResolution::Delete) {
                "Delete entire file — Enter confirm · Esc cancel"
            } else if view.result_preview {
                match view.chosen_side {
                    Some(ConflictSide::Ours) => "Result vs Base — Ours",
                    Some(ConflictSide::Theirs) => "Result vs Base — Theirs",
                    Some(ConflictSide::Base) => {
                        "Restore entire file from Base — Enter confirm · Esc cancel"
                    }
                    None => "Result vs Base — Edited",
                }
            } else if view.result_missing {
                "Result (deleted)"
            } else {
                "Result (conflict markers)"
            };
        let result_block = Block::default()
            .borders(Borders::ALL)
            .title(result_title)
            .border_style(Style::default().fg(Color::LightRed));
        let result_area = sections[1].inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        let result_visible = usize::from(result_area.height);
        let result_rows = if view.result_preview {
            let result_side_color = if view.chosen_side == Some(ConflictSide::Theirs) {
                theirs_color
            } else {
                ours_color
            };
            render_conflict_side_diff(
                &view.result_diff,
                display_path.as_ref(),
                self.diff_number_width,
                usize::from(result_area.width),
                self.diff_mode,
                result_side_color,
                &self.terminal_palette,
            )
            .into_iter()
            .skip(view.scroll_y)
            .take(result_visible)
            .collect::<Vec<_>>()
        } else {
            view.rows
                .iter()
                .skip(view.scroll_y)
                .take(result_visible)
                .map(|row| {
                    three_way_column(
                        row.result.as_ref(),
                        row.base.as_ref(),
                        display_path.as_ref(),
                        usize::from(result_area.width),
                        view.scroll_x,
                        Color::White,
                    )
                })
                .collect::<Vec<_>>()
        };
        f.render_widget(Paragraph::new(result_rows).block(result_block), sections[1]);

        let content_rows = view
            .rows
            .len()
            .max(view.ours_diff.len())
            .max(view.theirs_diff.len())
            .max(if view.result_preview {
                view.result_diff.len()
            } else {
                0
            });
        if content_rows > side_visible.max(result_visible) {
            let mut state = ScrollbarState::new(content_rows)
                .position(
                    view.scroll_y
                        .min(content_rows.saturating_sub(side_visible.max(result_visible))),
                )
                .viewport_content_length(side_visible.max(result_visible));
            f.render_stateful_widget(
                minimal_scrollbar(if self.focus == Focus::Diff {
                    Color::LightCyan
                } else {
                    Color::DarkGray
                }),
                border_scrollbar_area(area),
                &mut state,
            );
        }
    }

    fn render_stash_popup(&self, f: &mut Frame, area: Rect) {
        let Some(state) = &self.stash_popup else {
            return;
        };
        let popup_width = area.width.saturating_mul(4).div_ceil(5).min(area.width);
        let popup_height = if state.saving {
            area.height.min(9)
        } else {
            let rows = state.entries.len() as u16;
            area.height.min(rows.saturating_add(5).max(7))
        };
        let popup = Rect::new(
            area.x + area.width.saturating_sub(popup_width) / 2,
            area.y + area.height.saturating_sub(popup_height) / 2,
            popup_width,
            popup_height,
        );
        let hints = if state.confirming_drop {
            styled_key_hints(
                &[("y/Enter", "drop stash"), ("n/Esc", "cancel")],
                Color::LightCyan,
            )
        } else if state.saving {
            styled_key_hints(
                &[
                    ("Type", "message"),
                    ("Tab", "untracked"),
                    ("Enter", "save"),
                    ("Esc", "cancel"),
                ],
                Color::LightCyan,
            )
        } else {
            styled_key_hints(
                &[
                    ("↑/↓", "select"),
                    ("s", "save"),
                    ("u", "untracked"),
                    ("a", "apply"),
                    ("p", "pop"),
                    ("d", "drop"),
                    ("Esc/q", "close"),
                ],
                Color::LightCyan,
            )
        };
        f.render_widget(Clear, popup);
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Stashes ({})", state.entries.len()))
                .title_bottom(hints)
                .border_style(Color::LightCyan),
            popup,
        );
        let inner = popup.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        let body = if state.saving {
            Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Min(0),
            ])
            .split(inner)
        } else {
            Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(inner)
        };
        let message = if state.confirming_drop {
            state
                .entries
                .get(state.selected)
                .map(|entry| format!("Drop {} — {}?", entry.reference, entry.message))
                .unwrap_or_else(|| "Select a stash to drop".into())
        } else {
            format!(
                "Include untracked files: {} ({})",
                if state.include_untracked { "yes" } else { "no" },
                if state.saving {
                    "Tab toggles"
                } else {
                    "u toggles"
                }
            )
        };
        f.render_widget(
            Paragraph::new(message).style(Style::default().fg(if state.confirming_drop {
                Color::LightRed
            } else {
                Color::Gray
            })),
            body[0],
        );

        if state.saving {
            let input_block = Block::default()
                .borders(Borders::ALL)
                .title("Stash message")
                .border_style(Color::LightCyan);
            let input_area = body[1].inner(Margin {
                horizontal: 1,
                vertical: 1,
            });
            let prefix = "Message: ";
            let (cursor, horizontal_scroll) = single_line_cursor(
                &state.message,
                Line::from(prefix).width(),
                state.message_cursor,
                input_area,
            );
            let mut spans = vec![Span::styled(prefix, Style::default().fg(Color::LightCyan))];
            if state.message.is_empty() {
                spans.push(Span::styled(
                    "(leave empty for \"WIP\")",
                    Style::default().fg(Color::DarkGray),
                ));
            } else {
                spans.push(Span::raw(state.message.clone()));
            }
            f.render_widget(
                Paragraph::new(Line::from(spans))
                    .scroll((0, horizontal_scroll))
                    .block(input_block),
                body[1],
            );
            f.set_cursor_position(cursor);
            return;
        }

        let list_area = body[1];
        let visible = list_area.height as usize;
        let scroll = state.selected.saturating_sub(visible.saturating_sub(1));
        let width = usize::from(list_area.width);
        let mut entries = if state.entries.is_empty() {
            vec![Line::from(Span::styled(
                "(no stashes)",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            state
                .entries
                .iter()
                .enumerate()
                .skip(scroll)
                .take(visible)
                .map(|(index, entry)| {
                    let short_id = entry.id.chars().take(7).collect::<String>();
                    let text = truncate(
                        &format!("{}  {}  {}", entry.reference, short_id, entry.message),
                        width,
                    );
                    let mut line = Line::from(Span::styled(
                        text,
                        Style::default().fg(if index == state.selected {
                            Color::White
                        } else {
                            Color::Gray
                        }),
                    ));
                    if index == state.selected {
                        line = line.style(Style::default().bg(Color::Rgb(45, 50, 65)));
                    }
                    line
                })
                .collect::<Vec<_>>()
        };
        pad_colored_lines(&mut entries, width);
        f.render_widget(
            Paragraph::new(entries).scroll((scroll as u16, 0)),
            list_area,
        );
    }

    fn render_rebase_error_popup(&self, f: &mut Frame, area: Rect) {
        let message = self.error_popup.as_deref().unwrap_or_default();
        // Size the box to the message so a one-line error is not a huge empty
        // box; long messages wrap and then scroll.
        let popup_width = area.width.saturating_sub(4).min(80).max(24.min(area.width));
        let inner_width = usize::from(popup_width.saturating_sub(4)).max(1);
        let wrapped = message
            .lines()
            .map(|line| line.chars().count().max(1).div_ceil(inner_width))
            .sum::<usize>()
            .max(1);
        let popup_height = (wrapped as u16 + 3).min(area.height).max(5);
        let popup = Rect::new(
            area.x + area.width.saturating_sub(popup_width) / 2,
            area.y + area.height.saturating_sub(popup_height) / 2,
            popup_width,
            popup_height,
        );
        f.render_widget(Clear, popup);
        let hints = styled_key_hints(
            &[
                ("↑/↓", "scroll"),
                ("PgUp/PgDn", "page"),
                ("Enter", "close"),
                ("Esc", "close"),
            ],
            Color::LightCyan,
        );
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(self.error_title.as_str())
                .title_bottom(hints)
                .border_style(Color::LightRed),
            popup,
        );
        let inner = popup.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        f.render_widget(
            Paragraph::new(message)
                .wrap(Wrap { trim: false })
                .scroll((self.error_scroll, 0)),
            inner,
        );
    }

    fn render_rebase_plan_popup(&self, f: &mut Frame, area: Rect) {
        let popup_width = area.width.saturating_mul(9).div_ceil(10).min(area.width);
        let popup_height = area.height.saturating_mul(4).div_ceil(5).min(area.height);
        let popup = Rect::new(
            area.x + area.width.saturating_sub(popup_width) / 2,
            area.y + area.height.saturating_sub(popup_height) / 2,
            popup_width,
            popup_height,
        );
        let Some(plan_state) = &self.rebase_plan else {
            return;
        };
        let base: String = plan_state.plan.target.chars().take(7).collect();
        let hints = styled_key_hints(
            &[
                ("↑↓", "select"),
                ("←→", "cycle"),
                ("Alt+↑↓", "reorder"),
                ("Enter", "run"),
                ("Esc", "close"),
            ],
            Color::LightCyan,
        );
        f.render_widget(Clear, popup);
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Interactive rebase — from {base} (base included)"))
                .title_bottom(hints)
                .border_style(Color::LightCyan),
            popup,
        );

        let inner = popup.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        if inner.height < 2 {
            if inner.height > 0 {
                f.render_widget(
                    Paragraph::new("Terminal too small for the rebase plan"),
                    inner,
                );
            }
            return;
        }
        f.render_widget(
            Paragraph::new(Text::from(vec![
                Line::from(Span::styled(
                    "Git order is preserved around merges; merge rows are fixed",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(vec![
                    Span::styled("p pick", Style::default().fg(Color::LightGreen)),
                    Span::raw(" • "),
                    Span::styled("w reword/message", Style::default().fg(Color::LightCyan)),
                    Span::raw(" • "),
                    Span::styled("e edit", Style::default().fg(Color::LightYellow)),
                    Span::raw(" • "),
                    Span::styled("s squash", Style::default().fg(Color::LightMagenta)),
                    Span::raw(" • "),
                    Span::styled("f fixup", Style::default().fg(Color::LightBlue)),
                    Span::raw(" • "),
                    Span::styled("d drop", Style::default().fg(Color::LightRed)),
                ]),
            ])),
            Rect::new(inner.x, inner.y, inner.width, 2),
        );
        let list_area = Rect::new(
            inner.x,
            inner.y.saturating_add(2),
            inner.width,
            inner.height.saturating_sub(2),
        );
        let visible = usize::from(list_area.height).max(1);
        let scroll = plan_state
            .selected
            .saturating_sub(visible.saturating_sub(1));
        let lines = (scroll..rebase_plan_entry_count(&plan_state.plan).min(scroll + visible))
            .filter_map(|index| {
                let entry = plan_entry_at(&plan_state.plan, index)?;
                let selected = index == plan_state.selected;
                let label = rebase_todo_action_name(&entry.action);
                let color = match &entry.action {
                    RebaseTodoAction::Pick => Color::LightGreen,
                    RebaseTodoAction::Reword => Color::LightCyan,
                    RebaseTodoAction::Edit => Color::LightYellow,
                    RebaseTodoAction::Squash => Color::LightMagenta,
                    RebaseTodoAction::Fixup => Color::LightBlue,
                    RebaseTodoAction::Drop => Color::LightRed,
                    RebaseTodoAction::Merge => Color::DarkGray,
                };
                let prefix = if selected { "› " } else { "  " };
                let subject_width = usize::from(list_area.width).saturating_sub(prefix.len() + 10);
                let subject = truncate(&entry.subject, subject_width);
                let mut line = Line::from(vec![
                    Span::styled(
                        prefix,
                        Style::default().fg(if selected {
                            Color::LightCyan
                        } else {
                            Color::DarkGray
                        }),
                    ),
                    Span::styled(
                        format!("{label:<7}"),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        subject,
                        Style::default().fg(if selected { Color::White } else { Color::Gray }),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        entry.id.chars().take(7).collect::<String>(),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]);
                if selected {
                    line = line.style(Style::default().bg(Color::Rgb(45, 50, 65)));
                }
                pad_colored_lines(
                    std::slice::from_mut(&mut line),
                    usize::from(list_area.width),
                );
                Some(line)
            })
            .collect::<Vec<_>>();
        f.render_widget(Paragraph::new(lines).scroll((0, 0)), list_area);
    }

    fn render_staging(&self, f: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        if self.historical_commit.is_some() {
            let indices: Vec<usize> = (0..self.status.len()).collect();
            self.render_status_section(
                f,
                area,
                "Changes",
                &indices,
                Color::LightCyan,
                StatusSection::Unstaged,
            );
            return;
        }
        let staged = self.section_indices(StatusSection::Staged);
        let unstaged = self.section_indices(StatusSection::Unstaged);

        let panes = self.staging_panes(area);
        self.render_status_section(
            f,
            panes[0],
            "Unstaged",
            unstaged,
            Color::LightYellow,
            StatusSection::Unstaged,
        );
        self.render_status_section(
            f,
            panes[1],
            "Staged",
            staged,
            Color::LightGreen,
            StatusSection::Staged,
        );
    }

    fn render_commit_details(&self, f: &mut Frame, area: Rect) {
        let Some(id) = self.historical_commit.as_deref() else {
            return;
        };
        let Some(commit) = self.graph.commits.iter().find(|commit| commit.id == id) else {
            return;
        };
        let short: String = commit.id.chars().take(7).collect();
        let date = fmt_date(commit.committer_timestamp, commit.committer_timezone_offset);
        let mut lines = vec![
            Line::from(vec![
                Span::styled(format!("{short} "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    commit.author_name.clone(),
                    Style::default().fg(Color::LightCyan),
                ),
                Span::styled(format!("  {date}"), Style::default().fg(Color::DarkGray)),
            ]),
            Line::from(Span::styled(
                commit.summary.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];
        lines.extend(
            commit
                .message
                .lines()
                .map(|line| Line::from(line.to_string())),
        );
        if commit.message.is_empty() {
            lines.push(Line::from(""));
        }
        if let Some(query) = self.selected_search_query() {
            highlight_search_matches(&mut lines, query);
        }
        let content_width = area.width.saturating_sub(2).max(1) as usize;
        let visual_lines = lines
            .iter()
            .map(|line| {
                let width = line.to_string().chars().count();
                width.max(1).div_ceil(content_width)
            })
            .sum::<usize>();
        let viewport = area.height.saturating_sub(2) as usize;
        let scroll =
            (self.details_scroll as usize).min(visual_lines.saturating_sub(viewport)) as u16;
        f.render_widget(
            Paragraph::new(lines)
                .scroll((scroll, 0))
                .wrap(Wrap { trim: false })
                .block(self.panel_footer(
                    self.panel_block("Commit Details", self.focus == Focus::Details),
                    area,
                    self.focus == Focus::Details,
                    None,
                )),
            area,
        );
        if visual_lines > viewport {
            let mut state = ScrollbarState::new(visual_lines)
                .position(scrollbar_position(scroll as usize, visual_lines, viewport))
                .viewport_content_length(viewport);
            f.render_stateful_widget(
                minimal_scrollbar(Color::LightCyan),
                border_scrollbar_area(area),
                &mut state,
            );
        }
    }

    fn render_commit_popup(&self, f: &mut Frame, area: Rect) {
        let desired_height = if self.commit_confirm { 8 } else { 14 };
        let popup_height = area.height.min(desired_height);
        let popup = Rect::new(
            area.width / 5,
            area.height.saturating_sub(popup_height) / 2,
            area.width * 3 / 5,
            popup_height,
        );
        let mode = match &self.commit_mode {
            CommitMode::Normal => "Commit",
            CommitMode::Amend => "Amend latest",
            CommitMode::Fixup(_) => "Fixup selected commit",
            CommitMode::Reword(_) => "Reword selected commit",
            CommitMode::Edit(_) => "Edit selected commit",
            CommitMode::RebaseReplacement => "Replace edited commit",
            CommitMode::PlanReword(_) => "Reword in rebase plan",
        };
        f.render_widget(Clear, popup);
        let popup_hints = styled_key_hints(
            &[("Tab", "next"), ("Enter", "select"), ("Esc", "close")],
            Color::LightCyan,
        );
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(mode)
                .title_bottom(popup_hints)
                .border_style(Color::LightCyan),
            popup,
        );

        let inner = popup.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        if self.commit_confirm {
            let target = match &self.commit_mode {
                CommitMode::Fixup(target) => target.chars().take(7).collect::<String>(),
                CommitMode::Edit(target) => target.chars().take(7).collect::<String>(),
                _ => String::new(),
            };
            let panes =
                Layout::vertical([Constraint::Length(3), Constraint::Length(3)]).split(inner);
            let commit_style = self.button_style(self.commit_field == CommitField::Commit);
            let cancel_style = self.button_style(self.commit_field == CommitField::Cancel);
            let prompt = match &self.commit_mode {
                CommitMode::Normal => "Create the commit?".to_string(),
                CommitMode::Amend => "Amend the latest commit?".to_string(),
                CommitMode::Fixup(_) => format!("Fixup staged changes into {target}?"),
                CommitMode::Reword(_) => {
                    "Reword the selected commit and replay descendants?".to_string()
                }
                CommitMode::Edit(_) => format!("Rebase and stop to edit commit {target}?"),
                CommitMode::RebaseReplacement => "Replace the edited commit?".to_string(),
                CommitMode::PlanReword(_) => "Save the reworded message?".to_string(),
            };
            let prompt = match &self.action_error {
                Some(error) => format!("{prompt}\nError: {error}"),
                None => prompt,
            };
            f.render_widget(Paragraph::new(prompt).wrap(Wrap { trim: false }), panes[0]);
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        if self.commit_field == CommitField::Commit {
                            "▸ Confirm "
                        } else {
                            "  Confirm "
                        },
                        commit_style,
                    ),
                    Span::raw("  "),
                    Span::styled(
                        if self.commit_field == CommitField::Cancel {
                            "▸ Cancel "
                        } else {
                            "  Cancel "
                        },
                        cancel_style,
                    ),
                ]))
                .block(Block::default().borders(Borders::ALL)),
                panes[1],
            );
            return;
        }
        let panes = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(6),
            Constraint::Length(3),
        ])
        .split(inner);
        let selected_border = Style::default()
            .fg(Color::LightCyan)
            .add_modifier(Modifier::BOLD);
        let idle_border = Style::default().fg(Color::DarkGray);
        let title_block = Block::default()
            .borders(Borders::ALL)
            .title("Title")
            .border_style(if self.commit_field == CommitField::Title {
                selected_border
            } else {
                idle_border
            });
        let mut description_block = Block::default()
            .borders(Borders::ALL)
            .title("Description")
            .border_style(if self.commit_field == CommitField::Description {
                selected_border
            } else {
                idle_border
            });
        if let Some(error) = &self.action_error {
            description_block = description_block.title_bottom(Span::styled(
                format!(" {error} "),
                Style::default().fg(Color::LightRed),
            ));
        }
        let title_style = if self.commit_field == CommitField::Title {
            Style::default().fg(Color::White).bg(Color::Rgb(45, 50, 65))
        } else {
            Style::default()
        };
        let description_style = if self.commit_field == CommitField::Description {
            Style::default().fg(Color::White).bg(Color::Rgb(45, 50, 65))
        } else {
            Style::default()
        };
        let title_area = panes[0].inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        let (title_cursor, title_scroll) =
            single_line_cursor(&self.commit_title, 0, self.commit_title_cursor, title_area);
        f.render_widget(
            Paragraph::new(self.commit_title.clone())
                .style(title_style)
                .scroll((0, title_scroll))
                .block(title_block),
            panes[0],
        );
        if self.commit_field == CommitField::Title {
            f.set_cursor_position(title_cursor);
        }
        let description_area = panes[1].inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        let (description_cursor, description_scroll) = wrapped_text_cursor(
            &self.commit_description,
            self.commit_description_cursor,
            description_area,
        );
        let description_lines = wrapped_text_lines(
            &self.commit_description,
            usize::from(description_area.width).max(1),
        );
        f.render_widget(
            Paragraph::new(description_lines)
                .style(description_style)
                .scroll((description_scroll, 0))
                .block(description_block),
            panes[1],
        );
        if self.commit_field == CommitField::Description {
            f.set_cursor_position(description_cursor);
        }

        let commit_style = self.button_style(self.commit_field == CommitField::Commit);
        let cancel_style = self.button_style(self.commit_field == CommitField::Cancel);
        let confirm_label = match self.commit_mode {
            CommitMode::Reword(_) => " Rebase ",
            CommitMode::RebaseReplacement => " Replace ",
            CommitMode::PlanReword(_) => " Save message ",
            _ => " Commit ",
        };
        let button_block = Block::default().borders(Borders::ALL).border_style(
            if matches!(self.commit_field, CommitField::Commit | CommitField::Cancel) {
                selected_border
            } else {
                idle_border
            },
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(
                        "{}{confirm_label}",
                        if self.commit_field == CommitField::Commit {
                            "▸"
                        } else {
                            " "
                        }
                    ),
                    commit_style,
                ),
                Span::raw("  "),
                Span::styled(
                    if self.commit_field == CommitField::Cancel {
                        "▸ Cancel "
                    } else {
                        "  Cancel "
                    },
                    cancel_style,
                ),
            ]))
            .block(button_block),
            panes[2],
        );
    }

    fn render_graph_action_popup(&self, f: &mut Frame, area: Rect) {
        let action = self.graph_action.unwrap();
        let is_option_picker = matches!(action, GraphAction::Reset | GraphAction::Push);
        let popup_height = if is_option_picker {
            area.height.min(6)
        } else if matches!(action, GraphAction::Fetch | GraphAction::PushRemote) {
            self.remote_choices
                .len()
                .saturating_add(4)
                .min(usize::from(area.height)) as u16
        } else if action == GraphAction::Undo {
            area.height.min(16)
        } else if self.action_error.is_some() || action == GraphAction::Discard {
            area.height.min(10)
        } else {
            area.height.min(8)
        };
        let popup_width = if action == GraphAction::Undo {
            area.width * 4 / 5
        } else {
            area.width * 3 / 5
        };
        let popup = Rect::new(
            area.width.saturating_sub(popup_width) / 2,
            area.height.saturating_sub(popup_height) / 2,
            popup_width,
            popup_height,
        );
        let title = match action {
            GraphAction::Branch => "New branch",
            GraphAction::Tag => "New tag",
            GraphAction::Reset => "Reset",
            GraphAction::Checkout => "Checkout",
            GraphAction::CheckoutBranch => "Checkout branch",
            GraphAction::CherryPick => "Cherry-pick",
            GraphAction::Revert => "Revert",
            GraphAction::RebaseOnto => "Rebase onto",
            GraphAction::Discard => "Discard changes",
            GraphAction::Rename => "Rename branch",
            GraphAction::Delete => {
                if self
                    .delete_targets
                    .get(self.delete_target_index)
                    .is_some_and(DeleteTarget::is_tag)
                {
                    "Delete tag"
                } else {
                    "Delete branch"
                }
            }
            GraphAction::Pull => "Pull",
            GraphAction::Fetch => "Fetch from remote",
            GraphAction::PushRemote => "Push to remote",
            GraphAction::Push => "Push",
            GraphAction::Undo => "Undo from reflog",
        };
        let popup_hints = match action {
            GraphAction::Fetch | GraphAction::PushRemote => styled_key_hints(
                &[
                    ("↑/↓", "remote"),
                    (
                        "Enter",
                        if action == GraphAction::Fetch {
                            "fetch"
                        } else {
                            "next"
                        },
                    ),
                    ("Esc", "close"),
                ],
                Color::LightCyan,
            ),
            GraphAction::Undo => styled_key_hints(
                &[
                    ("↑/↓", "reflog entry"),
                    ("Enter", "restore"),
                    ("Esc", "close"),
                ],
                Color::LightCyan,
            ),
            GraphAction::Reset | GraphAction::Push => styled_key_hints(
                &[("Tab/↑↓", "next"), ("Enter", "select"), ("Esc", "close")],
                Color::LightCyan,
            ),
            GraphAction::Branch => styled_key_hints(
                &[
                    ("Type", "name"),
                    ("Tab", "next"),
                    ("Enter", "ok"),
                    ("Esc", "close"),
                ],
                Color::LightCyan,
            ),
            GraphAction::Tag => styled_key_hints(
                &[
                    ("Type", "name"),
                    ("Ctrl+P", "push after"),
                    ("Enter", "ok"),
                    ("Esc", "close"),
                ],
                Color::LightCyan,
            ),
            GraphAction::Rename => styled_key_hints(
                &[
                    ("Ctrl+R", "remote"),
                    ("Tab", "next"),
                    ("Enter", "ok"),
                    ("Esc", "close"),
                ],
                Color::LightCyan,
            ),
            GraphAction::Delete => styled_key_hints(
                &[
                    ("←/→", "target"),
                    ("r", "remote"),
                    ("Enter", "ok"),
                    ("Esc", "close"),
                ],
                Color::LightCyan,
            ),
            GraphAction::RebaseOnto => {
                styled_key_hints(&[("Enter", "rebase"), ("Esc", "close")], Color::LightCyan)
            }
            _ => styled_key_hints(
                &[("Tab", "next"), ("Enter", "ok"), ("Esc", "close")],
                Color::LightCyan,
            ),
        };
        f.render_widget(Clear, popup);
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .title_bottom(popup_hints)
                .border_style(Color::LightCyan),
            popup,
        );
        let inner = popup.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        if action == GraphAction::Undo {
            self.render_undo_popup(f, inner);
            return;
        }
        if matches!(action, GraphAction::Fetch | GraphAction::PushRemote) {
            self.render_remote_picker(f, inner);
            return;
        }
        if action == GraphAction::Reset {
            let panes =
                Layout::vertical([Constraint::Length(1), Constraint::Length(3)]).split(inner);
            f.render_widget(
                Paragraph::new(self.action_error.as_deref().unwrap_or("Select reset mode:")),
                panes[0],
            );
            let labels = [" Soft ", " Mixed ", " Hard ", " Cancel "];
            let mut button_spans = Vec::new();
            for (i, label) in labels.iter().enumerate() {
                let selected = self.reset_focus == i;
                button_spans.push(Span::styled(
                    format!("{}{}", if selected { "▸" } else { " " }, label),
                    self.button_style(selected),
                ));
                if i + 1 < labels.len() {
                    button_spans.push(Span::raw("  "));
                }
            }
            f.render_widget(
                Paragraph::new(Line::from(button_spans))
                    .block(Block::default().borders(Borders::ALL)),
                panes[1],
            );
            return;
        }
        if action == GraphAction::Push {
            let panes =
                Layout::vertical([Constraint::Length(1), Constraint::Length(3)]).split(inner);
            f.render_widget(
                Paragraph::new(self.action_error.as_deref().unwrap_or("Select push mode:")),
                panes[0],
            );
            let labels = [" Normal ", " Force ", " Force-lease ", " Cancel "];
            let mut button_spans = Vec::new();
            for (i, label) in labels.iter().enumerate() {
                let selected = self.push_focus == i;
                button_spans.push(Span::styled(
                    format!("{}{}", if selected { "▸" } else { " " }, label),
                    self.button_style(selected),
                ));
                if i + 1 < labels.len() {
                    button_spans.push(Span::raw("  "));
                }
            }
            f.render_widget(
                Paragraph::new(Line::from(button_spans))
                    .block(Block::default().borders(Borders::ALL)),
                panes[1],
            );
            return;
        }
        let content_height = if self.action_error.is_some() { 5 } else { 3 };
        let panes = Layout::vertical([Constraint::Length(content_height), Constraint::Length(3)])
            .split(inner);
        let mut content = match action {
            GraphAction::Branch => format!("Branch name: {}", self.graph_action_input),
            GraphAction::Tag => format!("Tag name: {}", self.graph_action_input),
            GraphAction::CheckoutBranch => format!(
                "Checkout local branch {}?",
                self.selected_local_branch().unwrap_or_default()
            ),
            GraphAction::Reset => {
                format!("Reset mode: {}", ["soft", "mixed", "hard"][self.reset_mode])
            }
            GraphAction::Rename => format!(
                "New branch name: {}  •  Remote: {}",
                self.graph_action_input,
                if self.remote_action { "yes" } else { "no" }
            ),
            GraphAction::Delete => {
                let target = self.delete_targets.get(self.delete_target_index);
                let kind = if target.is_some_and(DeleteTarget::is_tag) {
                    "tag"
                } else {
                    "branch"
                };
                let name = target.map(DeleteTarget::name).unwrap_or_default();
                let position = if self.delete_targets.len() > 1 {
                    format!(
                        " ({}/{})",
                        self.delete_target_index + 1,
                        self.delete_targets.len()
                    )
                } else {
                    String::new()
                };
                format!(
                    "Delete {kind} {name}?{position}  •  Remote: {}",
                    if self.remote_action { "yes" } else { "no" }
                )
            }
            GraphAction::Checkout => "Checkout selected commit (detached HEAD)?".to_string(),
            GraphAction::RebaseOnto => format!(
                "Rebase {} onto {}?",
                self.graph
                    .head_name
                    .as_deref()
                    .unwrap_or("the checked-out branch"),
                self.selected_commit_label()
            ),
            GraphAction::Pull => "Pull from the tracked remote?".to_string(),
            GraphAction::Fetch | GraphAction::PushRemote => {
                unreachable!("remote picker has a dedicated renderer")
            }
            GraphAction::Push => format!(
                "Push mode: {}",
                ["normal", "force", "force-with-lease"][self.push_mode]
            ),
            GraphAction::Discard => {
                let count = self.discard_paths.len();
                let plural = if count == 1 { "file" } else { "files" };
                let names = self
                    .discard_paths
                    .iter()
                    .take(3)
                    .map(|path| path.display().into_owned())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "Discard {count} unstaged {plural}?\n{names}{}\nStaged changes are kept.",
                    if count > 3 { ", …" } else { "" }
                )
            }
            GraphAction::Undo => unreachable!("undo has a dedicated reflog picker"),
            _ => format!("Confirm {title} at selected commit?"),
        };
        if let Some(error) = &self.action_error {
            content.push_str(&format!("\n\nError: {error}"));
        }
        let content_title = match action {
            GraphAction::Branch | GraphAction::Tag | GraphAction::Rename => "Name",
            GraphAction::Reset => "Mode",
            _ => "Confirmation",
        };
        let has_text_input = matches!(
            action,
            GraphAction::Branch | GraphAction::Tag | GraphAction::Rename
        );
        let input_selected = has_text_input && self.commit_field == CommitField::Title;
        let content_border = if input_selected {
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD)
        } else if has_text_input {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::LightCyan)
        };
        let content_block = Block::default()
            .borders(Borders::ALL)
            .title(content_title)
            .border_style(content_border);
        if has_text_input {
            let field_style = if input_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let (label, suffix) = match action {
                GraphAction::Branch => ("Branch name: ", None),
                GraphAction::Tag => (
                    "Tag name: ",
                    Some(format!(
                        "  •  Push after: {}",
                        if self.push_after_create { "yes" } else { "no" }
                    )),
                ),
                GraphAction::Rename => (
                    "New branch name: ",
                    Some(format!(
                        "  •  Remote: {}",
                        if self.remote_action { "yes" } else { "no" }
                    )),
                ),
                _ => unreachable!(),
            };
            let mut input_lines = vec![Line::from(vec![
                Span::styled(label, Style::default().fg(Color::DarkGray)),
                Span::styled(format!(" {} ", self.graph_action_input), field_style),
                Span::styled(
                    suffix.unwrap_or_default(),
                    Style::default().fg(Color::White),
                ),
            ])];
            if let Some(error) = &self.action_error {
                input_lines.push(Line::from(Span::styled(
                    format!("Error: {error}"),
                    Style::default().fg(Color::LightRed),
                )));
            }
            let input_area = panes[0].inner(Margin {
                horizontal: 1,
                vertical: 1,
            });
            let prefix_width = Line::from(label.to_string()).width() + 1;
            let (cursor, scroll) = single_line_cursor(
                &self.graph_action_input,
                prefix_width,
                self.graph_action_cursor,
                input_area,
            );
            f.render_widget(
                Paragraph::new(input_lines)
                    .scroll((0, scroll))
                    .block(content_block),
                panes[0],
            );
            if input_selected {
                f.set_cursor_position(cursor);
            }
        } else {
            f.render_widget(
                Paragraph::new(content)
                    .wrap(Wrap { trim: false })
                    .block(content_block),
                panes[0],
            );
        }
        let commit_style = self.button_style(self.commit_field == CommitField::Commit);
        let cancel_style = self.button_style(self.commit_field == CommitField::Cancel);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    if self.commit_field == CommitField::Commit {
                        "▸ Confirm "
                    } else {
                        "  Confirm "
                    },
                    commit_style,
                ),
                Span::raw("  "),
                Span::styled(
                    if self.commit_field == CommitField::Cancel {
                        "▸ Cancel "
                    } else {
                        "  Cancel "
                    },
                    cancel_style,
                ),
            ]))
            .block(Block::default().borders(Borders::ALL)),
            panes[1],
        );
    }

    fn render_undo_popup(&self, f: &mut Frame, area: Rect) {
        let panes = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);
        let latest = self
            .undo_options
            .first()
            .map(|entry| entry.latest_message.as_str())
            .unwrap_or("HEAD reflog");
        f.render_widget(Paragraph::new(format!("Latest action: {latest}")), panes[0]);

        let mut entries = self
            .undo_options
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let short_id = entry.previous_id.chars().take(7).collect::<String>();
                let text = format!(
                    "{}HEAD@{{{}}}  {short_id}  {}",
                    if index == self.undo_selection {
                        "▸ "
                    } else {
                        "  "
                    },
                    entry.depth,
                    entry.message
                );
                let style = if index == self.undo_selection {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                Line::from(Span::styled(text, style))
            })
            .collect::<Vec<_>>();
        pad_colored_lines(&mut entries, usize::from(panes[1].width));
        let visible_rows = panes[1].height as usize;
        let scroll = self
            .undo_selection
            .saturating_sub(visible_rows.saturating_sub(1));
        f.render_widget(
            Paragraph::new(entries).scroll((scroll.min(u16::MAX as usize) as u16, 0)),
            panes[1],
        );

        let footer = self
            .action_error
            .as_deref()
            .unwrap_or("Ref movement only; discarded worktree contents aren't restored.");
        let footer_style = if self.action_error.is_some() {
            Style::default().fg(Color::LightRed)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        f.render_widget(Paragraph::new(Span::styled(footer, footer_style)), panes[2]);
    }

    fn render_remote_picker(&self, f: &mut Frame, area: Rect) {
        let mut remotes = self
            .remote_choices
            .iter()
            .enumerate()
            .map(|(index, remote)| {
                let style = if index == self.remote_selection {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                Line::from(Span::styled(
                    format!(
                        "{}{}",
                        if index == self.remote_selection {
                            "▸ "
                        } else {
                            "  "
                        },
                        remote
                    ),
                    style,
                ))
            })
            .collect::<Vec<_>>();
        pad_colored_lines(&mut remotes, usize::from(area.width));
        let visible_rows = usize::from(area.height);
        let scroll = self
            .remote_selection
            .saturating_sub(visible_rows.saturating_sub(1));
        f.render_widget(
            Paragraph::new(remotes).scroll((scroll.min(u16::MAX as usize) as u16, 0)),
            area,
        );
    }

    fn render_status_section(
        &self,
        f: &mut Frame,
        area: Rect,
        title: &str,
        indices: &[usize],
        color: Color,
        section: StatusSection,
    ) {
        if area.is_empty() {
            return;
        }
        let mut lines: Vec<Line> = if indices.is_empty() {
            let empty = if self.details_loading && self.historical_commit.is_some() {
                "Loading files…"
            } else if self.historical_commit.is_some() {
                "No file changes"
            } else if section == StatusSection::Staged {
                "Nothing staged"
            } else {
                "No unstaged changes"
            };
            vec![Line::from(Span::styled(
                empty,
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            indices
                .iter()
                .map(|&i| {
                    self.file_status_line(
                        i,
                        section == StatusSection::Staged,
                        area.width.saturating_sub(2) as usize,
                    )
                })
                .collect()
        };
        pad_colored_lines(&mut lines, usize::from(area.width.saturating_sub(2)));
        let focused = self.focus == Focus::Status
            && (self.historical_commit.is_some() || self.status_section == section);
        let title = format!("{title} ({})", indices.len());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(if focused {
                Color::LightCyan
            } else {
                self.border_color()
            })
            .title(Span::styled(
                title,
                Style::default().fg(color).add_modifier(if focused {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
            ));
        let block = self.panel_footer(block, area, focused, None);
        let viewport = area.height.saturating_sub(2) as usize;
        let scroll = self.status_list_scroll(indices, area);
        f.render_widget(
            Paragraph::new(lines)
                .scroll((scroll as u16, 0))
                .block(block),
            area,
        );
        if indices.len() > viewport {
            let mut state = ScrollbarState::new(indices.len())
                .position(scrollbar_position(scroll, indices.len(), viewport))
                .viewport_content_length(viewport);
            f.render_stateful_widget(
                minimal_scrollbar(if focused {
                    Color::LightCyan
                } else {
                    self.border_color()
                }),
                border_scrollbar_area(area),
                &mut state,
            );
        }
    }

    pub(super) fn file_status_line(
        &self,
        index: usize,
        staged: bool,
        width: usize,
    ) -> Line<'static> {
        let entry = &self.status[index];
        let code = if self.historical_commit.is_some() || staged {
            entry.index
        } else {
            entry.worktree
        };
        let (glyph, color) = if entry.is_conflict() {
            ('⚠', Color::LightRed)
        } else {
            status_glyph(code)
        };
        let resolving = !staged
            && self
                .three_way_view
                .as_ref()
                .is_some_and(|view| view.path == entry.path);
        let selected = resolving
            || (self.selected_statuses.contains(&index)
                && (self.historical_commit.is_some()
                    || self.status_section
                        == if staged {
                            StatusSection::Staged
                        } else {
                            StatusSection::Unstaged
                        }));
        let active = self.focus == Focus::Status || resolving;
        let selection = if selected {
            self.selection_style(active)
        } else {
            Style::default()
        };
        let selected_bg = selection.bg.unwrap_or(Color::Reset);
        let glyph_style = selection.fg(color);
        // Leave the stat blank until the async totals arrive, rather than
        // showing a misleading "+0 -0".
        let stat = self.diff_stats.get(&(entry.path.clone(), staged)).copied();
        let stat_text = stat.map_or_else(String::new, |(added, removed)| {
            format!("+{added} -{removed}")
        });
        let stats_width = Line::from(stat_text.as_str()).width();
        let glyph_width = Line::from(glyph.to_string()).width();
        let (icon, icon_color) = self.file_icon(&entry.path.display());
        let icon_width = Line::from(icon.to_string()).width();
        // In very narrow panes, keep the +/- totals visible.
        if width < glyph_width + 1 + icon_width + 1 + stats_width + 1 {
            let mut spans = Vec::new();
            if let Some((added, removed)) = stat {
                let removed = truncate_start(&format!("-{removed}"), width);
                let available = width.saturating_sub(Line::from(removed.as_str()).width());
                let added = truncate_start(&format!("+{added} "), available);
                let padding = available.saturating_sub(Line::from(added.as_str()).width());
                spans.push(Span::raw(" ".repeat(padding)));
                spans.push(Span::styled(added, Style::default().fg(Color::Green)));
                spans.push(Span::styled(removed, Style::default().fg(Color::Red)));
            }
            return Line::from(spans).style(selection.patch(if selected && self.no_color {
                self.selection_text_style(active)
            } else {
                Style::default()
            }));
        }
        let (staged_hunks, unstaged_hunks) = self
            .diff_hunk_counts
            .get(&entry.path)
            .copied()
            .unwrap_or((0, 0));
        let total_hunks = staged_hunks + unstaged_hunks;
        let mut hunk_recap = if total_hunks > 0 {
            let count = if staged { staged_hunks } else { unstaged_hunks };
            Some(format!("{count}/{total_hunks} hunks"))
        } else {
            None
        };
        let recap_width = hunk_recap
            .as_deref()
            .map_or(0, |recap| Line::from(recap).width());
        let recap_spacing = usize::from(hunk_recap.is_some());
        if hunk_recap.is_some()
            && width
                < glyph_width + 1 + icon_width + 1 + stats_width + recap_width + recap_spacing + 9
        {
            // Keep the +/- totals visible on narrow panes; the recap can be
            // omitted when both summaries cannot fit side by side.
            hunk_recap = None;
        }
        let recap_width = hunk_recap
            .as_deref()
            .map_or(0, |recap| Line::from(recap).width());
        let recap_spacing = usize::from(hunk_recap.is_some());
        let suffix_width = stats_width + recap_width + recap_spacing;
        let path_width = width.saturating_sub(glyph_width + 1 + icon_width + 1 + suffix_width + 1);
        let display_path = truncate_start(&entry.path.display(), path_width);
        let path_style = selection.patch(if selected {
            self.selection_text_style(active)
        } else {
            Style::default()
        });
        let path_display_width = Line::from(display_path.as_str()).width();
        let available_padding = width
            .saturating_sub(glyph_width + 1 + icon_width + 1 + path_display_width + suffix_width);
        let padding = if display_path.is_empty() {
            available_padding
        } else {
            available_padding.max(1)
        };
        let (directory, basename) = display_path
            .rfind('/')
            .map_or(("", display_path.as_str()), |slash| {
                display_path.split_at(slash + 1)
            });
        let icon_style = selection.fg(icon_color);
        let mut spans = vec![
            Span::styled(format!("{glyph} "), glyph_style),
            Span::styled(format!("{icon} "), icon_style),
            Span::styled(
                directory.to_string(),
                path_style
                    .fg(Color::DarkGray)
                    .remove_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ),
            Span::styled(basename.to_string(), path_style),
            Span::styled(
                " ".repeat(padding),
                Style::default().bg(if selected { selected_bg } else { Color::Reset }),
            ),
        ];
        if let Some(hunk_recap) = hunk_recap {
            spans.push(Span::styled(
                hunk_recap,
                Style::default().fg(Color::LightCyan).bg(if selected {
                    selected_bg
                } else {
                    Color::Reset
                }),
            ));
            spans.push(Span::styled(
                " ",
                Style::default().bg(if selected { selected_bg } else { Color::Reset }),
            ));
        }
        if let Some((added, removed)) = stat {
            spans.push(Span::styled(
                format!("+{added}"),
                Style::default().fg(Color::Green).bg(if selected {
                    selected_bg
                } else {
                    Color::Reset
                }),
            ));
            spans.push(Span::styled(
                " ",
                Style::default().bg(if selected { selected_bg } else { Color::Reset }),
            ));
            spans.push(Span::styled(
                format!("-{removed}"),
                Style::default().fg(Color::Red).bg(if selected {
                    selected_bg
                } else {
                    Color::Reset
                }),
            ));
        }
        Line::from(spans).style(selection)
    }

    /// Reserve space before adding left/right titles so hints never overlap
    /// metadata. Border titles never consume content rows.
    fn panel_footer(
        &self,
        mut block: Block<'static>,
        area: Rect,
        focused: bool,
        metadata: Option<&str>,
    ) -> Block<'static> {
        let width = area.width.saturating_sub(2) as usize;
        if area.height < 3 || width == 0 {
            return block;
        }
        let muted = Style::default()
            .fg(Color::DarkGray)
            .remove_modifier(Modifier::BOLD);
        let metadata = metadata.map(|text| truncate(&format!(" {text} "), width));
        let metadata_width = metadata
            .as_ref()
            .map_or(0, |text| Line::from(text.as_str()).width());
        let available = width.saturating_sub(metadata_width + usize::from(metadata_width > 0));
        let mut hints = if focused {
            styled_key_hints_fitting(
                &self.panel_actions(),
                Color::LightCyan,
                available.saturating_sub(2),
            )
        } else {
            Line::default()
        };
        // The border can be bold; descriptions deliberately remain subdued.
        for span in &mut hints.spans {
            if !span.style.add_modifier.contains(Modifier::BOLD) {
                span.style = muted;
            }
        }
        let mut footer = Line::default();
        if !hints.spans.is_empty() {
            footer.push_span(Span::styled(" ", muted));
            footer.spans.extend(hints.spans);
            footer.push_span(Span::styled(" ", muted));
        }
        if let Some(metadata) = metadata {
            block = block.title_bottom(Line::from(Span::styled(metadata, muted)).right_aligned());
        }
        if footer.spans.is_empty() {
            block
        } else {
            block.title_bottom(footer)
        }
    }

    fn panel_block(&self, title: &str, focused: bool) -> Block<'static> {
        let style = if focused {
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.border_color())
        };
        Block::default()
            .borders(Borders::ALL)
            .border_style(style)
            .title(Span::styled(
                title.to_string(),
                if focused {
                    style
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ))
    }

    fn clamp_scroll(&mut self, height: usize) {
        let total = self.rows.len() + usize::from(!self.local_status.is_empty());
        self.scroll_y = self.scroll_y.min(total.saturating_sub(height.max(1)));
    }

    pub(super) fn render_row<'a>(&'a self, row: &'a GraphRow, width: usize) -> Line<'a> {
        let commit = &self.graph.commits[row.commit_index];
        let mut line = Line::default();
        let mut used = 0usize;
        let lane_start = self.scroll_x.min(self.max_lanes);
        let lane_end = row.cells.len().min(self.max_lanes);
        for ci in lane_start..lane_end {
            let color = PALETTE[row.colors[ci] % PALETTE.len()];
            line.push_span(Span::styled(
                row.cells[ci].char().to_string(),
                Style::default().fg(color),
            ));
            used += 1;
            if ci + 1 < lane_end {
                if matches!(
                    row.cells[ci + 1],
                    layout::CellGlyph::BranchDown
                        | layout::CellGlyph::Horizontal
                        | layout::CellGlyph::MergeRight
                ) {
                    let connector_color = PALETTE[row.colors[ci + 1] % PALETTE.len()];
                    line.push_span(Span::styled("─", Style::default().fg(connector_color)));
                } else {
                    line.push_span(Span::raw(" "));
                }
                used += 1;
            }
        }
        line.push_span(Span::raw(" "));
        used += 1;

        // The color of the branch's own lane in the graph, so each branch label
        // matches its path (HEAD/tags keep their own colors).
        let lane_color = row
            .node_col
            .map(|nc| PALETTE[row.colors[nc] % PALETTE.len()])
            .unwrap_or(Color::White);

        for r in self
            .graph_display_refs
            .get(&commit.id)
            .into_iter()
            .flatten()
        {
            let is_checked_out_branch = r.kind == RefKind::LocalBranch
                && self.graph.head_id.as_deref() == Some(commit.id.as_str())
                && self.graph.head_name.as_deref() == Some(r.name.as_str());
            let (mut text, color) = match r.kind {
                RefKind::Head => ("HEAD".to_string(), Color::Green),
                RefKind::LocalBranch if r.has_remote => {
                    let marker = if is_checked_out_branch {
                        format!("{HEAD_BRANCH_MARKER} ")
                    } else {
                        String::new()
                    };
                    (format!("{marker}{REMOTE_GLYPH} {}", r.name), lane_color)
                }
                RefKind::LocalBranch => {
                    let label = if is_checked_out_branch {
                        format!("{HEAD_BRANCH_MARKER} {}", r.name)
                    } else {
                        r.name.clone()
                    };
                    (label, lane_color)
                }
                RefKind::RemoteBranch => (r.name.clone(), lane_color),
                RefKind::Tag => (format!("{TAG_GLYPH} {}", r.name), Color::Cyan),
            };
            if r.kind == RefKind::LocalBranch {
                if let Some(tracking) = self.tracking_by_branch.get(&r.name) {
                    if tracking.gone {
                        text.push_str(" ✗");
                    }
                    if tracking.ahead > 0 {
                        text.push_str(&format!(" {AHEAD_GLYPH}{}", tracking.ahead));
                    }
                    if tracking.behind > 0 {
                        text.push_str(&format!(" {BEHIND_GLYPH}{}", tracking.behind));
                    }
                }
            }
            let label_budget = width.saturating_sub(used + 13);
            if label_budget < 6 {
                break;
            }
            text = truncate(&text, label_budget.saturating_sub(5));
            let color = presentation::pill_fill_color(color, &self.terminal_palette);
            match self.bg {
                // Rounded caps use the same color as the pill fill.
                Some(bg) => {
                    let text_style = Style::default()
                        .fg(presentation::pill_text_color(
                            color,
                            self.bg,
                            &self.terminal_palette,
                        ))
                        .bg(color);
                    let cap_style = Style::default().fg(color).bg(bg);
                    line.push_span(Span::styled(PILL_LEFT.to_string(), cap_style));
                    line.push_span(Span::styled(format!(" {text} "), text_style));
                    line.push_span(Span::styled(PILL_RIGHT.to_string(), cap_style));
                }
                // Use reverse video when the terminal background is unknown.
                None => {
                    let style = Style::default()
                        .fg(color)
                        .bg(presentation::pill_text_color(
                            color,
                            self.bg,
                            &self.terminal_palette,
                        ))
                        .add_modifier(Modifier::REVERSED);
                    line.push_span(Span::styled(format!(" {text} "), style));
                }
            }
            used += Line::from(text.as_str()).width() + if self.bg.is_some() { 4 } else { 2 };
            line.push_span(Span::raw(" "));
            used += 1;
        }

        // The graph pane keeps the compact identity column visible. When there
        // is room, author/date follow the SHA; in a narrower graph pane they
        // are omitted rather than pushing the SHA off-screen.
        let sha_w = 8usize; // 7 chars + trailing space
        let right = sha_w;
        let summary_max = width.saturating_sub(used).saturating_sub(right + 1);
        let summary = truncate(&commit.summary, summary_max);
        let selected = self
            .selected_graph
            .checked_sub(usize::from(!self.local_status.is_empty()))
            .and_then(|index| self.rows.get(index))
            .is_some_and(|selected| selected.commit_index == row.commit_index);
        let mut summary_line = Line::from(Span::styled(
            summary.clone(),
            if selected {
                self.selection_text_style(self.focus == Focus::Graph)
            } else {
                Style::default()
            },
        ));
        if let Some(query) = self.selected_search_query() {
            highlight_search_matches(std::slice::from_mut(&mut summary_line), query);
        }
        for span in summary_line.spans {
            line.push_span(span);
        }
        used += Line::from(summary.as_str()).width();

        let pad = width.saturating_sub(used).saturating_sub(right).max(1);
        line.push_span(Span::raw(" ".repeat(pad)));

        let sha: String = commit.id.chars().take(7).collect();
        let sha_color = self
            .conflict_commit_color(&commit.id)
            .unwrap_or(Color::DarkGray);
        line.push_span(Span::styled(
            format!("{sha:>7} "),
            Style::default().fg(sha_color),
        ));

        line
    }

    pub(super) fn status_line(&self) -> Line<'_> {
        let branch = self
            .graph
            .head_name
            .clone()
            .unwrap_or_else(|| "(detached)".to_string());
        if let Some(search) = &self.search_popup {
            let mut spans = vec![Span::styled(
                format!("Search /{}", search.query),
                Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD),
            )];
            let summary = if let Some(error) = &search.error {
                format!(" · {}", truncate(error, 48))
            } else if search.searched && search.matches.is_empty() {
                " · 0 matches".to_string()
            } else if search.searched {
                let count = search.matches.len();
                format!(
                    " · {count} match{} · {}/{}",
                    if count == 1 { "" } else { "es" },
                    search.selected + 1,
                    count
                )
            } else {
                " · Enter to search".to_string()
            };
            spans.push(Span::styled(summary, Style::default().fg(Color::White)));
            return Line::from(spans);
        }
        if self.loading {
            let elapsed = self
                .loading_started
                .map_or(Duration::ZERO, |started| started.elapsed());
            let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
                [(elapsed.as_millis() as usize / 100) % 10];
            let progress = if self.graph_page_in_progress || !self.graph_complete {
                let count = self.rows.len();
                format!(
                    "Loading more history… showing {count} commit{}",
                    if count == 1 { "" } else { "s" }
                )
            } else {
                "Loading repository graph…".to_string()
            };
            return Line::from(Span::styled(
                format!("figit | {branch} | {spinner} {progress}"),
                Style::default().fg(Color::LightYellow),
            ));
        }
        if let Some(operation) = &self.remote_operation {
            let elapsed = self
                .remote_started
                .map_or(Duration::ZERO, |started| started.elapsed());
            let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
                [(elapsed.as_millis() as usize / 100) % 10];
            return Line::from(vec![Span::styled(
                format!(
                    "figit | {branch} | {spinner} {} in progress ({:.1}s)",
                    operation.label(),
                    elapsed.as_secs_f32()
                ),
                Style::default().fg(Color::LightYellow),
            )]);
        }
        if self.pending_fixup {
            return Line::from(Span::styled(
                "Choose fixup target · Enter confirm · Esc cancel",
                Style::default().fg(Color::LightYellow),
            ));
        }
        if self.details_loading {
            return Line::from(Span::styled(
                format!("figit | {branch} | Loading selected commit files and diff…"),
                Style::default().fg(Color::LightYellow),
            ));
        }
        if let Some(error) = &self.details_error {
            return Line::from(Span::styled(
                format!(
                    "figit | {branch} | Could not load commit details: {}",
                    truncate(error, 52)
                ),
                Style::default().fg(Color::LightRed),
            ));
        }
        if let Some(error) = &self.diff_error {
            return Line::from(Span::styled(
                format!(
                    "figit | {branch} | Could not load diff: {}",
                    truncate(error, 52)
                ),
                Style::default().fg(Color::LightRed),
            ));
        }
        if let Some((notice, _)) = &self.remote_notice {
            return Line::from(vec![Span::styled(
                format!("figit | {branch} | {notice}"),
                Style::default().fg(Color::LightGreen),
            )]);
        }
        if let Some(error) = self.watch_error.as_ref() {
            return Line::from(vec![Span::styled(
                format!("figit | {branch} | Auto-refresh: {}", truncate(error, 52)),
                Style::default().fg(Color::LightYellow),
            )]);
        }
        if let Some(error) = &self.action_error {
            let controls = if self.rebase_active {
                if self.rebase_edit.is_some() {
                    " • c replace • Ctrl+A abort"
                } else {
                    " • Ctrl+C retry • Ctrl+A abort"
                }
            } else {
                ""
            };
            return Line::from(vec![Span::styled(
                format!("figit | {branch} | {}{controls}", truncate(error, 55)),
                Style::default().fg(Color::LightRed),
            )]);
        }
        if self.rebase_active {
            let message = if self.rebase_edit.is_some() {
                "edit paused — staged changes; c replace • Ctrl+A abort"
            } else if self.rebase_paused {
                "rebase paused — Ctrl+C continue • Ctrl+A abort"
            } else {
                "rebase active — Ctrl+C continue • Ctrl+A abort"
            };
            return Line::from(vec![Span::styled(
                format!("figit | {branch} | {message}"),
                Style::default().fg(Color::LightYellow),
            )]);
        }
        let update_message = match &self.update_status {
            UpdateStatus::Available => Some((
                "Update available • U install • restart afterward".to_string(),
                Color::LightYellow,
            )),
            UpdateStatus::Installing => Some((
                "Installing update… please wait".to_string(),
                Color::LightYellow,
            )),
            UpdateStatus::RestartRequired => Some((
                "Update installed • q quit and relaunch".to_string(),
                Color::LightGreen,
            )),
            UpdateStatus::Current => Some(("Already up to date".to_string(), Color::LightGreen)),
            UpdateStatus::UnsupportedInstall => Some((
                "Auto-update requires an official installer build".to_string(),
                Color::LightYellow,
            )),
            UpdateStatus::Failed(error) => Some((
                format!("Update failed: {}", truncate(error, 58)),
                Color::LightRed,
            )),
            UpdateStatus::Idle | UpdateStatus::Checking => None,
        };
        if let Some((message, color)) = update_message {
            return Line::from(Span::styled(
                format!("figit | {branch} | {message}"),
                Style::default().fg(color),
            ));
        }
        if self.graph_action.is_some()
            || self.commit_popup
            || self.abort_rebase_popup
            || self.rebase_plan.is_some()
            || self.stash_popup.is_some()
            || self.help_popup
        {
            // Popups own the keyboard; don't advertise panel keys behind them.
            return Line::from(Span::styled(
                format!("figit | {branch}"),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if self.watcher_poll_deadline.is_some() {
            return Line::from(Span::styled(
                format!("figit | {branch} | Auto-refresh: polling every 5s"),
                Style::default().fg(Color::DarkGray),
            ));
        }
        self.context_hints(self.viewport_width as usize)
    }
}

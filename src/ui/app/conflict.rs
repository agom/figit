use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ConflictLineKey {
    row: usize,
    side: u8,
    width: usize,
    scroll: usize,
    color: Color,
}

pub(super) struct ConflictSources {
    base_lines: Vec<String>,
    base: String,
    ours: AlignedSide,
    theirs: AlignedSide,
}

impl ConflictSources {
    pub(super) fn new(versions: &crate::backend::ConflictVersions) -> Self {
        let base =
            String::from_utf8_lossy(versions.base.as_deref().unwrap_or_default()).into_owned();
        let ours = align_conflict_side(
            &base,
            &String::from_utf8_lossy(versions.ours.as_deref().unwrap_or_default()),
        );
        let theirs = align_conflict_side(
            &base,
            &String::from_utf8_lossy(versions.theirs.as_deref().unwrap_or_default()),
        );
        Self {
            base_lines: display_lines(&base),
            base,
            ours,
            theirs,
        }
    }

    pub(super) fn rows_for_result(&self, working: &str) -> Vec<ThreeWayRow> {
        let result = align_conflict_side(&self.base, working);
        let mut rows = Vec::new();
        for index in 0..=self.base_lines.len() {
            let count = self.ours.insertions[index]
                .len()
                .max(self.theirs.insertions[index].len())
                .max(result.insertions[index].len());
            for offset in 0..count {
                rows.push(ThreeWayRow {
                    base: None,
                    ours: self.ours.insertions[index].get(offset).cloned(),
                    theirs: self.theirs.insertions[index].get(offset).cloned(),
                    result: result.insertions[index].get(offset).cloned(),
                });
            }
            if let Some(text) = self.base_lines.get(index) {
                rows.push(ThreeWayRow {
                    base: Some(ThreeWayCell {
                        number: index + 1,
                        text: text.clone(),
                    }),
                    ours: self.ours.lines[index].clone(),
                    theirs: self.theirs.lines[index].clone(),
                    result: result.lines[index].clone(),
                });
            }
        }
        rows
    }
}

impl ThreeWayView {
    pub(super) fn new(path: GitPath, versions: crate::backend::ConflictVersions) -> Self {
        let working = versions.working.clone().unwrap_or_default();
        let text = String::from_utf8_lossy(&working);
        let hunks = conflict_hunks(&text);
        let resolves_hunks = !hunks.is_empty() && std::str::from_utf8(&working).is_ok();
        let sources = ConflictSources::new(&versions);
        let rows = std::sync::Arc::new(sources.rows_for_result(&text));
        let hunk_rows = conflict_row_ranges(&rows, &text, &hunks);
        let mut view = Self {
            path,
            ours_missing: versions.ours.is_none(),
            theirs_missing: versions.theirs.is_none(),
            result_missing: versions.working.is_none(),
            versions,
            sources,
            working_rows: rows.clone(),
            rows,
            hunks,
            resolves_hunks,
            working_hunk_rows: hunk_rows.clone(),
            hunk_rows,
            line_cache: Default::default(),
            max_columns: 0,
            number_width: 4,
            viewport_height: 1,
            viewport_width: 1,
            working,
            pending_resolution: None,
            result_preview: false,
            chosen_side: None,
            hunk_index: 0,
            scroll_y: 0,
            scroll_x: 0,
        };
        view.measure_rows();
        view.jump_to_hunk();
        view
    }

    fn measure_rows(&mut self) {
        let mut columns = 0;
        let mut number = 0;
        for row in self.rows.iter() {
            for cell in [&row.base, &row.ours, &row.theirs, &row.result]
                .into_iter()
                .flatten()
            {
                columns = columns.max(Line::from(cell.text.as_str()).width());
                number = number.max(cell.number);
            }
        }
        self.max_columns = columns;
        self.number_width = (number.to_string().len() + 1).max(4);
        self.line_cache.borrow_mut().clear();
    }

    pub(super) fn active_range(&self) -> std::ops::Range<usize> {
        if self.chosen_side == Some(ConflictSide::Base)
            || (self.result_preview && !self.resolves_hunks)
            || self.pending_resolution == Some(ConflictResolution::Delete)
        {
            return 0..self.rows.len();
        }
        self.hunk_rows
            .get(self.hunk_index)
            .cloned()
            .unwrap_or(0..self.rows.len())
    }

    pub(super) fn clamp_scroll(&mut self) {
        self.scroll_y = self
            .scroll_y
            .min(self.rows.len().saturating_sub(self.viewport_height.max(1)));
        self.scroll_x = self
            .scroll_x
            .min(self.max_columns.saturating_sub(self.viewport_width.max(1)));
    }

    pub(super) fn scroll_by(&mut self, delta: isize) {
        self.scroll_y = self.scroll_y.saturating_add_signed(delta);
        self.clamp_scroll();
    }

    pub(super) fn jump_to_hunk(&mut self) {
        self.scroll_y = self.active_range().start.saturating_sub(2);
        self.clamp_scroll();
    }

    pub(super) fn cancel_preview(&mut self) {
        if !self.result_preview && self.pending_resolution.is_none() {
            return;
        }
        self.pending_resolution = None;
        self.result_preview = false;
        self.chosen_side = None;
        self.rows = self.working_rows.clone();
        self.hunk_rows = self.working_hunk_rows.clone();
        self.measure_rows();
        self.jump_to_hunk();
    }

    pub(super) fn preview(&mut self, resolution: ConflictResolution, side: ConflictSide) {
        let rows = self.sources.rows_for_result(&String::from_utf8_lossy(
            resolution.content().unwrap_or_default(),
        ));
        // Each pane uses this same row grid. Map the original conflict regions
        // through stable source line numbers when the preview removes markers.
        let mut source_rows = HashMap::new();
        for (index, row) in rows.iter().enumerate() {
            for (side, cell) in [&row.base, &row.ours, &row.theirs].into_iter().enumerate() {
                if let Some(cell) = cell {
                    source_rows.insert((side, cell.number), index);
                }
            }
        }
        self.hunk_rows = self
            .working_hunk_rows
            .iter()
            .map(|range| {
                let mut first = usize::MAX;
                let mut last = 0;
                for row in &self.working_rows[range.clone()] {
                    for (side, cell) in [&row.base, &row.ours, &row.theirs].into_iter().enumerate()
                    {
                        if let Some(index) = cell
                            .as_ref()
                            .and_then(|cell| source_rows.get(&(side, cell.number)))
                        {
                            first = first.min(*index);
                            last = last.max(*index + 1);
                        }
                    }
                }
                if first == usize::MAX {
                    let start = range.start.min(rows.len().saturating_sub(1));
                    start..(start + 1).min(rows.len())
                } else {
                    first..last
                }
            })
            .collect();
        self.rows = std::sync::Arc::new(rows);
        self.pending_resolution = Some(resolution);
        self.result_preview = true;
        self.chosen_side = Some(side);
        self.measure_rows();
        self.jump_to_hunk();
    }

    fn line(&self, row: usize, side: u8, width: usize, color: Color) -> Line<'static> {
        let key = ConflictLineKey {
            row,
            side,
            width,
            scroll: self.scroll_x,
            color,
        };
        if let Some(line) = self.line_cache.borrow().get(&key) {
            return line.clone();
        }
        let cells = &self.rows[row];
        let cell = match side {
            0 => cells.ours.as_ref(),
            1 => cells.theirs.as_ref(),
            _ => cells.result.as_ref(),
        };
        let line = three_way_column(
            cell,
            cells.base.as_ref(),
            self.path.display().as_ref(),
            width,
            self.scroll_x,
            color,
            self.number_width,
        );
        let mut cache = self.line_cache.borrow_mut();
        // Cache a small working set, not the complete syntax-highlighted file.
        if cache.len() >= self.viewport_height.saturating_mul(6).max(512) {
            cache.clear();
        }
        cache.insert(key, line.clone());
        line
    }
}

fn conflict_row_ranges(
    rows: &[ThreeWayRow],
    text: &str,
    hunks: &[ConflictHunk],
) -> Vec<std::ops::Range<usize>> {
    let starts = std::iter::once(0)
        .chain(
            text.match_indices('\n')
                .map(|(index, _)| index + 1)
                .filter(|&index| index < text.len()),
        )
        .collect::<Vec<_>>();
    let mut line_rows = vec![0; starts.len() + 1];
    for (index, row) in rows.iter().enumerate() {
        if let Some(cell) = &row.result {
            line_rows[cell.number] = index;
        }
    }
    hunks
        .iter()
        .map(|hunk| {
            let start = starts.partition_point(|&offset| offset < hunk.start) + 1;
            let end = starts.partition_point(|&offset| offset < hunk.end);
            line_rows[start]..line_rows[end] + 1
        })
        .collect()
}

impl App {
    pub(super) fn render_three_way_view(&mut self, frame: &mut Frame, area: Rect) {
        let (ours_color, theirs_color) = self
            .conflict_colors()
            .unwrap_or((Color::LightBlue, Color::LightYellow));
        let palette = self.terminal_palette;
        let active_style = self.selection_style(self.focus == Focus::Diff);
        let no_color = self.no_color;
        let resolver_focused = self.focus == Focus::Diff
            || (self.focus == Focus::Status
                && self
                    .status
                    .get(self.selected_status)
                    .is_some_and(StatusEntry::is_conflict));
        let source_label = |id: &str| {
            let branch = self
                .graph
                .refs
                .iter()
                .filter(|reference| {
                    reference.id == id
                        && matches!(reference.kind, RefKind::LocalBranch | RefKind::RemoteBranch)
                })
                .min_by_key(|reference| {
                    (
                        reference.kind != RefKind::LocalBranch,
                        reference.name.as_str(),
                    )
                });
            let short = id.chars().take(7).collect::<String>();
            branch.map_or(short.clone(), |reference| {
                format!("{} {short}", reference.name)
            })
        };
        let ids = self
            .conflict_commits
            .as_ref()
            .map(|commits| (source_label(&commits.ours), source_label(&commits.theirs)));
        let Some(view) = self.three_way_view.as_mut() else {
            return;
        };
        if area.width < 24 || area.height < 10 {
            frame.render_widget(
                Paragraph::new("Resolver needs more space · z maximize").wrap(Wrap { trim: false }),
                area,
            );
            return;
        }
        let layout = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);
        let scope = if view.hunks.is_empty() {
            "Whole-file conflict".to_string()
        } else {
            format!("Hunk {}/{}", view.hunk_index + 1, view.hunks.len())
        };
        let prefix = if area.width < 40 {
            "Merge — "
        } else {
            "Three-way merge — "
        };
        let available =
            (area.width as usize).saturating_sub(Line::from(prefix).width() + scope.len() + 3);
        let title = format!(
            "{prefix}{} · {scope}",
            truncate_start(view.path.display().as_ref(), available)
        );
        frame.render_widget(
            Paragraph::new(truncate(&title, area.width as usize)).style(
                Style::default()
                    .fg(Color::Rgb(255, 105, 180))
                    .add_modifier(Modifier::BOLD),
            ),
            layout[0],
        );
        let sections =
            Layout::vertical([Constraint::Percentage(50), Constraint::Min(0)]).split(layout[1]);
        let sides =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Min(0)]).split(sections[0]);
        let interiors = [
            sides[0].inner(Margin {
                horizontal: 1,
                vertical: 1,
            }),
            sides[1].inner(Margin {
                horizontal: 1,
                vertical: 1,
            }),
            sections[1].inner(Margin {
                horizontal: 1,
                vertical: 1,
            }),
        ];
        view.viewport_height = interiors
            .iter()
            .map(|area| area.height as usize)
            .min()
            .unwrap_or(1)
            .max(1);
        view.viewport_width = interiors
            .iter()
            .map(|area| (area.width as usize).saturating_sub(view.number_width))
            .min()
            .unwrap_or(1)
            .max(1);
        view.clamp_scroll();
        let source_title = |name: &str, id: Option<&str>, deleted: bool| {
            format!(
                "{name} {}{}",
                id.unwrap_or(""),
                if deleted { " (deleted)" } else { "" }
            )
        };
        let ours_title = source_title(
            "Ours",
            ids.as_ref().map(|ids| ids.0.as_str()),
            view.ours_missing,
        );
        let theirs_title = source_title(
            "Theirs",
            ids.as_ref().map(|ids| ids.1.as_str()),
            view.theirs_missing,
        );
        let result_title = if view.result_preview {
            if view.pending_resolution == Some(ConflictResolution::Delete) {
                "Delete entire file — preview"
            } else {
                match view.chosen_side {
                    Some(ConflictSide::Ours) => {
                        if !view.resolves_hunks {
                            "Result — Preview Ours (whole file)"
                        } else {
                            "Result — Preview Ours (this conflict)"
                        }
                    }
                    Some(ConflictSide::Theirs) => {
                        if !view.resolves_hunks {
                            "Result — Preview Theirs (whole file)"
                        } else {
                            "Result — Preview Theirs (this conflict)"
                        }
                    }
                    Some(ConflictSide::Base) => "Restore entire file from Base — preview",
                    None => "Result — Preview Edited",
                }
            }
        } else if view.result_missing {
            "Result — deleted"
        } else {
            "Result — unresolved conflicts"
        };
        let titles = [ours_title.as_str(), theirs_title.as_str(), result_title];
        let result_color = match view.chosen_side {
            Some(ConflictSide::Ours) => ours_color,
            Some(ConflictSide::Theirs) => theirs_color,
            _ => Color::White,
        };
        let colors = [ours_color, theirs_color, result_color];
        let areas = [sides[0], sides[1], sections[1]];
        let active = view.active_range();
        for side in 0..3 {
            let chosen = if side == 0 {
                view.chosen_side == Some(ConflictSide::Ours)
            } else {
                view.chosen_side == Some(ConflictSide::Theirs)
            };
            let border = if side < 2 {
                conflict_side_border_style(colors[side], chosen, &palette)
            } else {
                Style::default().fg(Color::LightCyan)
            };
            let mut lines = Vec::new();
            for index in view.scroll_y
                ..(view.scroll_y + interiors[side].height as usize).min(view.rows.len())
            {
                let row = &view.rows[index];
                let cell = match side {
                    0 => row.ours.as_ref(),
                    1 => row.theirs.as_ref(),
                    _ => row.result.as_ref(),
                };
                let changed = cell.is_some_and(|cell| {
                    row.base.as_ref().is_none_or(|base| base.text != cell.text)
                });
                let mut line = view.line(
                    index,
                    side as u8,
                    interiors[side].width as usize,
                    colors[side],
                );
                if active.contains(&index) {
                    line.style = if changed && (side < 2 || view.result_preview) {
                        Style::default().bg(darker_color(colors[side], &palette))
                    } else {
                        active_style
                    };
                    if no_color {
                        line.style = line
                            .style
                            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
                    }
                }
                pad_colored_lines(
                    std::slice::from_mut(&mut line),
                    interiors[side].width as usize,
                );
                lines.push(line);
            }
            frame.render_widget(
                Paragraph::new(lines).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(Span::styled(
                            truncate(titles[side], areas[side].width.saturating_sub(2) as usize),
                            Style::default().fg(colors[side]),
                        ))
                        .border_style(border),
                ),
                areas[side],
            );
            if side == 2 && view.pending_resolution == Some(ConflictResolution::Delete) {
                frame.render_widget(
                    Paragraph::new("File will be deleted when you confirm")
                        .style(Style::default().fg(Color::LightRed)),
                    interiors[side],
                );
            }
        }
        let scope = if !view.resolves_hunks {
            "preview file"
        } else {
            "preview conflict"
        };
        if !resolver_focused {
            frame.render_widget(
                Paragraph::new(styled_key_hints_fitting(
                    &[("Tab", "change panel"), (":", "panel commands")],
                    Color::LightCyan,
                    area.width as usize,
                )),
                layout[2],
            );
            return;
        }
        let mut actions = Vec::new();
        if view.result_preview {
            actions.extend([("Enter", "confirm"), ("Esc", "cancel preview")]);
        }
        actions.push(("o/t", scope));
        if !view.hunks.is_empty() {
            actions.push(("[/]", "conflict"));
        }
        actions.extend([
            ("b", "restore whole file"),
            ("e", "editor"),
            (":", "commands"),
        ]);
        frame.render_widget(
            Paragraph::new(styled_key_hints_fitting(
                &actions,
                Color::LightCyan,
                area.width as usize,
            )),
            layout[2],
        );
        if view.rows.len() > view.viewport_height {
            let mut state = ScrollbarState::new(view.rows.len())
                .position(view.scroll_y)
                .viewport_content_length(view.viewport_height);
            frame.render_stateful_widget(
                minimal_scrollbar(Color::DarkGray),
                border_scrollbar_area(sections[1]),
                &mut state,
            );
        }
    }
}

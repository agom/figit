use super::*;

impl App {
    pub(super) fn light_theme(&self) -> bool {
        matches!(self.bg, Some(Color::Rgb(r, g, b)) if
            u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114 > 150_000)
    }

    pub(super) fn border_color(&self) -> Color {
        if self.light_theme() {
            Color::Rgb(190, 198, 207)
        } else {
            Color::Rgb(66, 74, 88)
        }
    }

    pub(super) fn button_style(&self, selected: bool) -> Style {
        if selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightCyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        }
    }

    pub(super) fn selection_style(&self, active: bool) -> Style {
        let background = match (self.light_theme(), active) {
            (false, true) => Color::Rgb(48, 54, 68),
            (true, true) => Color::Rgb(228, 232, 237),
            (false, false) => Color::Rgb(30, 34, 44),
            (true, false) => Color::Rgb(235, 238, 241),
        };
        Style::default().bg(background)
    }

    pub(super) fn selection_text_style(&self, active: bool) -> Style {
        let mut style = Style::default();
        if active || self.no_color {
            style = style.add_modifier(Modifier::BOLD);
        }
        if active && self.no_color {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        style
    }

    /// Preserve semantic foregrounds and pill fills while highlighting the row.
    pub(super) fn style_graph_selection(&self, line: &mut Line<'_>, active: bool) {
        let selection = self.selection_style(active);
        line.style = line.style.patch(selection);
        for span in &mut line.spans {
            if span.style.add_modifier.contains(Modifier::REVERSED) {
                // Fallback pills explicitly set both reverse-video colors.
                continue;
            } else if matches!(span.content.as_ref(), "\u{e0b6}" | "\u{e0b4}")
                || span.content.trim().is_empty()
                || span.style.bg.is_none_or(|color| color == Color::Reset)
            {
                span.style = span.style.patch(selection);
            }
            // Pill interiors and search-match backgrounds retain their own fill.
        }
    }

    /// The two lists share their geometry with mouse hit-testing. Empty lists
    /// retain a compact, focusable indicator without moving the diff boundary.
    pub(super) fn staging_panes(&self, area: Rect) -> [Rect; 2] {
        if self.maximized && self.focus == Focus::Status && self.historical_commit.is_none() {
            return if self.status_section == StatusSection::Unstaged {
                [area, Rect::default()]
            } else {
                [Rect::default(), area]
            };
        }
        let vertical = self.graph_visible && self.three_way_view.is_none();
        let empty = [
            self.unstaged_indices.is_empty(),
            self.staged_indices.is_empty(),
        ];
        let constraints = if empty[0] == empty[1] {
            [Constraint::Percentage(50), Constraint::Min(0)]
        } else {
            let compact = if vertical {
                3
            } else {
                (area.width / 5).clamp(16, 22).min(area.width / 2)
            };
            if empty[0] {
                [Constraint::Length(compact), Constraint::Min(0)]
            } else {
                [Constraint::Min(0), Constraint::Length(compact)]
            }
        };
        let panes = if vertical {
            Layout::vertical(constraints).split(area)
        } else {
            Layout::horizontal(constraints).split(area)
        };
        [panes[0], panes[1]]
    }

    pub(super) fn cycle_panel_focus(&mut self, forward: bool) {
        let mut panels = Vec::new();
        if self.graph_visible {
            panels.push((Focus::Status, self.status_section));
            panels.push((Focus::Graph, self.status_section));
            if self.historical_commit.is_some() && self.three_way_view.is_none() {
                panels.push((Focus::Details, self.status_section));
            }
        } else if self.historical_commit.is_some() {
            panels.push((Focus::Status, StatusSection::Unstaged));
            if self.three_way_view.is_none() {
                panels.push((Focus::Details, StatusSection::Unstaged));
            }
        } else {
            if !self.unstaged_indices.is_empty() || self.staged_indices.is_empty() {
                panels.push((Focus::Status, StatusSection::Unstaged));
            }
            if !self.staged_indices.is_empty() {
                panels.push((Focus::Status, StatusSection::Staged));
            }
        }
        panels.push((Focus::Diff, self.status_section));
        let current = panels
            .iter()
            .position(|(focus, section)| {
                *focus == self.focus && (*focus != Focus::Status || *section == self.status_section)
            })
            .unwrap_or(panels.len() - 1);
        let next = if forward {
            (current + 1) % panels.len()
        } else {
            (current + panels.len() - 1) % panels.len()
        };
        let (focus, section) = panels[next];
        let section_changed = self.status_section != section;
        self.focus = focus;
        if focus == Focus::Status {
            self.status_section = section;
            self.ensure_status_selection();
            if section_changed {
                self.refresh_diff();
            }
            // A resolver refresh may prefer Diff; Tab's explicit focus wins.
            self.focus = focus;
        }
    }

    pub(super) fn repository_header(&self, width: usize) -> Line<'static> {
        let repository = self
            .repository_path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "figit".into());
        let branch = self.graph.head_name.as_deref().unwrap_or("detached HEAD");
        let tracking = self
            .tracking_by_branch
            .get(branch)
            .map(|tracking| {
                if tracking.gone {
                    " · upstream gone".to_string()
                } else if tracking.ahead != 0 || tracking.behind != 0 {
                    format!(
                        " {AHEAD_GLYPH}{} {BEHIND_GLYPH}{}",
                        tracking.ahead, tracking.behind
                    )
                } else {
                    String::new()
                }
            })
            .unwrap_or_default();
        let conflicts = self.active_conflict_paths.len();
        let state = if conflicts > 0 {
            format!(
                " · {conflicts} conflict{} remaining",
                if conflicts == 1 { "" } else { "s" }
            )
        } else if self.rebase_active {
            " · Rebase paused".into()
        } else if self.local_status.is_empty() {
            " · Working tree clean".into()
        } else {
            format!(" · {} changed", self.local_status.len())
        };
        let count = format!(
            "{}{} commit{}",
            self.rows.len(),
            if self.graph_complete { "" } else { "+" },
            if self.rows.len() == 1 { "" } else { "s" }
        );
        let identity = format!(" {repository} / {branch}{tracking}");
        let suffix = if width >= 100 {
            format!("{state} · {count}")
        } else {
            state
        };
        let available = width.saturating_sub(Line::from(suffix.as_str()).width());
        Line::from(vec![
            Span::styled(
                truncate(&identity, available),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                suffix,
                Style::default().fg(if conflicts > 0 || self.rebase_active {
                    Color::LightYellow
                } else {
                    Color::DarkGray
                }),
            ),
        ])
    }

    /// Contextual actions for the focused pane. Keep this free of repository
    /// reads: rendering must only consult already loaded state.
    pub(super) fn panel_actions(&self) -> Vec<(&'static str, &'static str)> {
        if !self.panel_hints_visible || !self.modal_closed() || self.loading {
            return Vec::new();
        }
        let mut actions = Vec::new();
        let has_hunks = !self.hunk_header_indices.is_empty();
        let has_changes = self
            .diff_lines
            .iter()
            .any(|line| matches!(line.kind, DiffKind::Added | DiffKind::Removed));
        let selected_file = self.status.get(self.selected_status).filter(|_| {
            self.section_indices(self.status_section)
                .contains(&self.selected_status)
        });
        let conflict =
            self.three_way_view.is_some() || selected_file.is_some_and(StatusEntry::is_conflict);
        match self.focus {
            Focus::Graph => {
                if self.pending_fixup && self.selected_commit_id().is_some() {
                    actions.push(("Enter", "fixup"));
                } else {
                    let available = self.panel_commands();
                    for (id, label) in [
                        (CommandId::Fetch, "fetch"),
                        (CommandId::Graph(GraphAction::Pull), "pull"),
                        (CommandId::Graph(GraphAction::CheckoutBranch), "checkout"),
                        (CommandId::Search, "search"),
                    ] {
                        let command =
                            available
                                .iter()
                                .find(|command| command.id == id)
                                .or_else(|| {
                                    (id == CommandId::Graph(GraphAction::CheckoutBranch))
                                        .then(|| {
                                            available.iter().find(|command| {
                                                command.id
                                                    == CommandId::Graph(GraphAction::Checkout)
                                            })
                                        })
                                        .flatten()
                                });
                        if let Some(command) = command {
                            actions.push((command.key, label));
                        }
                    }
                }
            }
            Focus::Details => {}
            Focus::Status if self.historical_commit.is_some() => {}
            Focus::Status if conflict => {
                if self.remote_operation.is_none() {
                    actions.extend([
                        ("o/t", "preview side"),
                        ("b", "restore whole file"),
                        ("e", "editor"),
                    ]);
                }
            }
            Focus::Status if self.remote_operation.is_none() => {
                if selected_file.is_some() {
                    let staged = self.status_section == StatusSection::Staged;
                    // Space applies the partial selection while hunk/line mode
                    // is retained. F always applies whole files in this case.
                    let key = if self.hunk_mode || self.line_mode {
                        "F"
                    } else {
                        "Space"
                    };
                    actions.push((key, if staged { "unstage file" } else { "stage file" }));
                    if staged && self.commit_actions_available() {
                        actions.push(("c", "commit"));
                    } else if !staged && !self.selected_statuses.is_empty() {
                        actions.push(("x", "discard file"));
                    }
                    if has_hunks {
                        actions.push(("h", "hunks"));
                    }
                }
            }
            Focus::Diff if !conflict && self.diff_path.is_some() && !self.details_loading => {
                if self.remote_operation.is_none() {
                    if self.diff_truncated.is_some() {
                        actions.push(("L", "load full diff"));
                    }
                    if self.historical_commit.is_none()
                        && selected_file.is_some()
                        && ((!self.hunk_mode && !self.line_mode)
                            || (self.hunk_mode && has_hunks)
                            || (self.line_mode && self.selected_diff_line.is_some()))
                    {
                        let staged = self.diff_target == DiffTarget::Index;
                        let scope = match (staged, self.hunk_mode, self.line_mode) {
                            (true, _, true) => "unstage line",
                            (false, _, true) => "stage line",
                            (true, true, _) => "unstage hunk",
                            (false, true, _) => "stage hunk",
                            (true, _, _) => "unstage file",
                            _ => "stage file",
                        };
                        actions.push(("Space", scope));
                    }
                }
                if has_hunks {
                    actions.push(("[/]", "hunk"));
                }
                if self.remote_operation.is_none() {
                    if self.historical_commit.is_some() {
                        if !self.line_mode {
                            actions.push(("v", "view"));
                        }
                    } else if self.hunk_mode && has_changes {
                        actions.push(("l", "lines"));
                    } else if self.line_mode {
                        actions.push(("f", "file mode"));
                    } else if has_hunks {
                        actions.push(("h", "hunks"));
                    } else if !self.line_mode {
                        actions.push(("v", "view"));
                    }
                }
            }
            _ => {}
        }
        actions.truncate(if self.focus == Focus::Graph { 4 } else { 3 });
        actions
    }

    pub(super) fn context_hints(&self, width: usize) -> Line<'static> {
        styled_key_hints_fitting(
            &[
                ("Tab", "focus"),
                (":", "Commands"),
                ("?", "help"),
                ("g", "graph"),
                (
                    "z",
                    if self.maximized {
                        "restore"
                    } else {
                        "maximize"
                    },
                ),
                ("H", "hints"),
                ("q", "quit"),
            ],
            Color::LightCyan,
            width,
        )
    }

    /// Keep syntax colors and semantic accents readable against a light
    /// terminal as well as the default dark diff surfaces. Apply once per frame
    /// so popups and source highlighting use the same contrast policy.
    pub(super) fn apply_terminal_theme(&self, buffer: &mut ratatui::buffer::Buffer) {
        let light = self.light_theme();
        for position in buffer.area.positions() {
            let cell = buffer.cell_mut(position).expect("position inside buffer");
            if self.no_color {
                // Suppress color changes before the backend writes them. Empty
                // ANSI color sequences otherwise reset bold in NO_COLOR mode.
                cell.fg = Color::Reset;
                cell.bg = Color::Reset;
                continue;
            }
            // Cap glyphs are colored surfaces, not text: keep their fill equal
            // to the pill interior in both themes.
            if matches!(cell.symbol(), "\u{e0b6}" | "\u{e0b4}") {
                if light {
                    cell.bg = light_surface(cell.bg);
                    cell.fg = light_surface(cell.fg);
                }
                continue;
            }
            if light {
                // A pill may still have a dark fill in a light terminal. Its
                // explicitly contrasted foreground must not be darkened like
                // a syntax token on the terminal's default white background.
                let contrasted = matches!(
                    cell.fg,
                    Color::Rgb(235, 239, 247) | Color::Rgb(255, 255, 255)
                ) && cell.bg != Color::Reset
                    && cell.fg == pill_text_color(cell.bg, self.bg, &self.terminal_palette);
                cell.bg = light_surface(cell.bg);
                if contrasted {
                    continue;
                }
                cell.fg = match cell.fg {
                    Color::White | Color::Gray => Color::Rgb(35, 42, 52),
                    Color::DarkGray => Color::Rgb(93, 104, 118),
                    Color::LightCyan => Color::Rgb(0, 99, 125),
                    Color::LightBlue => Color::Rgb(40, 79, 158),
                    Color::LightGreen | Color::Green => Color::Rgb(28, 110, 52),
                    Color::LightRed | Color::Red => Color::Rgb(170, 43, 55),
                    Color::LightYellow | Color::Yellow => Color::Rgb(131, 89, 0),
                    Color::LightMagenta | Color::Magenta => Color::Rgb(137, 55, 142),
                    other => other,
                };
                if let Color::Rgb(r, g, b) = cell.fg {
                    // Scale bright syntax tokens down, preserving their hue.
                    let brightness =
                        (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000;
                    if brightness > 120 && !cell.modifier.contains(Modifier::REVERSED) {
                        cell.fg = Color::Rgb(
                            (u32::from(r) * 120 / brightness) as u8,
                            (u32::from(g) * 120 / brightness) as u8,
                            (u32::from(b) * 120 / brightness) as u8,
                        );
                    }
                }
            } else if cell.fg == Color::DarkGray {
                cell.fg = Color::Rgb(143, 153, 170);
            }
            if cell.fg == Color::White {
                cell.fg = Color::Reset;
            }
        }
    }

    /// Nerd Font glyph and color for the file type at `path`.
    pub(super) fn file_icon(&self, path: &str) -> (char, Color) {
        let theme = if self.light_theme() {
            devicons::Theme::Light
        } else {
            devicons::Theme::Dark
        };
        let icon = devicons::icon_for_file(path, &Some(theme));
        (icon.icon, devicon_color(icon.color))
    }
}

pub(super) fn devicon_color(hex: &str) -> Color {
    let hex = hex.trim_start_matches('#');
    if hex.len() == 6 {
        if let (Ok(red), Ok(green), Ok(blue)) = (
            u8::from_str_radix(&hex[0..2], 16),
            u8::from_str_radix(&hex[2..4], 16),
            u8::from_str_radix(&hex[4..6], 16),
        ) {
            return Color::Rgb(red, green, blue);
        }
    }
    Color::Gray
}

fn light_surface(color: Color) -> Color {
    match color {
        Color::LightBlue | Color::Blue => Color::Rgb(197, 216, 241),
        Color::LightGreen | Color::Green => Color::Rgb(206, 230, 210),
        Color::LightYellow | Color::Yellow => Color::Rgb(241, 226, 183),
        Color::LightRed | Color::Red => Color::Rgb(241, 206, 207),
        Color::LightMagenta | Color::Magenta => Color::Rgb(232, 207, 233),
        Color::LightCyan | Color::Cyan => Color::Rgb(195, 226, 233),
        Color::Rgb(45, 50, 65) => Color::Rgb(210, 226, 235),
        Color::Rgb(r, g, b) if r.max(g).max(b) < 180 => {
            let max = r.max(g).max(b);
            let base = if max >= 75 { 224 } else { 247 };
            Color::Rgb(base - (max - r), base - (max - g), base - (max - b))
        }
        other => other,
    }
}

/// Pick a foreground with at least 4.5:1 contrast against the displayed pill
/// fill, including terminal palette colors and the light-theme transformation.
pub(super) fn pill_text_color(
    fill: Color,
    terminal_bg: Option<Color>,
    palette: &TerminalAnsiPalette,
) -> Color {
    let light = matches!(terminal_bg, Some(Color::Rgb(r, g, b)) if
        u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114 > 150_000);
    let fill = if light { light_surface(fill) } else { fill };
    let luminance = |[r, g, b]: [u8; 3]| {
        let linear = |channel: u8| {
            let value = f64::from(channel) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    };
    let background = luminance(pill_rgb(fill, palette));
    let contrast = |rgb| {
        let foreground = luminance(rgb);
        (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
    };
    let dark = [24, 28, 36];
    let bright = [235, 239, 247];
    let mut chosen = if contrast(dark) >= contrast(bright) {
        dark
    } else {
        bright
    };
    if contrast(chosen) < 4.5 {
        chosen = if contrast([0, 0, 0]) >= contrast([255, 255, 255]) {
            [0, 0, 0]
        } else {
            [255, 255, 255]
        };
    }
    Color::Rgb(chosen[0], chosen[1], chosen[2])
}

pub(super) fn pill_fill_color(color: Color, palette: &TerminalAnsiPalette) -> Color {
    if matches!(color, Color::Rgb(..))
        || matches!(
            palette.get(pill_color_index(color) as usize),
            Some(Some(Color::Rgb(..)))
        )
    {
        return color;
    }
    // If a terminal cannot report its ANSI palette, lock the fallback fill to
    // RGB too so its foreground contrast does not depend on an unknown theme.
    let [r, g, b] = pill_rgb(color, palette);
    Color::Rgb(r, g, b)
}

fn pill_color_index(color: Color) -> u8 {
    match color {
        Color::Rgb(..) => 0,
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
        Color::Indexed(index) => index,
        Color::Reset => 0,
    }
}

fn pill_rgb(color: Color, palette: &TerminalAnsiPalette) -> [u8; 3] {
    if let Color::Rgb(r, g, b) = color {
        return [r, g, b];
    }
    let index = pill_color_index(color);
    if let Some(Some(Color::Rgb(r, g, b))) = palette.get(index as usize) {
        return [*r, *g, *b];
    }
    const ANSI: [[u8; 3]; 16] = [
        [0, 0, 0],
        [205, 0, 0],
        [0, 205, 0],
        [205, 205, 0],
        [0, 0, 238],
        [205, 0, 205],
        [0, 205, 205],
        [229, 229, 229],
        [127, 127, 127],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [92, 92, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];
    match index {
        0..=15 => ANSI[index as usize],
        16..=231 => {
            let value = index - 16;
            let channel = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            [
                channel(value / 36),
                channel((value / 6) % 6),
                channel(value % 6),
            ]
        }
        _ => [8 + (index - 232) * 10; 3],
    }
}

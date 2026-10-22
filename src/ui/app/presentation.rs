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
        if active {
            Style::default()
                .bg(if self.light_theme() {
                    Color::Rgb(210, 226, 235)
                } else {
                    Color::Rgb(45, 50, 65)
                })
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(if self.light_theme() {
                Color::Rgb(235, 238, 241)
            } else {
                Color::Rgb(30, 34, 44)
            })
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
        let empty = [
            self.unstaged_indices.is_empty(),
            self.staged_indices.is_empty(),
        ];
        let constraints = if empty[0] == empty[1] {
            [Constraint::Percentage(50), Constraint::Min(0)]
        } else {
            let compact = if self.graph_visible {
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
        let panes = if self.graph_visible {
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
                    format!(" ↑{} ↓{}", tracking.ahead, tracking.behind)
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

    pub(super) fn context_hints(&self, width: usize) -> Line<'static> {
        let mut actions = match self.focus {
            Focus::Graph => vec![
                ("Enter", "checkout"),
                ("/", "search"),
                (
                    "p",
                    if self.selected_current_branch() {
                        "pull"
                    } else {
                        "pick commit"
                    },
                ),
            ],
            Focus::Details => vec![("j/k", "scroll message")],
            Focus::Status | Focus::Diff
                if self.three_way_view.is_some()
                    || self
                        .status
                        .get(self.selected_status)
                        .is_some_and(StatusEntry::is_conflict) =>
            {
                vec![
                    ("o/t", "preview side"),
                    ("b", "whole Base"),
                    ("e", "editor"),
                ]
            }
            Focus::Status | Focus::Diff if self.historical_commit.is_some() => {
                vec![("[/]", "hunk"), ("v", "diff mode")]
            }
            Focus::Status | Focus::Diff => {
                let staged = self.status_section == StatusSection::Staged;
                let scope = match (staged, self.hunk_mode, self.line_mode) {
                    (true, _, true) => "unstage line",
                    (false, _, true) => "stage line",
                    (true, true, _) => "unstage hunk",
                    (false, true, _) => "stage hunk",
                    (true, _, _) => "unstage file",
                    _ => "stage file",
                };
                let mut items = vec![("Space", scope)];
                if self.hunk_mode
                    || (self.focus == Focus::Diff && !self.hunk_header_indices.is_empty())
                {
                    items.push(("[/]", "hunk"));
                }
                if staged && self.focus == Focus::Status {
                    items.push(("c", "commit"));
                } else if !staged {
                    items.push(("x", "discard file"));
                }
                items.push(("h/l", "hunk/line"));
                items
            }
            Focus::Commit => vec![("Enter", "confirm"), ("Esc", "cancel")],
        };
        // Essential navigation stays visible even in a narrow terminal.
        let mut tail = vec![
            ("Tab", "focus"),
            ("g", "graph"),
            (
                "z",
                if self.maximized {
                    "restore"
                } else {
                    "maximize"
                },
            ),
            ("?", "help"),
        ];
        if width >= 100 {
            tail.push(("q", "quit"));
        }
        let tail_width = styled_key_hints(&tail, Color::LightCyan).width();
        let available = width.saturating_sub(tail_width + 3);
        while !actions.is_empty()
            && styled_key_hints(&actions, Color::LightCyan).width() > available
        {
            actions.pop();
        }
        actions.extend(tail);
        styled_key_hints_fitting(&actions, Color::LightCyan, width)
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
            if light {
                cell.bg = light_surface(cell.bg);
                // The cap glyph is part of the colored surface, not text.
                if matches!(cell.symbol(), "\u{e0b6}" | "\u{e0b4}") {
                    cell.fg = light_surface(cell.fg);
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

    /// Nerd Font glyph and color for the file type at `path`. Uses full-size
    /// Font Awesome file glyphs (the `devicons` set has some compact designs)
    /// with the per-type color from `devicons`.
    pub(super) fn file_icon(&self, path: &str) -> (char, Color) {
        let theme = if self.light_theme() {
            devicons::Theme::Light
        } else {
            devicons::Theme::Dark
        };
        let color = devicon_color(devicons::icon_for_file(path, &Some(theme)).color);
        (file_type_glyph(path), color)
    }
}

/// Full-size Font Awesome file glyphs, chosen by file name or extension.
pub(super) fn file_type_glyph(path: &str) -> char {
    const FILE: char = '\u{f15b}'; // nf-fa-file
    const FILE_TEXT: char = '\u{f15c}'; // nf-fa-file_text
    const FILE_CODE: char = '\u{f1c9}'; // nf-fa-file_code_o
    const FILE_CSV: char = '\u{eefc}'; // nf-fa-file_csv
    const FILE_IMAGE: char = '\u{f1c5}'; // nf-fa-file_picture_o
    const FILE_PDF: char = '\u{f1c1}'; // nf-fa-file_pdf_o
    const FILE_ARCHIVE: char = '\u{f1c6}'; // nf-fa-file_zipper
    const FILE_AUDIO: char = '\u{f1c7}'; // nf-fa-file_sound_o
    const FILE_VIDEO: char = '\u{f1c8}'; // nf-fa-file_video_o
    const FILE_WORD: char = '\u{f1c2}'; // nf-fa-file_word_o
    const FILE_EXCEL: char = '\u{f1c3}'; // nf-fa-file_excel_o
    const FILE_SLIDES: char = '\u{f1c4}'; // nf-fa-file_powerpoint_o
    const TERMINAL: char = '\u{f120}'; // nf-fa-terminal
    const LOCK: char = '\u{f023}'; // nf-fa-lock
    const GIT: char = '\u{f1d3}'; // nf-fa-git

    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    match name.as_str() {
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitconfig" => return GIT,
        "license" | "licence" | "copying" | "notice" => return FILE_TEXT,
        "makefile" | "gnumakefile" | "dockerfile" | "containerfile" | "justfile" => {
            return FILE_CODE;
        }
        _ => {}
    }

    let ext = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    match ext {
        "md" | "markdown" | "rst" | "adoc" | "org" | "txt" | "text" | "log" => FILE_TEXT,
        "csv" | "tsv" => FILE_CSV,
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "svg" | "webp" | "ico" | "tif" | "tiff"
        | "heic" | "avif" => FILE_IMAGE,
        "pdf" => FILE_PDF,
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "jar" | "war" => {
            FILE_ARCHIVE
        }
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "opus" => FILE_AUDIO,
        "mp4" | "mkv" | "mov" | "avi" | "webm" | "flv" | "wmv" => FILE_VIDEO,
        "doc" | "docx" | "odt" | "rtf" => FILE_WORD,
        "xls" | "xlsx" | "ods" => FILE_EXCEL,
        "ppt" | "pptx" | "odp" => FILE_SLIDES,
        "lock" => LOCK,
        "sh" | "bash" | "zsh" | "fish" => TERMINAL,
        _ if is_code_extension(ext) => FILE_CODE,
        _ => FILE,
    }
}

fn is_code_extension(ext: &str) -> bool {
    matches!(
        ext,
        "rs" | "py"
            | "pyi"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "ts"
            | "tsx"
            | "go"
            | "java"
            | "kt"
            | "kts"
            | "rb"
            | "php"
            | "swift"
            | "c"
            | "h"
            | "cc"
            | "cpp"
            | "cxx"
            | "hpp"
            | "hh"
            | "hxx"
            | "cs"
            | "scala"
            | "lua"
            | "pl"
            | "pm"
            | "r"
            | "dart"
            | "ex"
            | "exs"
            | "erl"
            | "hrl"
            | "hs"
            | "lhs"
            | "clj"
            | "cljs"
            | "vim"
            | "el"
            | "lisp"
            | "scm"
            | "ps1"
            | "bat"
            | "cmd"
            | "sql"
            | "graphql"
            | "gql"
            | "proto"
            | "vue"
            | "svelte"
            | "astro"
            | "zig"
            | "nim"
            | "cr"
            | "ml"
            | "mli"
            | "fs"
            | "fsx"
            | "json"
            | "jsonc"
            | "yaml"
            | "yml"
            | "toml"
            | "ini"
            | "cfg"
            | "conf"
            | "env"
            | "properties"
            | "xml"
    )
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

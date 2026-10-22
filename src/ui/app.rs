use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use axoupdater::AxoUpdater;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::execute;
use notify::{Config as NotifyConfig, RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::Frame;
use similar::{ChangeTag, DiffTag, TextDiff};
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style as SynStyle, ThemeSet};
use syntect::parsing::SyntaxSet;

use crate::backend::{
    ConflictCommits, ConflictResolution, ConflictSide, DiffLineSelection, DiffTarget, GitBackend,
    GraphWindow, PathDiffStats, RebaseAction, RebaseOutcome, RebasePlan, RebasePlanBlock,
    RebasePlanEntry, RebaseTodoAction, StashEntry, UndoInfo,
};
use crate::core::model::{
    BranchTracking, Commit, DisplayRef, GitPath, GraphModel, RefKind, StatusEntry,
};
use crate::graph::layout::{self, GraphRow};

const PALETTE: [Color; 7] = [
    Color::LightBlue,
    Color::LightGreen,
    Color::LightYellow,
    Color::LightMagenta,
    Color::LightCyan,
    Color::LightRed,
    Color::White,
];

/// Glyph shown before a local branch that is also tracked on a remote.
/// Nerd Fonts `nf-fa-github` (U+F09B); swap here if your font differs.
const REMOTE_GLYPH: &str = "\u{f09b}";

/// Diffs longer than this are truncated before parsing, which is what keeps
/// very large files from stalling the UI. Press `L` to load the full diff on
/// demand.
const MAX_RENDERED_DIFF_LINES: usize = 20_000;

/// Glyph shown before a tag (Nerd Fonts, U+F04F9).
const TAG_GLYPH: &str = "\u{f04f9}";

/// Ratatui's initializer installs a hook to restore raw mode and the alternate
/// screen. Chain one that also releases the mouse-capture mode enabled below.
fn install_mouse_capture_panic_hook() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        previous_hook(panic_info);
    }));
}

/// Rounded ends for the negative pills (left/right half black circles).
const PILL_LEFT: char = '\u{e0b6}'; // 
const PILL_RIGHT: char = '\u{e0b4}'; // 
const HEAD_BRANCH_MARKER: &str = "✓";

type TerminalAnsiPalette = [Option<Color>; 16];

/// Truncate `s` to at most `max` characters, adding a trailing ellipsis when
/// cut. `max == 0` yields an empty string.
fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    if max == 1 {
        return "…".to_string();
    }
    let keep = max - 1;
    let head: String = s.chars().take(keep).collect();
    format!("{head}…")
}

fn truncate_start(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    if max == 1 {
        return "…".to_string();
    }
    let tail: String = s
        .chars()
        .rev()
        .take(max - 1)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("…{tail}")
}

fn single_line_cursor(
    text: &str,
    prefix_width: usize,
    cursor_index: usize,
    area: Rect,
) -> (Position, u16) {
    let width = usize::from(area.width).max(1);
    let cursor_text: String = text.chars().take(cursor_index).collect();
    let cursor_offset = prefix_width + Line::from(cursor_text).width();
    let scroll = cursor_offset.saturating_sub(width.saturating_sub(1));
    let column = cursor_offset
        .saturating_sub(scroll)
        .min(width.saturating_sub(1));
    (
        Position {
            x: area.x.saturating_add(column as u16),
            y: area.y,
        },
        scroll.min(u16::MAX as usize) as u16,
    )
}

/// One visual line of wrapped editor text. `start`/`end` are character
/// indices; a line that ends at an explicit newline includes the newline index
/// in `ends_with_newline` so the cursor can sit at the end of the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WrappedLine {
    start: usize,
    end: usize,
    ends_with_newline: bool,
}

fn char_width(character: char) -> usize {
    Line::from(character.to_string()).width().max(1)
}

/// Wrap `text` to `width` display columns, preferring spaces but preserving
/// every character (including the spaces at the break) so rendering and cursor
/// math agree.
fn wrap_text(text: &str, width: usize) -> Vec<WrappedLine> {
    let chars: Vec<char> = text.chars().collect();
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut logical_start = 0;
    loop {
        let mut logical_end = logical_start;
        while logical_end < chars.len() && chars[logical_end] != '\n' {
            logical_end += 1;
        }
        let has_newline = logical_end < chars.len();
        wrap_logical_line(
            &chars,
            logical_start,
            logical_end,
            has_newline,
            width,
            &mut lines,
        );
        if !has_newline {
            break;
        }
        logical_start = logical_end + 1;
        if logical_start == chars.len() {
            lines.push(WrappedLine {
                start: logical_start,
                end: logical_start,
                ends_with_newline: false,
            });
            break;
        }
    }
    lines
}

fn wrap_logical_line(
    chars: &[char],
    start: usize,
    end: usize,
    has_newline: bool,
    width: usize,
    lines: &mut Vec<WrappedLine>,
) {
    if start == end {
        lines.push(WrappedLine {
            start,
            end,
            ends_with_newline: has_newline,
        });
        return;
    }

    let mut segment_start = start;
    let mut column = 0;
    let mut last_space = None;
    let mut index = start;
    while index < end {
        let width_char = char_width(chars[index]);
        if column + width_char > width && index > segment_start {
            let break_at = last_space
                .filter(|&space| space + 1 > segment_start)
                .map_or(index, |space| space + 1);
            lines.push(WrappedLine {
                start: segment_start,
                end: break_at,
                ends_with_newline: false,
            });
            segment_start = break_at;
            index = segment_start;
            column = 0;
            last_space = None;
            continue;
        }
        column += width_char;
        if chars[index] == ' ' {
            last_space = Some(index);
        }
        index += 1;
    }

    lines.push(WrappedLine {
        start: segment_start,
        end,
        ends_with_newline: has_newline,
    });
}

fn wrapped_cursor(lines: &[WrappedLine], chars: &[char], cursor: usize) -> (usize, usize) {
    let cursor = cursor.min(chars.len());
    for (row, line) in lines.iter().enumerate() {
        let is_last = row + 1 == lines.len();
        let contains = if line.ends_with_newline || is_last {
            cursor >= line.start && cursor <= line.end
        } else {
            cursor >= line.start && cursor < line.end
        };
        if contains {
            let column = chars[line.start..cursor.min(line.end)]
                .iter()
                .map(|&character| char_width(character))
                .sum();
            return (row, column);
        }
    }
    (lines.len().saturating_sub(1), 0)
}

fn wrapped_text_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    let chars: Vec<char> = text.chars().collect();
    wrap_text(text, width)
        .into_iter()
        .map(|line| Line::from(chars[line.start..line.end].iter().collect::<String>()))
        .collect()
}

fn wrapped_text_cursor(text: &str, cursor_index: usize, area: Rect) -> (Position, u16) {
    let width = usize::from(area.width).max(1);
    let chars: Vec<char> = text.chars().collect();
    let lines = wrap_text(text, width);
    let (row, column) = wrapped_cursor(&lines, &chars, cursor_index);

    let visible_height = usize::from(area.height).max(1);
    let scroll = row.saturating_sub(visible_height - 1);
    let x = area
        .x
        .saturating_add(column.min(width.saturating_sub(1)) as u16);
    let y = area.y.saturating_add(row.saturating_sub(scroll) as u16);
    (
        Position {
            x: x.min(area.x.saturating_add(area.width.saturating_sub(1))),
            y: y.min(area.y.saturating_add(area.height.saturating_sub(1))),
        },
        scroll.min(u16::MAX as usize) as u16,
    )
}

fn insert_at_cursor(text: &mut String, cursor: &mut usize, character: char) {
    let byte = text
        .char_indices()
        .nth(*cursor)
        .map_or(text.len(), |(byte, _)| byte);
    text.insert(byte, character);
    *cursor += 1;
}

fn backspace_at_cursor(text: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let start = text
        .char_indices()
        .nth(*cursor - 1)
        .map_or(text.len(), |(byte, _)| byte);
    let end = text
        .char_indices()
        .nth(*cursor)
        .map_or(text.len(), |(byte, _)| byte);
    text.replace_range(start..end, "");
    *cursor -= 1;
}

fn delete_at_cursor(text: &mut String, cursor: usize) {
    let Some((start, _)) = text.char_indices().nth(cursor) else {
        return;
    };
    let end = text
        .char_indices()
        .nth(cursor + 1)
        .map_or(text.len(), |(byte, _)| byte);
    text.replace_range(start..end, "");
}

fn move_cursor_vertical(text: &str, cursor: &mut usize, delta: i32) {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut start = 0usize;
    let mut current_line = 0usize;
    let mut current_column = 0usize;
    for (index, line) in lines.iter().enumerate() {
        let end = start + line.chars().count();
        if *cursor <= end || index + 1 == lines.len() {
            current_line = index;
            current_column = cursor.saturating_sub(start).min(end - start);
            break;
        }
        start = end + 1;
    }
    let target_line = if delta < 0 {
        current_line.saturating_sub(delta.unsigned_abs() as usize)
    } else {
        (current_line + delta as usize).min(lines.len().saturating_sub(1))
    };
    let target_start: usize = lines
        .iter()
        .take(target_line)
        .map(|line| line.chars().count() + 1)
        .sum();
    *cursor = target_start + current_column.min(lines[target_line].chars().count());
}

fn move_cursor_to_line_edge(text: &str, cursor: &mut usize, end: bool) {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut start = 0usize;
    for (index, line) in lines.iter().enumerate() {
        let line_end = start + line.chars().count();
        if *cursor <= line_end || index + 1 == lines.len() {
            *cursor = if end { line_end } else { start };
            return;
        }
        start = line_end + 1;
    }
}

/// Format a unix timestamp in the commit's own timezone as `YYYY-MM-DD`, using
/// Howard Hinnant's civil-from-days algorithm (no chrono dependency).
fn fmt_date(seconds: i64, offset_seconds: i32) -> String {
    let shifted = seconds + i64::from(offset_seconds);
    let z = shifted.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn status_glyph(code: char) -> (char, Color) {
    match code {
        'A' => ('✚', Color::Green),
        'D' => ('✖', Color::Red),
        'M' => ('●', Color::Yellow),
        'R' => ('➜', Color::Magenta),
        'C' => ('▣', Color::Magenta),
        'T' => ('⇄', Color::Cyan),
        '?' => ('?', Color::Yellow),
        'U' => ('⚠', Color::Red),
        _ => ('•', Color::Yellow),
    }
}

fn darker_color(color: Color, palette: &TerminalAnsiPalette) -> Color {
    let index = match color {
        Color::Black => Some(0),
        Color::Red | Color::LightRed => Some(1),
        Color::Green | Color::LightGreen => Some(2),
        Color::Yellow | Color::LightYellow => Some(3),
        Color::Blue | Color::LightBlue => Some(4),
        Color::Magenta | Color::LightMagenta => Some(5),
        Color::Cyan | Color::LightCyan => Some(6),
        Color::Gray | Color::White => Some(7),
        _ => None,
    };
    let theme_index = match color {
        Color::LightRed => Some(9),
        Color::LightGreen => Some(10),
        Color::LightYellow => Some(11),
        Color::LightBlue => Some(12),
        Color::LightMagenta => Some(13),
        Color::LightCyan => Some(14),
        Color::White => Some(15),
        _ => index,
    };
    let themed = theme_index.and_then(|index| palette[index]);
    match themed {
        Some(Color::Rgb(r, g, b)) => Color::Rgb(r / 2, g / 2, b / 2),
        Some(color) => color,
        None => match color {
            Color::LightRed => Color::Red,
            Color::LightGreen => Color::Green,
            Color::LightYellow => Color::Yellow,
            Color::LightBlue => Color::Blue,
            Color::LightCyan => Color::Cyan,
            Color::LightMagenta => Color::Magenta,
            Color::White => Color::Gray,
            Color::Rgb(r, g, b) => Color::Rgb(r / 2, g / 2, b / 2),
            color => color,
        },
    }
}

fn conflict_side_border_style(
    color: Color,
    selected: bool,
    palette: &TerminalAnsiPalette,
) -> Style {
    let color = if selected {
        color
    } else {
        darker_color(color, palette)
    };
    let style = Style::default().fg(color);
    if selected {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

fn dim_graph_line(
    line: &mut Line<'_>,
    lane_start: usize,
    lane_columns: usize,
    terminal_bg: Option<Color>,
    terminal_palette: &TerminalAnsiPalette,
) {
    for (index, span) in line.spans.iter_mut().enumerate() {
        if (lane_start..lane_start + lane_columns).contains(&index) {
            if let Some(foreground) = span.style.fg {
                span.style = span.style.fg(darker_color(foreground, terminal_palette));
            }
        } else if let Some(background) = span.style.bg {
            if terminal_bg == Some(background) {
                if let Some(foreground) = span.style.fg {
                    span.style = span.style.fg(darker_color(foreground, terminal_palette));
                }
            } else {
                span.style = span.style.bg(darker_color(background, terminal_palette));
                if let Some(foreground) = span.style.fg {
                    if terminal_bg == Some(foreground) {
                        span.style = span.style.fg(darker_color(foreground, terminal_palette));
                    }
                }
            }
        } else if span.style.add_modifier.contains(Modifier::REVERSED) {
            if let Some(foreground) = span.style.fg {
                span.style = span.style.fg(darker_color(foreground, terminal_palette));
            }
        } else {
            span.style = span.style.fg(Color::DarkGray);
        }
    }
}

fn scrollbar_position(scroll: usize, content: usize, viewport: usize) -> usize {
    if content == 0 || scroll == 0 {
        0
    } else {
        (scroll + viewport.saturating_sub(1)).min(content.saturating_sub(1))
    }
}

fn current_branch_ids(graph: &GraphModel) -> HashSet<String> {
    let by_id: HashMap<&str, &crate::core::model::Commit> = graph
        .commits
        .iter()
        .map(|commit| (commit.id.as_str(), commit))
        .collect();
    let mut ids = HashSet::new();
    let mut pending = graph.head_id.iter().cloned().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        if !ids.insert(id.clone()) {
            continue;
        }
        if let Some(commit) = by_id.get(id.as_str()) {
            pending.extend(commit.parents.iter().cloned());
        }
    }
    ids
}

fn split_commit_message(commit: &Commit) -> (String, String) {
    (
        commit.summary.clone(),
        commit.message.trim_end().to_string(),
    )
}

fn minimal_scrollbar(color: Color) -> Scrollbar<'static> {
    Scrollbar::default()
        .orientation(ScrollbarOrientation::VerticalRight)
        .thumb_symbol("█")
        .track_symbol(None)
        .begin_symbol(None)
        .end_symbol(None)
        .thumb_style(color)
}

fn border_scrollbar_area(area: Rect) -> Rect {
    Rect::new(
        area.x + area.width.saturating_sub(1),
        area.y + 1,
        1,
        area.height.saturating_sub(2),
    )
}

fn parse_diff(diff: &str) -> Vec<DiffLine> {
    let mut old_no = 0u32;
    let mut new_no = 0u32;
    let mut lines = Vec::new();
    for raw in diff.lines() {
        if raw.starts_with("diff ")
            || raw.starts_with("index ")
            || raw.starts_with("+++")
            || raw.starts_with("---")
        {
            continue;
        }
        if raw.starts_with('\\') {
            // `\` marks out-of-band notes (e.g. "No newline at end of file", or
            // our own "file too large" notice). Render them as metadata so they
            // never count as a text hunk.
            lines.push(DiffLine {
                old_no: None,
                new_no: None,
                text: raw.trim_start_matches('\\').trim_start().to_string(),
                kind: DiffKind::Meta,
            });
            continue;
        }
        if raw.starts_with("@@") {
            if let Some((old, new)) = parse_hunk_header(raw) {
                old_no = old;
                new_no = new;
            }
            lines.push(DiffLine {
                old_no: None,
                new_no: None,
                text: raw.to_string(),
                kind: DiffKind::Meta,
            });
        } else if raw.starts_with('+') {
            lines.push(DiffLine {
                old_no: None,
                new_no: Some(new_no),
                text: raw.to_string(),
                kind: DiffKind::Added,
            });
            new_no += 1;
        } else if raw.starts_with('-') {
            lines.push(DiffLine {
                old_no: Some(old_no),
                new_no: None,
                text: raw.to_string(),
                kind: DiffKind::Removed,
            });
            old_no += 1;
        } else {
            lines.push(DiffLine {
                old_no: Some(old_no),
                new_no: Some(new_no),
                text: raw.to_string(),
                kind: DiffKind::Context,
            });
            old_no += 1;
            new_no += 1;
        }
    }
    lines
}

fn diff_hunk_headers(lines: &[DiffLine]) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            (line.kind == DiffKind::Meta && line.text.starts_with("@@")).then_some(index)
        })
        .collect()
}

#[cfg(test)]
fn mark_selected_diff_hunk(lines: &mut [Line<'static>], selected_hunk: usize) {
    let headers = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            line.spans
                .iter()
                .any(|span| span.content.starts_with("@@"))
                .then_some(index)
        })
        .collect::<Vec<_>>();
    let Some(&start) = headers.get(selected_hunk) else {
        return;
    };
    let end = headers
        .get(selected_hunk + 1)
        .copied()
        .unwrap_or(lines.len());
    for line in &mut lines[start..end] {
        emphasize_selected_diff_hunk(line);
    }
}

fn emphasize_selected_diff_hunk(line: &mut Line<'static>) {
    let selection_background = Color::Rgb(45, 50, 65);
    let has_diff_background = line.spans.iter().any(|span| {
        span.style
            .bg
            .is_some_and(|background| background != Color::Reset)
    });
    if !has_diff_background {
        line.style = line.style.bg(selection_background);
    }
    for span in &mut line.spans {
        span.style = span.style.add_modifier(Modifier::BOLD);
        if span.style.bg.is_none() || span.style.bg == Some(Color::Reset) {
            span.style = span.style.bg(selection_background);
        }
    }
}

fn emphasize_selected_diff_line(line: &mut Line<'static>, kind: DiffKind) {
    if !matches!(kind, DiffKind::Added | DiffKind::Removed) {
        return;
    }
    let highlight = bright_diff_background(kind).unwrap_or(Color::Rgb(45, 50, 65));
    line.style = line.style.bg(highlight);
    for span in &mut line.spans {
        span.style = span.style.add_modifier(Modifier::BOLD);
        if span.style.bg.is_none()
            || span.style.bg == Some(Color::Reset)
            || diff_background(kind) == span.style.bg
        {
            span.style = span.style.bg(highlight);
        }
    }
}

fn highlight_search_matches(lines: &mut [Line<'static>], query: &str) {
    let needle = query.to_lowercase().chars().collect::<Vec<_>>();
    if needle.is_empty() {
        return;
    }
    for line in lines {
        let mut rebuilt: Vec<Span<'static>> = Vec::new();
        let mut matched = false;
        for span in &line.spans {
            let chars = span.content.chars().collect::<Vec<_>>();
            let lowered = span.content.to_lowercase().chars().collect::<Vec<_>>();
            if lowered.len() != chars.len() {
                rebuilt.push(span.clone());
                continue;
            }
            let mut cursor = 0;
            let mut segment_start = 0;
            while cursor + needle.len() <= chars.len() {
                if lowered[cursor..cursor + needle.len()] == needle[..] {
                    if segment_start < cursor {
                        rebuilt.push(Span::styled(
                            chars[segment_start..cursor].iter().collect::<String>(),
                            span.style,
                        ));
                    }
                    rebuilt.push(Span::styled(
                        chars[cursor..cursor + needle.len()]
                            .iter()
                            .collect::<String>(),
                        span.style
                            .fg(Color::Black)
                            .bg(Color::LightYellow)
                            .add_modifier(Modifier::BOLD),
                    ));
                    matched = true;
                    cursor += needle.len();
                    segment_start = cursor;
                } else {
                    cursor += 1;
                }
            }
            if segment_start < chars.len() {
                rebuilt.push(Span::styled(
                    chars[segment_start..].iter().collect::<String>(),
                    span.style,
                ));
            }
        }
        if matched {
            line.spans = rebuilt;
        }
    }
}

fn pad_colored_lines<'a>(lines: &mut [Line<'a>], width: usize) {
    for line in lines {
        let background = line
            .style
            .bg
            .filter(|background| *background != Color::Reset)
            .or_else(|| {
                line.spans.iter().find_map(|span| {
                    span.style
                        .bg
                        .filter(|background| *background != Color::Reset)
                })
            });
        let padding = width.saturating_sub(line.width());
        if padding > 0 {
            let style = background
                .map_or_else(Style::default, |background| Style::default().bg(background));
            line.spans.push(Span::styled(" ".repeat(padding), style));
        }
    }
}

fn conflict_result_diff(base: &str, result: &str, result_label: &str) -> Vec<DiffLine> {
    let diff = TextDiff::from_lines(base, result);
    let mut lines = vec![DiffLine {
        old_no: None,
        new_no: None,
        text: format!("Base → {result_label}"),
        kind: DiffKind::Meta,
    }];
    let (mut old_no, mut new_no) = (1, 1);
    for op in diff.ops() {
        for change in diff.iter_changes(op) {
            let text = change.value().strip_suffix('\n').unwrap_or(change.value());
            let line = match change.tag() {
                ChangeTag::Equal => {
                    let line = DiffLine {
                        old_no: Some(old_no),
                        new_no: Some(new_no),
                        text: format!(" {text}"),
                        kind: DiffKind::Context,
                    };
                    old_no += 1;
                    new_no += 1;
                    line
                }
                ChangeTag::Delete => {
                    let line = DiffLine {
                        old_no: Some(old_no),
                        new_no: None,
                        text: format!("-{text}"),
                        kind: DiffKind::Removed,
                    };
                    old_no += 1;
                    line
                }
                ChangeTag::Insert => {
                    let line = DiffLine {
                        old_no: None,
                        new_no: Some(new_no),
                        text: format!("+{text}"),
                        kind: DiffKind::Added,
                    };
                    new_no += 1;
                    line
                }
            };
            lines.push(line);
        }
    }
    lines
}

fn render_conflict_side_diff(
    lines: &[DiffLine],
    path: &str,
    number_width: usize,
    width: usize,
    mode: DiffMode,
    side_color: Color,
    terminal_palette: &TerminalAnsiPalette,
) -> Vec<Line<'static>> {
    let side_background = darker_color(side_color, terminal_palette);
    let mut rendered = render_diff(lines, mode, width, path, number_width, 0, true)
        .into_iter()
        .map(|mut line| {
            for span in &mut line.spans {
                if span.style.fg == Some(Color::LightGreen) {
                    span.style = span.style.fg(side_color);
                }
                if matches!(
                    span.style.bg,
                    Some(Color::Rgb(38, 56, 43) | Color::Rgb(54, 91, 60))
                ) {
                    span.style = span.style.bg(side_background);
                }
            }
            line
        })
        .collect::<Vec<_>>();
    pad_colored_lines(&mut rendered, width);
    rendered
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let mut parts = line.split_whitespace();
    parts.next()?;
    let old = parts
        .next()?
        .strip_prefix('-')?
        .split(',')
        .next()?
        .parse()
        .ok()?;
    let new = parts
        .next()?
        .strip_prefix('+')?
        .split(',')
        .next()?
        .parse()
        .ok()?;
    Some((old, new))
}

fn diff_color(kind: DiffKind) -> Color {
    match kind {
        DiffKind::Meta => Color::LightCyan,
        DiffKind::Context => Color::Gray,
        DiffKind::Added => Color::LightGreen,
        DiffKind::Removed => Color::LightRed,
    }
}

fn diff_background(kind: DiffKind) -> Option<Color> {
    match kind {
        DiffKind::Added => Some(Color::Rgb(38, 56, 43)),
        DiffKind::Removed => Some(Color::Rgb(70, 44, 49)),
        _ => None,
    }
}

fn bright_diff_background(kind: DiffKind) -> Option<Color> {
    match kind {
        DiffKind::Added => Some(Color::Rgb(54, 91, 60)),
        DiffKind::Removed => Some(Color::Rgb(107, 60, 66)),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum ConflictDiffSection {
    Header,
    BaseOurs,
    BaseTheirs,
    Compare,
    Working,
    Combined,
}

/// Conflict colors follow the two commit lanes, not normal added/removed colors.
fn render_conflict_diff(
    lines: &[DiffLine],
    path: &str,
    number_width: usize,
    ours: Color,
    theirs: Color,
    highlight: bool,
) -> Vec<Line<'static>> {
    let mut section = ConflictDiffSection::Header;
    let mut marker_side = None;
    lines
        .iter()
        .map(|line| {
            let text = line.text.as_str();
            if text.starts_with("Diff: base → ours") {
                section = ConflictDiffSection::BaseOurs;
            } else if text.starts_with("Diff: base → theirs") {
                section = ConflictDiffSection::BaseTheirs;
            } else if text.starts_with("Diff: ours → theirs") {
                section = ConflictDiffSection::Compare;
            } else if text.starts_with("Working file with conflict markers:") {
                section = ConflictDiffSection::Working;
                marker_side = None;
            } else if text.starts_with("Combined conflict diff:") {
                section = ConflictDiffSection::Combined;
            }
            let color = match section {
                ConflictDiffSection::BaseOurs => Some(ours),
                ConflictDiffSection::BaseTheirs => Some(theirs),
                ConflictDiffSection::Compare => match line.kind {
                    DiffKind::Removed => Some(ours),
                    DiffKind::Added => Some(theirs),
                    _ => None,
                },
                ConflictDiffSection::Working => {
                    if text.starts_with("<<<<<<<") {
                        marker_side = Some(ours);
                    } else if text.starts_with("=======") {
                        marker_side = None;
                    } else if text.starts_with(">>>>>>>") {
                        marker_side = Some(theirs);
                    }
                    let color = marker_side;
                    if text.starts_with(">>>>>>>") {
                        marker_side = None;
                    } else if text.starts_with("=======") {
                        marker_side = Some(theirs);
                    }
                    color
                }
                ConflictDiffSection::Header | ConflictDiffSection::Combined => None,
            };
            let mut rendered = render_unified_line(line, path, None, number_width, 0, highlight);
            for span in rendered.spans.iter_mut().skip(1) {
                span.style = span.style.fg(color.unwrap_or(Color::Gray)).bg(Color::Reset);
            }
            if let Some(numbers) = rendered.spans.first_mut() {
                numbers.style = numbers.style.bg(Color::Reset);
            }
            rendered
        })
        .collect()
}

/// Finds the next unpaired added line inside the same removed/added block, so
/// replacements pair up by position even when earlier edits shifted the old and
/// new line numbers apart. Dissimilar pairs are dropped later by the inline
/// emphasis similarity check.
fn next_pairable_added(
    lines: &[DiffLine],
    start: usize,
    paired_added: &HashSet<usize>,
) -> Option<usize> {
    ((start + 1)..lines.len())
        .take_while(|&j| matches!(lines[j].kind, DiffKind::Removed | DiffKind::Added))
        .find(|&j| lines[j].kind == DiffKind::Added && !paired_added.contains(&j))
}

/// One rendered diff row: a unified line or a side-by-side pair. The layout is
/// built cheaply (no syntax highlighting); each row is materialized separately
/// so large diffs only highlight the few rows that are actually on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffRow {
    Unified {
        index: usize,
        /// The paired removed/added line, for intra-line emphasis.
        partner: Option<usize>,
    },
    Side {
        old: Option<usize>,
        new: Option<usize>,
    },
}

impl DiffRow {
    fn input_indices(self) -> (Option<usize>, Option<usize>) {
        match self {
            DiffRow::Unified { index, .. } => (Some(index), None),
            DiffRow::Side { old, new } => (old, new),
        }
    }
}

/// Lays a diff out into rows without doing any syntax highlighting.
fn diff_layout(lines: &[DiffLine], mode: DiffMode) -> Vec<DiffRow> {
    if mode == DiffMode::Unified {
        // Keep the diff's original order: removed lines stay together and
        // added lines stay together. Pairing is only used to emphasize
        // intra-line changes when the lines actually correspond; reordering
        // unrelated lines makes large rewrites look scrambled.
        let partners = unified_partners(lines);
        return lines
            .iter()
            .enumerate()
            .map(|(index, _)| DiffRow::Unified {
                index,
                partner: partners.get(&index).copied(),
            })
            .collect();
    }

    let mut rows = Vec::new();
    let mut paired_added = HashSet::new();
    let mut i = 0;
    while i < lines.len() {
        if paired_added.contains(&i) {
            i += 1;
            continue;
        }
        if lines[i].kind == DiffKind::Meta {
            rows.push(DiffRow::Side {
                old: Some(i),
                new: None,
            });
            i += 1;
            continue;
        }
        if lines[i].kind == DiffKind::Removed {
            if let Some(j) = next_pairable_added(lines, i, &paired_added) {
                rows.push(DiffRow::Side {
                    old: Some(i),
                    new: Some(j),
                });
                paired_added.insert(j);
                i += 1;
                continue;
            }
        }
        rows.push(match lines[i].kind {
            DiffKind::Added => DiffRow::Side {
                old: None,
                new: Some(i),
            },
            DiffKind::Removed => DiffRow::Side {
                old: Some(i),
                new: None,
            },
            _ => DiffRow::Side {
                old: Some(i),
                new: Some(i),
            },
        });
        i += 1;
    }
    rows
}

/// Positional removed/added partners within each contiguous change block, used
/// for intra-line emphasis in the unified view.
fn unified_partners(lines: &[DiffLine]) -> HashMap<usize, usize> {
    let mut partners = HashMap::new();
    let mut paired_added = HashSet::new();
    for (index, line) in lines.iter().enumerate() {
        if line.kind != DiffKind::Removed {
            continue;
        }
        if let Some(added) = next_pairable_added(lines, index, &paired_added) {
            partners.insert(index, added);
            partners.insert(added, index);
            paired_added.insert(added);
        }
    }
    partners
}

fn render_diff(
    lines: &[DiffLine],
    mode: DiffMode,
    width: usize,
    path: &str,
    number_width: usize,
    offset_x: usize,
    highlight: bool,
) -> Vec<Line<'static>> {
    diff_layout(lines, mode)
        .into_iter()
        .map(|row| render_diff_row(lines, row, width, path, number_width, offset_x, highlight))
        .collect()
}

/// Materializes a single diff row, syntax-highlighting its text.
fn render_diff_row(
    lines: &[DiffLine],
    row: DiffRow,
    width: usize,
    path: &str,
    number_width: usize,
    offset_x: usize,
    highlight: bool,
) -> Line<'static> {
    match row {
        DiffRow::Unified { index, partner } => {
            let inline = partner.map(|partner| inline_emphasis_ranges(lines, index, partner));
            render_unified_line(
                &lines[index],
                path,
                inline.as_deref(),
                number_width,
                offset_x,
                highlight,
            )
        }
        DiffRow::Side { old, new } => {
            if let Some(old) = old {
                if lines[old].kind == DiffKind::Meta {
                    return Line::from(Span::styled(
                        skip_columns(&lines[old].text, offset_x).to_string(),
                        Style::default().fg(diff_color(DiffKind::Meta)),
                    ));
                }
            }
            let half = width.saturating_sub(3) / 2;
            render_side_pair(
                old.map(|index| &lines[index]),
                new.map(|index| &lines[index]),
                half,
                number_width,
                offset_x,
                path,
                highlight,
            )
        }
    }
}

/// The intra-line emphasis ranges for line `index`, paired with `partner`.
fn inline_emphasis_ranges(lines: &[DiffLine], index: usize, partner: usize) -> ChangeRanges {
    let (removed, added) = if lines[index].kind == DiffKind::Removed {
        (index, partner)
    } else {
        (partner, index)
    };
    let old = lines[removed]
        .text
        .strip_prefix('-')
        .unwrap_or(&lines[removed].text);
    let new = lines[added]
        .text
        .strip_prefix('+')
        .unwrap_or(&lines[added].text);
    let (old_ranges, new_ranges) = inline_change_ranges(old, new);
    if lines[index].kind == DiffKind::Removed {
        old_ranges
    } else {
        new_ranges
    }
}

/// Drops the first `columns` display columns from `text`.
fn skip_columns(text: &str, columns: usize) -> &str {
    if columns == 0 {
        return text;
    }
    match text.char_indices().nth(columns) {
        Some((byte, _)) => &text[byte..],
        None => "",
    }
}

fn render_side_pair(
    old: Option<&DiffLine>,
    new: Option<&DiffLine>,
    width: usize,
    number_width: usize,
    offset_x: usize,
    path: &str,
    highlight: bool,
) -> Line<'static> {
    let mut spans = side_spans(old, true, width, number_width, offset_x, path, highlight);
    spans.push(Span::raw(" │ "));
    spans.extend(side_spans(
        new,
        false,
        width,
        number_width,
        offset_x,
        path,
        highlight,
    ));
    Line::from(spans)
}

fn side_spans(
    line: Option<&DiffLine>,
    old_side: bool,
    width: usize,
    number_width: usize,
    offset_x: usize,
    path: &str,
    highlight: bool,
) -> Vec<Span<'static>> {
    let Some(line) = line else {
        return vec![Span::raw(" ".repeat(width))];
    };
    let number = if old_side { line.old_no } else { line.new_no };
    let number_text = number.map_or_else(
        || " ".repeat(number_width),
        |n| format!("{n:>width$}", width = number_width),
    );
    let number_color = match line.kind {
        DiffKind::Added | DiffKind::Removed => Color::White,
        _ => Color::DarkGray,
    };
    let marker = match line.kind {
        DiffKind::Added if !old_side => '+',
        DiffKind::Removed if old_side => '-',
        _ => ' ',
    };
    let content = truncate(
        skip_columns(line.text.get(1..).unwrap_or(&line.text), offset_x),
        width.saturating_sub(number_width + 1),
    );
    let mut spans = vec![Span::styled(number_text, Style::default().fg(number_color))];
    let mut marker_style = Style::default().fg(diff_color(line.kind));
    if let Some(bg) = diff_background(line.kind) {
        marker_style = marker_style.bg(bg);
    }
    spans.push(Span::styled(marker.to_string(), marker_style));
    spans.extend(highlight_code(&content, path, line.kind, highlight));
    let used = number_width + 1 + content.chars().count();
    if used < width {
        let mut pad_style = Style::default();
        if let Some(bg) = diff_background(line.kind) {
            pad_style = pad_style.bg(bg);
        }
        spans.push(Span::styled(" ".repeat(width - used), pad_style));
    }
    spans
}

fn render_unified_line(
    line: &DiffLine,
    path: &str,
    inline: Option<&[(usize, usize)]>,
    number_width: usize,
    offset_x: usize,
    highlight: bool,
) -> Line<'static> {
    let numbers = match (line.old_no, line.new_no) {
        (Some(old), Some(new)) => format!("{old:>width$} {new:>width$} ", width = number_width),
        (Some(old), None) => format!("{old:>width$} {:>width$} ", "", width = number_width),
        (None, Some(new)) => format!("{:>width$} {new:>width$} ", "", width = number_width),
        (None, None) => " ".repeat(number_width * 2 + 2),
    };
    let background = diff_background(line.kind);
    let number_color = match line.kind {
        DiffKind::Added | DiffKind::Removed => Color::White,
        _ => Color::DarkGray,
    };
    let mut number_style = Style::default().fg(number_color);
    if let Some(bg) = background {
        number_style = number_style.bg(bg);
    }
    let mut rendered = vec![Span::styled(numbers, number_style)];
    if line.kind == DiffKind::Meta {
        rendered.push(Span::styled(
            skip_columns(&line.text, offset_x).to_string(),
            Style::default().fg(diff_color(line.kind)),
        ));
        return Line::from(rendered);
    }
    let prefix = line.text.chars().next().unwrap_or(' ');
    let mut prefix_style = Style::default().fg(diff_color(line.kind));
    if let Some(bg) = background {
        prefix_style = prefix_style.bg(bg);
    }
    rendered.push(Span::styled(prefix.to_string(), prefix_style));
    let full_code = line.text.get(prefix.len_utf8()..).unwrap_or_default();
    let code = skip_columns(full_code, offset_x);
    let dropped = full_code.len() - code.len();
    if let Some(ranges) = inline {
        let mut cursor = 0;
        for &(start, end) in ranges {
            if end <= dropped {
                continue;
            }
            let start = start.saturating_sub(dropped).min(code.len());
            let end = (end - dropped).min(code.len());
            if start >= end {
                continue;
            }
            rendered.extend(highlight_code(
                &code[cursor..start],
                path,
                line.kind,
                highlight,
            ));
            let mut style = Style::default().fg(diff_color(line.kind));
            if let Some(bg) = bright_diff_background(line.kind) {
                style = style.bg(bg);
            }
            rendered.push(Span::styled(code[start..end].to_string(), style));
            cursor = end;
        }
        rendered.extend(highlight_code(&code[cursor..], path, line.kind, highlight));
    } else {
        rendered.extend(highlight_code(code, path, line.kind, highlight));
    }
    Line::from(rendered)
}

type ChangeRanges = Vec<(usize, usize)>;

/// Paired removed/added lines that differ this much are left unemphasized.
/// Highlighting unrelated lines token by token is just noise.
const INLINE_EMPHASIS_MIN_RATIO: f32 = 0.4;

fn inline_change_ranges(old: &str, new: &str) -> (ChangeRanges, ChangeRanges) {
    if TextDiff::from_chars(old, new).ratio() < INLINE_EMPHASIS_MIN_RATIO {
        return (ChangeRanges::new(), ChangeRanges::new());
    }
    let old_tokens = inline_tokens(old);
    let new_tokens = inline_tokens(new);
    let diff = TextDiff::from_slices(&old_tokens, &new_tokens);
    let mut old_changed = vec![false; old_tokens.len()];
    let mut new_changed = vec![false; new_tokens.len()];
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {}
            ChangeTag::Delete => {
                if let Some(index) = change.old_index() {
                    old_changed[index] = true;
                }
            }
            ChangeTag::Insert => {
                if let Some(index) = change.new_index() {
                    new_changed[index] = true;
                }
            }
        }
    }
    (
        token_ranges(&old_tokens, &old_changed),
        token_ranges(&new_tokens, &new_changed),
    )
}

/// Splits a line into word, whitespace and punctuation tokens so intra-line
/// emphasis lands on whole tokens instead of individual characters.
fn inline_tokens(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut iter = text.char_indices().peekable();
    while let Some((start, first)) = iter.next() {
        let class = inline_token_class(first);
        let mut end = start + first.len_utf8();
        while let Some(&(next_start, next)) = iter.peek() {
            if inline_token_class(next) != class {
                break;
            }
            iter.next();
            end = next_start + next.len_utf8();
        }
        tokens.push(&text[start..end]);
    }
    tokens
}

fn inline_token_class(ch: char) -> u8 {
    if ch.is_alphanumeric() || ch == '_' {
        0
    } else if ch.is_whitespace() {
        1
    } else {
        2
    }
}

fn token_ranges(tokens: &[&str], changed: &[bool]) -> ChangeRanges {
    let mut ranges = Vec::new();
    let mut byte = 0;
    let mut start = None;
    for (token, &is_changed) in tokens.iter().zip(changed) {
        if is_changed && start.is_none() {
            start = Some(byte);
        }
        if !is_changed {
            if let Some(begin) = start.take() {
                ranges.push((begin, byte));
            }
        }
        byte += token.len();
    }
    if let Some(begin) = start {
        ranges.push((begin, byte));
    }
    ranges
}

fn syntax_highlight(code: &str, path: &str, fallback: Color) -> Vec<Span<'static>> {
    static SYNTAX: OnceLock<SyntaxSet> = OnceLock::new();
    static THEMES: OnceLock<ThemeSet> = OnceLock::new();
    let syntax = SYNTAX.get_or_init(SyntaxSet::load_defaults_newlines);
    let themes = THEMES.get_or_init(ThemeSet::load_defaults);
    let Some(syntax_ref) = syntax.find_syntax_for_file(path).ok().flatten() else {
        return vec![Span::styled(
            code.to_string(),
            Style::default().fg(fallback),
        )];
    };
    let theme = themes
        .themes
        .get("base16-ocean.dark")
        .or_else(|| themes.themes.values().next());
    let Some(theme) = theme else {
        return vec![Span::styled(
            code.to_string(),
            Style::default().fg(fallback),
        )];
    };
    let mut highlighter = HighlightLines::new(syntax_ref, theme);
    let Ok(ranges) = highlighter.highlight_line(code, syntax) else {
        return vec![Span::styled(
            code.to_string(),
            Style::default().fg(fallback),
        )];
    };
    ranges
        .into_iter()
        .map(|(style, text)| Span::styled(text.to_string(), Style::default().fg(syn_color(style))))
        .collect()
}

fn highlight_code(code: &str, path: &str, kind: DiffKind, highlight: bool) -> Vec<Span<'static>> {
    let background = diff_background(kind);
    let spans = if highlight {
        syntax_highlight(code, path, diff_color(kind))
    } else {
        vec![Span::styled(
            code.to_string(),
            Style::default().fg(diff_color(kind)),
        )]
    };
    spans
        .into_iter()
        .map(|mut span| {
            if let Some(background) = background {
                span.style = span.style.bg(background);
            }
            span
        })
        .collect()
}

fn syn_color(style: SynStyle) -> Color {
    Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b)
}

struct GraphHintBuilder {
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    current_width: usize,
    width: usize,
    focused: bool,
}

impl GraphHintBuilder {
    fn append_group(&mut self, title: &str, color: Color, actions: &[(&str, &str)]) {
        let category_color = if self.focused { color } else { Color::DarkGray };
        let description_color = if self.focused {
            Color::White
        } else {
            Color::DarkGray
        };
        for (index, (key, description)) in actions.iter().enumerate() {
            let mut heading = if index == 0 {
                format!("{title}  ")
            } else {
                String::new()
            };
            let item_width = key.chars().count() + 1 + description.chars().count();
            let separator_width = usize::from(!self.current.is_empty()) * 3;
            if !self.current.is_empty()
                && self.current_width + separator_width + heading.chars().count() + item_width
                    > self.width
            {
                self.lines
                    .push(Line::from(std::mem::take(&mut self.current)));
                self.current_width = 0;
            }
            if self.current.is_empty()
                && !heading.is_empty()
                && heading.chars().count() + item_width > self.width
            {
                self.lines.push(Line::from(Span::styled(
                    title.to_string(),
                    Style::default()
                        .fg(category_color)
                        .add_modifier(Modifier::BOLD),
                )));
                heading.clear();
            }
            if !self.current.is_empty() {
                self.current
                    .push(Span::styled(" • ", Style::default().fg(description_color)));
                self.current_width += 3;
            }
            if !heading.is_empty() {
                self.current.push(Span::styled(
                    heading.clone(),
                    Style::default()
                        .fg(category_color)
                        .add_modifier(Modifier::BOLD),
                ));
                self.current_width += heading.chars().count();
            }
            self.current.push(Span::styled(
                key.to_string(),
                Style::default()
                    .fg(category_color)
                    .add_modifier(Modifier::BOLD),
            ));
            self.current.push(Span::styled(
                format!(" {description}"),
                Style::default().fg(description_color),
            ));
            self.current_width += key.chars().count() + 1 + description.chars().count();
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        if !self.current.is_empty() {
            self.lines.push(Line::from(self.current));
        }
        self.lines
    }
}

fn graph_hint_lines(width: usize, focused: bool, on_current_branch: bool) -> Vec<Line<'static>> {
    let mut builder = GraphHintBuilder {
        lines: Vec::new(),
        current: Vec::new(),
        current_width: 0,
        width,
        focused,
    };
    builder.append_group(
        "Commit",
        Color::LightMagenta,
        &[
            ("c", "checkout"),
            // `p` cherry-picks anywhere except on the checked-out branch tip,
            // where it pulls instead.
            ("p", if on_current_branch { "pull" } else { "pick" }),
            ("x", "revert"),
            ("u", "undo"),
            ("/", "search"),
            ("w", "reword"),
            ("e", "edit"),
            ("i", "rebase"),
            ("R", "onto"),
        ],
    );
    builder.append_group(
        "Branch",
        Color::LightGreen,
        &[
            ("Enter", "checkout"),
            ("b", "branch"),
            ("t", "tag"),
            ("r", "reset"),
            ("n", "rename"),
            ("d", "delete"),
            ("F", "fetch"),
            ("+", "push"),
        ],
    );
    builder.append_group("Worktree", Color::LightYellow, &[("s", "stashes")]);
    builder.finish()
}

fn styled_key_hints(items: &[(&str, &str)], key_color: Color) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (key, description)) in items.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" • ", Style::default().fg(Color::White)));
        }
        spans.push(Span::styled(
            key.to_string(),
            Style::default().fg(key_color).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {description}"),
            Style::default().fg(Color::White),
        ));
    }
    Line::from(spans)
}

/// Renders key hints but drops trailing items that would not fit `max_width`,
/// so a narrow panel never clips a hint mid-word.
fn styled_key_hints_fitting(
    items: &[(&str, &str)],
    key_color: Color,
    max_width: usize,
) -> Line<'static> {
    let mut kept = Vec::new();
    let mut width = 0usize;
    for (key, description) in items {
        let item = key.chars().count() + 1 + description.chars().count();
        let with_separator = item + if kept.is_empty() { 0 } else { 3 };
        if width + with_separator > max_width {
            break;
        }
        width += with_separator;
        kept.push((*key, *description));
    }
    styled_key_hints(&kept, key_color)
}

pub struct App {
    graph: GraphModel,
    graph_display_refs: HashMap<String, Vec<DisplayRef>>,
    tracking_by_branch: HashMap<String, BranchTracking>,
    current_branch_ids: HashSet<String>,
    status: Vec<StatusEntry>,
    backend: Option<Box<dyn GitBackend>>,
    diff_lines: Vec<DiffLine>,
    diff_error: Option<String>,
    diff_full: bool,
    diff_truncated: Option<(usize, usize)>,
    selected_diff_hunk: usize,
    hunk_mode: bool,
    line_mode: bool,
    diff_mode_before_line: Option<DiffMode>,
    selected_diff_line: Option<usize>,
    diff_target: DiffTarget,
    diff_path: Option<GitPath>,
    /// Cached diff layout (cheap) for the current mode; rows are materialized
    /// on demand so syntax highlighting only runs for the visible slice.
    diff_cache: Option<(DiffMode, Vec<DiffRow>)>,
    /// Indices of hunk-header lines within `diff_lines`, recomputed only when
    /// the diff changes so the render and hunk-navigation paths don't rescan.
    hunk_header_indices: Vec<usize>,
    diff_stats: HashMap<(GitPath, bool), (usize, usize)>,
    diff_hunk_counts: HashMap<GitPath, (usize, usize)>,
    conflict_commits: Option<ConflictCommits>,
    active_conflict_paths: HashSet<GitPath>,
    rows: Vec<GraphRow>,
    max_lanes: usize,
    scroll_y: usize,
    scroll_to_selected_graph: bool,
    scroll_x: usize,
    selected_graph: usize,
    local_status: Vec<StatusEntry>,
    unstaged_indices: Vec<usize>,
    staged_indices: Vec<usize>,
    historical_commit: Option<String>,
    /// The terminal's actual background color (queried once via OSC 11), used
    /// to draw pill text/caps explicitly; `None` falls back to reverse video.
    bg: Option<Color>,
    terminal_palette: TerminalAnsiPalette,
    focus: Focus,
    selected_status: usize,
    selected_statuses: HashSet<usize>,
    status_section: StatusSection,
    staging_area: Rect,
    graph_area: Rect,
    details_area: Rect,
    diff_area: Rect,
    diff_scroll: u16,
    /// Horizontal pan (in display columns) of the focused diff's content.
    diff_scroll_x: u16,
    details_scroll: u16,
    diff_mode: DiffMode,
    graph_visible: bool,
    diff_number_width: usize,
    commit_title: String,
    commit_title_cursor: usize,
    commit_description: String,
    commit_description_cursor: usize,
    commit_field: CommitField,
    commit_mode: CommitMode,
    commit_popup: bool,
    commit_confirm: bool,
    abort_rebase_popup: bool,
    error_popup: Option<String>,
    error_title: String,
    error_scroll: u16,
    graph_action: Option<GraphAction>,
    graph_action_return_focus: Option<Focus>,
    graph_action_input: String,
    graph_action_cursor: usize,
    discard_paths: Vec<GitPath>,
    reset_mode: usize,
    reset_focus: usize,
    remote_action: bool,
    push_mode: usize,
    push_focus: usize,
    action_error: Option<String>,
    undo_options: Vec<UndoInfo>,
    undo_selection: usize,
    repository_path: Option<PathBuf>,
    watch_tx: SyncSender<()>,
    watch_rx: Receiver<()>,
    refresh_tx: Sender<RepositoryRefreshCompletion>,
    refresh_rx: Receiver<RepositoryRefreshCompletion>,
    refresh_deadline: Option<Instant>,
    refresh_in_progress: bool,
    /// Bumped on every internal repository mutation. A background refresh that
    /// started before the latest bump is stale and must not overwrite the
    /// status the app already recomputed itself.
    refresh_generation: u64,
    /// Our own mutations write to `.git` (loose objects, refs) and would
    /// otherwise trigger a redundant auto-refresh right after the UI already
    /// reloaded. Ignore watcher events inside this window.
    ignore_watch_until: Option<Instant>,
    watch_error: Option<String>,
    graph_details_tx: Sender<GraphDetailsCompletion>,
    graph_details_rx: Receiver<GraphDetailsCompletion>,
    graph_details_deadline: Option<Instant>,
    graph_details_in_progress: bool,
    diff_stats_tx: Sender<DiffStatsCompletion>,
    diff_stats_rx: Receiver<DiffStatsCompletion>,
    diff_stats_generation: u64,
    details_loading: bool,
    details_error: Option<String>,
    search_popup: Option<SearchPopupState>,
    remote_tx: Sender<RemoteCompletion>,
    remote_rx: Receiver<RemoteCompletion>,
    remote_operation: Option<RemoteOperation>,
    remote_started: Option<Instant>,
    remote_notice: Option<(String, Instant)>,
    update_tx: Sender<UpdateEvent>,
    update_rx: Receiver<UpdateEvent>,
    update_status: UpdateStatus,
    update_status_since: Option<Instant>,
    remote_choices: Vec<String>,
    remote_selection: usize,
    push_remote: Option<String>,
    push_target: Option<PushTarget>,
    push_after_create: bool,
    delete_targets: Vec<DeleteTarget>,
    delete_target_index: usize,
    loading: bool,
    loading_started: Option<Instant>,
    graph_page_tx: Sender<GraphPageCompletion>,
    graph_page_rx: Receiver<GraphPageCompletion>,
    graph_page_in_progress: bool,
    graph_generation: u64,
    layout_engine: layout::Layout,
    graph_complete: bool,
    reset_boundary: Option<String>,
    pending_fixup: bool,
    fixup_origin: usize,
    rebase_paused: bool,
    rebase_active: bool,
    rebase_edit: Option<RebaseEditState>,
    rebase_plan: Option<RebasePlanState>,
    pending_conflict_editor: Option<GitPath>,
    three_way_view: Option<ThreeWayView>,
    stash_popup: Option<StashUiState>,
    help_popup: bool,
    help_scroll: u16,
}

struct StashUiState {
    entries: Vec<StashEntry>,
    selected: usize,
    saving: bool,
    message: String,
    message_cursor: usize,
    include_untracked: bool,
    confirming_drop: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Status,
    Graph,
    Details,
    Commit,
    Diff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommitField {
    Title,
    Description,
    Commit,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CommitMode {
    Normal,
    Amend,
    Fixup(String),
    Reword(String),
    Edit(String),
    RebaseReplacement,
    PlanReword(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RebaseEditState {
    target: String,
    parents: Vec<String>,
    title: String,
    description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RebasePlanState {
    plan: RebasePlan,
    selected: usize,
}

fn rebase_plan_entry_count(plan: &RebasePlan) -> usize {
    plan.blocks
        .iter()
        .map(|block| match block {
            RebasePlanBlock::Control(_) => 0,
            RebasePlanBlock::Commits(entries) => entries.len(),
            RebasePlanBlock::Merge(_) => 1,
        })
        .sum()
}

fn plan_entry_location(plan: &RebasePlan, selected: usize) -> Option<(usize, usize)> {
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

fn plan_entry_at(plan: &RebasePlan, selected: usize) -> Option<&RebasePlanEntry> {
    let (block_index, entry_index) = plan_entry_location(plan, selected)?;
    match &plan.blocks[block_index] {
        RebasePlanBlock::Commits(entries) => entries.get(entry_index),
        RebasePlanBlock::Merge(entry) if entry_index == 0 => Some(entry),
        RebasePlanBlock::Control(_) | RebasePlanBlock::Merge(_) => None,
    }
}

fn plan_entry_mut<'a>(plan: &'a mut RebasePlan, id: &str) -> Option<&'a mut RebasePlanEntry> {
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

fn rebase_todo_action_name(action: &RebaseTodoAction) -> &'static str {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GraphAction {
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum PushTarget {
    Branch(String),
    Tag(String),
}

/// A branch or tag at the selected commit that can be deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DeleteTarget {
    Branch(String),
    Tag(String),
}

impl DeleteTarget {
    fn name(&self) -> &str {
        match self {
            Self::Branch(name) | Self::Tag(name) => name,
        }
    }

    fn is_tag(&self) -> bool {
        matches!(self, Self::Tag(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteOperation {
    Fetch(String),
    Pull,
    Push {
        mode: usize,
        remote: String,
        target: PushTarget,
    },
}

impl RemoteOperation {
    fn label(&self) -> &'static str {
        match self {
            Self::Fetch(_) => "Fetch",
            Self::Pull => "Pull",
            Self::Push { .. } => "Push",
        }
    }
}

#[derive(Clone)]
struct GraphSelectionSnapshot {
    commit_id: Option<String>,
    row: usize,
    had_local_row: bool,
    selected_local_row: bool,
}

struct RemoteCompletion {
    operation: RemoteOperation,
    result: std::result::Result<RepositorySnapshot, String>,
    selection: GraphSelectionSnapshot,
}

struct RepositoryRefreshCompletion {
    generation: u64,
    result: std::result::Result<RepositorySnapshot, String>,
}

struct GraphPageCompletion {
    /// Commit count and graph generation when the page was requested. The
    /// generation distinguishes reloads whose first page has the same length.
    skip: usize,
    generation: u64,
    result: std::result::Result<GraphWindow, String>,
}

struct RepositorySnapshot {
    graph: GraphModel,
    status: Vec<StatusEntry>,
    rows: Vec<GraphRow>,
    max_lanes: usize,
    layout: layout::Layout,
    complete: bool,
}

struct GraphDetailsCompletion {
    commit_id: String,
    result: std::result::Result<HistoricalCommitDetails, String>,
}

struct DiffStatsCompletion {
    generation: u64,
    result: std::result::Result<DiffStatsSnapshot, String>,
}

struct DiffStatsSnapshot {
    stats: Vec<PathDiffStats>,
    hunks: Vec<(GitPath, (usize, usize))>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateAvailability {
    Available,
    Current,
    UnsupportedInstall,
    SkippedRecentlyChecked,
}

enum UpdateEvent {
    Check {
        manual: bool,
        result: std::result::Result<UpdateAvailability, String>,
    },
    Install(std::result::Result<bool, String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UpdateStatus {
    Idle,
    Checking,
    Available,
    Installing,
    RestartRequired,
    Current,
    UnsupportedInstall,
    Failed(String),
}

struct HistoricalCommitDetails {
    files: Vec<StatusEntry>,
    stats: Vec<(GitPath, usize, usize)>,
    diff: Option<(GitPath, String, usize)>,
}

struct SearchPopupState {
    query: String,
    cursor: usize,
    matches: Vec<String>,
    selected: usize,
    searched: bool,
    error: Option<String>,
}

fn conflict_paths(status: &[StatusEntry]) -> HashSet<GitPath> {
    status
        .iter()
        .filter(|entry| entry.is_conflict())
        .map(|entry| entry.path.clone())
        .collect()
}

/// Paths inside `.git` that change as a side effect of reading repository
/// state. Reacting to them would make the watcher refresh itself in a loop.
fn is_ignored_watch_path(path: &Path, git_dir: Option<&Path>) -> bool {
    let Some(git_dir) = git_dir else { return false };
    if !path.starts_with(git_dir) {
        return false;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    name == "index" || name.ends_with(".lock")
}

/// Commits loaded per history page. The first page renders immediately and
/// older history is appended as the selection approaches the end.
/// Override with `FIGIT_GRAPH_PAGE` (useful on very large repositories).
const GRAPH_PAGE: usize = 2000;

fn graph_page_size() -> usize {
    std::env::var("FIGIT_GRAPH_PAGE")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .filter(|size| *size > 0)
        .unwrap_or(GRAPH_PAGE)
}

fn update_check_cache_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .map(|cache| cache.join("figit/update-check"))
}

fn update_check_is_recent() -> bool {
    let Some(path) = update_check_cache_path() else {
        return false;
    };
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(previous) = contents.trim().parse::<u64>() else {
        return false;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(previous, |duration| duration.as_secs());
    now.saturating_sub(previous) < Duration::from_secs(24 * 60 * 60).as_secs()
}

fn record_update_check() {
    let Some(path) = update_check_cache_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return;
    };
    if std::fs::create_dir_all(parent).is_ok() {
        let _ = std::fs::write(path, now.as_secs().to_string());
    }
}

fn load_repository_snapshot(path: &Path) -> Result<RepositorySnapshot> {
    let backend = crate::backend::open(path)?;
    crate::profile::mark("snapshot: backend");
    let window = backend.graph_window(0, graph_page_size())?;
    crate::profile::mark("snapshot: graph page");
    let status = backend.status()?;
    crate::profile::mark("snapshot: status");
    let mut layout = layout::Layout::new();
    let rows = if window.complete {
        let known: HashSet<&str> = window
            .commits
            .iter()
            .map(|commit| commit.id.as_str())
            .collect();
        layout.extend(&window.commits, Some(&known))
    } else {
        layout.extend(&window.commits, None)
    };
    let max_lanes = layout::max_lanes(&rows);
    crate::profile::mark("snapshot: layout");
    Ok(RepositorySnapshot {
        graph: GraphModel {
            commits: window.commits,
            refs: window.refs,
            branch_tracking: window.branch_tracking,
            head_id: window.head_id,
            head_name: window.head_name,
        },
        status,
        rows,
        max_lanes,
        layout,
        complete: window.complete,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusSection {
    Unstaged,
    Staged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffMode {
    Unified,
    SideBySide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffKind {
    Meta,
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone)]
struct DiffLine {
    old_no: Option<u32>,
    new_no: Option<u32>,
    text: String,
    kind: DiffKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ThreeWayCell {
    number: usize,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ThreeWayRow {
    base: Option<ThreeWayCell>,
    ours: Option<ThreeWayCell>,
    theirs: Option<ThreeWayCell>,
    result: Option<ThreeWayCell>,
}

struct ThreeWayView {
    path: GitPath,
    rows: Vec<ThreeWayRow>,
    ours_diff: Vec<DiffLine>,
    theirs_diff: Vec<DiffLine>,
    result_diff: Vec<DiffLine>,
    working: Vec<u8>,
    pending_resolution: Option<ConflictResolution>,
    result_preview: bool,
    chosen_side: Option<ConflictSide>,
    hunk_index: usize,
    ours_missing: bool,
    theirs_missing: bool,
    result_missing: bool,
    scroll_y: usize,
    scroll_x: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConflictHunk {
    start: usize,
    end: usize,
    ours: String,
    base: Option<String>,
    theirs: String,
}

fn conflict_hunks(text: &str) -> Vec<ConflictHunk> {
    let lines = text.split_inclusive('\n').collect::<Vec<_>>();
    let mut hunks = Vec::new();
    let mut index = 0;
    let mut offset = 0;
    while index < lines.len() {
        let line = lines[index];
        let line_start = offset;
        offset += line.len();
        index += 1;
        if !line.starts_with("<<<<<<<") {
            continue;
        }

        let mut ours = String::new();
        let mut base = None;
        let mut base_text = String::new();
        let mut theirs = String::new();
        let mut section = 0;
        while index < lines.len() {
            let line = lines[index];
            offset += line.len();
            index += 1;
            if line.starts_with("|||||||") {
                base = Some(String::new());
                section = 1;
            } else if line.starts_with("=======") {
                if section == 1 {
                    base = Some(std::mem::take(&mut base_text));
                }
                section = 2;
            } else if line.starts_with(">>>>>>>") {
                hunks.push(ConflictHunk {
                    start: line_start,
                    end: offset,
                    ours,
                    base,
                    theirs,
                });
                break;
            } else {
                match section {
                    0 => ours.push_str(line),
                    1 => base_text.push_str(line),
                    _ => theirs.push_str(line),
                }
            }
        }
    }
    hunks
}

fn replace_conflict_hunk(text: &str, hunk_index: usize, side: ConflictSide) -> Option<String> {
    let hunk = conflict_hunks(text).into_iter().nth(hunk_index)?;
    let replacement = match side {
        ConflictSide::Ours => hunk.ours,
        ConflictSide::Theirs => hunk.theirs,
        ConflictSide::Base => hunk.base?,
    };
    let mut result = String::with_capacity(text.len());
    result.push_str(&text[..hunk.start]);
    result.push_str(&replacement);
    result.push_str(&text[hunk.end..]);
    Some(result)
}

fn diff_hunk_scroll(lines: &[DiffLine], hunk_index: usize) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.kind == DiffKind::Meta && line.text.starts_with("@@"))
        .nth(hunk_index)
        .map(|(index, _)| index)
}

struct AlignedSide {
    lines: Vec<Option<ThreeWayCell>>,
    insertions: Vec<Vec<ThreeWayCell>>,
}

fn display_lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

fn fit_to_cells(text: &str, width: usize) -> String {
    let mut output = String::new();
    let mut used = 0;
    for character in text.chars() {
        let cells = Line::from(character.to_string()).width();
        if used + cells > width {
            if width == 0 {
                return String::new();
            }
            while used + 1 > width {
                let Some(last) = output.pop() else { break };
                used -= Line::from(last.to_string()).width();
            }
            output.push('…');
            return output;
        }
        output.push(character);
        used += cells;
    }
    output
}

fn align_conflict_side(base: &str, side: &str) -> AlignedSide {
    let base_lines = display_lines(base);
    let side_lines = display_lines(side);
    let mut aligned = AlignedSide {
        lines: vec![None; base_lines.len()],
        insertions: vec![Vec::new(); base_lines.len() + 1],
    };
    let diff = TextDiff::from_lines(base, side);
    for op in diff.ops() {
        let old = op.old_range();
        let new = op.new_range();
        match op.tag() {
            DiffTag::Equal | DiffTag::Replace => {
                let matched = (old.end - old.start).min(new.end - new.start);
                for offset in 0..matched {
                    let side_index = new.start + offset;
                    aligned.lines[old.start + offset] = Some(ThreeWayCell {
                        number: side_index + 1,
                        text: side_lines[side_index].clone(),
                    });
                }
                for (side_index, text) in side_lines
                    .iter()
                    .enumerate()
                    .take(new.end)
                    .skip(new.start + matched)
                {
                    aligned.insertions[old.end].push(ThreeWayCell {
                        number: side_index + 1,
                        text: text.clone(),
                    });
                }
            }
            DiffTag::Insert => {
                for (side_index, text) in
                    side_lines.iter().enumerate().take(new.end).skip(new.start)
                {
                    aligned.insertions[old.start].push(ThreeWayCell {
                        number: side_index + 1,
                        text: text.clone(),
                    });
                }
            }
            DiffTag::Delete => {}
        }
    }
    aligned
}

fn three_way_rows(versions: &crate::backend::ConflictVersions) -> Vec<ThreeWayRow> {
    let base = String::from_utf8_lossy(versions.base.as_deref().unwrap_or_default());
    let ours = String::from_utf8_lossy(versions.ours.as_deref().unwrap_or_default());
    let theirs = String::from_utf8_lossy(versions.theirs.as_deref().unwrap_or_default());
    let working = String::from_utf8_lossy(versions.working.as_deref().unwrap_or_default());
    let base_lines = display_lines(&base);
    let ours = align_conflict_side(&base, &ours);
    let theirs = align_conflict_side(&base, &theirs);
    let result = align_conflict_side(&base, &working);
    let mut rows = Vec::new();
    for index in 0..=base_lines.len() {
        let count = ours.insertions[index]
            .len()
            .max(theirs.insertions[index].len())
            .max(result.insertions[index].len());
        for offset in 0..count {
            rows.push(ThreeWayRow {
                base: None,
                ours: ours.insertions[index].get(offset).cloned(),
                theirs: theirs.insertions[index].get(offset).cloned(),
                result: result.insertions[index].get(offset).cloned(),
            });
        }
        if let Some(text) = base_lines.get(index) {
            rows.push(ThreeWayRow {
                base: Some(ThreeWayCell {
                    number: index + 1,
                    text: text.clone(),
                }),
                ours: ours.lines[index].clone(),
                theirs: theirs.lines[index].clone(),
                result: result.lines[index].clone(),
            });
        }
    }
    rows
}

fn three_way_column(
    cell: Option<&ThreeWayCell>,
    base: Option<&ThreeWayCell>,
    path: &str,
    width: usize,
    horizontal_scroll: usize,
    color: Color,
) -> Line<'static> {
    let number_width = 4.min(width);
    let text_width = width.saturating_sub(number_width);
    let changed = cell.is_some_and(|cell| base.is_none_or(|base| cell.text != base.text));
    let number = cell.map_or_else(
        || " ".repeat(number_width),
        |cell| format!("{:>3} ", cell.number),
    );
    let number = truncate_start(&number, number_width);
    let text = cell.map_or_else(String::new, |cell| {
        fit_to_cells(
            &cell
                .text
                .chars()
                .skip(horizontal_scroll)
                .collect::<String>(),
            text_width,
        )
    });
    let text_width_used = Line::from(text.clone()).width();
    let text_color = if changed { color } else { Color::DarkGray };
    let mut spans = vec![Span::styled(number, Style::default().fg(text_color))];
    spans.extend(syntax_highlight(&text, path, text_color));
    if text_width_used < text_width {
        spans.push(Span::styled(
            " ".repeat(text_width - text_width_used),
            Style::default().fg(text_color),
        ));
    }
    Line::from(spans)
}

impl App {
    pub fn new(
        graph: GraphModel,
        status: Vec<StatusEntry>,
        backend: Option<Box<dyn GitBackend>>,
    ) -> Self {
        let rows = layout::layout(&graph);
        let max_lanes = layout::max_lanes(&rows);
        let has_local_changes = !status.is_empty();
        let repository_path = backend
            .as_ref()
            .map(|backend| backend.working_directory().to_path_buf());
        let (remote_tx, remote_rx) = mpsc::channel();
        let (update_tx, update_rx) = mpsc::channel();
        let (watch_tx, watch_rx) = mpsc::sync_channel(1);
        let (refresh_tx, refresh_rx) = mpsc::channel();
        let (graph_page_tx, graph_page_rx) = mpsc::channel();
        let (graph_details_tx, graph_details_rx) = mpsc::channel();
        let (diff_stats_tx, diff_stats_rx) = mpsc::channel();
        let conflict_commits = if status.iter().any(StatusEntry::is_conflict) {
            backend
                .as_ref()
                .and_then(|backend| backend.conflict_commits().ok().flatten())
        } else {
            None
        };
        let active_conflict_paths = conflict_paths(&status);
        let rebase_active = backend
            .as_ref()
            .and_then(|backend| backend.rebase_in_progress().ok())
            .unwrap_or(false);
        let mut app = Self {
            graph,
            graph_display_refs: HashMap::new(),
            tracking_by_branch: HashMap::new(),
            current_branch_ids: HashSet::new(),
            status: status.clone(),
            backend,
            diff_lines: Vec::new(),
            diff_error: None,
            diff_full: false,
            diff_truncated: None,
            selected_diff_hunk: 0,
            hunk_mode: false,
            line_mode: false,
            diff_mode_before_line: None,
            selected_diff_line: None,
            diff_target: DiffTarget::Worktree,
            diff_path: None,
            diff_cache: None,
            hunk_header_indices: Vec::new(),
            diff_stats: HashMap::new(),
            diff_hunk_counts: HashMap::new(),
            conflict_commits,
            active_conflict_paths,
            rows,
            max_lanes,
            scroll_y: 0,
            scroll_to_selected_graph: false,
            scroll_x: 0,
            selected_graph: 0,
            local_status: status.clone(),
            unstaged_indices: Vec::new(),
            staged_indices: Vec::new(),
            historical_commit: None,
            bg: None,
            terminal_palette: [None; 16],
            focus: if has_local_changes {
                Focus::Status
            } else {
                Focus::Graph
            },
            selected_status: 0,
            selected_statuses: HashSet::new(),
            status_section: StatusSection::Unstaged,
            staging_area: Rect::default(),
            graph_area: Rect::default(),
            details_area: Rect::default(),
            diff_area: Rect::default(),
            diff_scroll: 0,
            diff_scroll_x: 0,
            details_scroll: 0,
            diff_mode: DiffMode::Unified,
            graph_visible: true,
            diff_number_width: 3,
            commit_title: String::new(),
            commit_title_cursor: 0,
            commit_description: String::new(),
            commit_description_cursor: 0,
            commit_field: CommitField::Title,
            commit_mode: CommitMode::Normal,
            commit_popup: false,
            commit_confirm: false,
            abort_rebase_popup: false,
            error_popup: None,
            error_title: "Git error".into(),
            error_scroll: 0,
            graph_action: None,
            graph_action_return_focus: None,
            graph_action_input: String::new(),
            graph_action_cursor: 0,
            discard_paths: Vec::new(),
            reset_mode: 1,
            reset_focus: 1,
            remote_action: false,
            push_mode: 0,
            push_focus: 0,
            action_error: None,
            undo_options: Vec::new(),
            undo_selection: 0,
            repository_path,
            watch_tx,
            watch_rx,
            refresh_tx,
            refresh_rx,
            refresh_deadline: None,
            refresh_in_progress: false,
            refresh_generation: 0,
            ignore_watch_until: None,
            watch_error: None,
            graph_details_tx,
            graph_details_rx,
            graph_details_deadline: None,
            graph_details_in_progress: false,
            diff_stats_tx,
            diff_stats_rx,
            diff_stats_generation: 0,
            details_loading: false,
            details_error: None,
            search_popup: None,
            remote_tx,
            remote_rx,
            remote_operation: None,
            remote_started: None,
            remote_notice: None,
            update_tx,
            update_rx,
            update_status: UpdateStatus::Idle,
            update_status_since: None,
            remote_choices: Vec::new(),
            remote_selection: 0,
            push_remote: None,
            push_target: None,
            push_after_create: false,
            delete_targets: Vec::new(),
            delete_target_index: 0,
            loading: false,
            loading_started: None,
            graph_page_tx,
            graph_page_rx,
            graph_page_in_progress: false,
            graph_generation: 0,
            layout_engine: layout::Layout::new(),
            graph_complete: true,
            reset_boundary: None,
            fixup_origin: 0,
            pending_fixup: false,
            rebase_paused: rebase_active,
            rebase_active,
            rebase_edit: None,
            rebase_plan: None,
            pending_conflict_editor: None,
            three_way_view: None,
            stash_popup: None,
            help_popup: false,
            help_scroll: 0,
        };
        app.rebuild_graph_indexes();
        app.refresh_status_indices();
        app.status_section = if app.unstaged_indices.is_empty() && !app.staged_indices.is_empty() {
            StatusSection::Staged
        } else {
            StatusSection::Unstaged
        };
        app.selected_status = app
            .section_indices(app.status_section)
            .first()
            .copied()
            .unwrap_or(0);
        if !app.status.is_empty() {
            app.selected_statuses.insert(app.selected_status);
        }
        app.refresh_diff();
        app.refresh_diff_stats();
        app
    }

    fn rebuild_graph_indexes(&mut self) {
        self.current_branch_ids = current_branch_ids(&self.graph);
        self.graph_display_refs = self.graph.display_refs_by_commit();
        self.tracking_by_branch = self
            .graph
            .branch_tracking
            .iter()
            .cloned()
            .map(|tracking| (tracking.branch.clone(), tracking))
            .collect();
    }

    pub fn loading(backend: Box<dyn GitBackend>) -> Self {
        let mut app = Self::new(GraphModel::default(), Vec::new(), Some(backend));
        app.loading = true;
        app.loading_started = Some(Instant::now());
        app
    }

    /// Replace the parsed diff, keeping the derived caches (hunk headers and
    /// the rendered-line cache) in sync. Always go through this rather than
    /// assigning `diff_lines` directly.
    fn set_diff_lines(&mut self, lines: Vec<DiffLine>) {
        self.hunk_header_indices = diff_hunk_headers(&lines);
        self.diff_lines = lines;
        self.diff_cache = None;
    }

    fn apply_diff_text(&mut self, diff: String) {
        let total = diff.lines().count();
        if !self.diff_full && total > MAX_RENDERED_DIFF_LINES {
            let mut text = diff
                .lines()
                .take(MAX_RENDERED_DIFF_LINES)
                .collect::<Vec<_>>()
                .join("\n");
            text.push('\n');
            self.set_diff_lines(parse_diff(&text));
            self.diff_truncated = Some((MAX_RENDERED_DIFF_LINES, total));
        } else {
            self.set_diff_lines(parse_diff(&diff));
            self.diff_truncated = None;
        }
    }

    fn load_full_diff(&mut self) {
        if self.diff_truncated.is_none() {
            return;
        }
        self.diff_full = true;
        self.refresh_diff();
    }

    fn refresh_diff(&mut self) {
        let previous_three_way_view = self.three_way_view.take();
        let previous_path = self.diff_path.clone();
        let previous_target = self.diff_target;
        self.diff_path = self
            .status
            .get(self.selected_status)
            .map(|e| e.path.clone());
        self.diff_target = if self.status_section == StatusSection::Staged {
            DiffTarget::Index
        } else {
            DiffTarget::Worktree
        };
        if previous_path != self.diff_path || previous_target != self.diff_target {
            self.selected_diff_hunk = 0;
            self.selected_diff_line = None;
            self.diff_full = false;
            self.diff_truncated = None;
        }
        self.diff_number_width = self
            .diff_path
            .as_ref()
            .and_then(|path| {
                self.backend.as_ref().and_then(|backend| {
                    backend
                        .file_line_number_width(path, self.historical_commit.as_deref())
                        .ok()
                })
            })
            .unwrap_or(3);
        let diff_result = self
            .status
            .get(self.selected_status)
            .and_then(|entry| {
                self.backend
                    .as_ref()
                    .map(|backend| match &self.historical_commit {
                        Some(commit) => backend.commit_diff(commit, &entry.path),
                        None => backend.diff(&entry.path, self.diff_target),
                    })
            })
            .transpose()
            .map_err(|error| format!("{error:#}"));
        let diff = match diff_result {
            Ok(Some(diff)) => {
                self.diff_error = None;
                diff
            }
            Ok(None) => {
                self.diff_error = None;
                String::new()
            }
            Err(error) => {
                self.diff_error = Some(error.clone());
                format!("\\ Could not load diff: {error}\n")
            }
        };
        self.apply_diff_text(diff);
        self.selected_diff_hunk = self
            .selected_diff_hunk
            .min(self.hunk_header_indices.len().saturating_sub(1));
        if self.line_mode
            && !self.selected_diff_line.is_some_and(|selected| {
                self.diff_lines
                    .get(selected)
                    .is_some_and(|line| matches!(line.kind, DiffKind::Added | DiffKind::Removed))
            })
        {
            self.selected_diff_line = self
                .diff_lines
                .iter()
                .position(|line| matches!(line.kind, DiffKind::Added | DiffKind::Removed));
        }
        self.diff_scroll = 0;
        self.diff_scroll_x = 0;
        self.diff_cache = None;
        let active_conflicts = self
            .active_conflict_paths
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let selected_conflict = (self.historical_commit.is_none())
            .then(|| self.status.get(self.selected_status))
            .flatten()
            .filter(|entry| entry.is_conflict())
            .map(|entry| entry.path.clone());
        let resolver_path = selected_conflict
            .filter(|path| active_conflicts.contains(path))
            .or_else(|| {
                previous_three_way_view
                    .as_ref()
                    .map(|view| view.path.clone())
                    .filter(|path| active_conflicts.contains(path))
            })
            .or_else(|| active_conflicts.first().cloned());
        self.three_way_view = resolver_path.and_then(|path| {
            self.backend
                .as_ref()
                .and_then(|backend| backend.conflict_versions(&path).ok().flatten())
                .map(|versions| {
                    let base =
                        String::from_utf8_lossy(versions.base.as_deref().unwrap_or_default());
                    let ours =
                        String::from_utf8_lossy(versions.ours.as_deref().unwrap_or_default());
                    let theirs =
                        String::from_utf8_lossy(versions.theirs.as_deref().unwrap_or_default());
                    let result =
                        String::from_utf8_lossy(versions.working.as_deref().unwrap_or_default());
                    let working = versions.working.clone().unwrap_or_default();
                    let mut view = ThreeWayView {
                        path: path.clone(),
                        rows: three_way_rows(&versions),
                        ours_diff: conflict_result_diff(&base, &ours, "Ours"),
                        theirs_diff: conflict_result_diff(&base, &theirs, "Theirs"),
                        result_diff: conflict_result_diff(&base, &result, "Result"),
                        working,
                        pending_resolution: None,
                        result_preview: false,
                        chosen_side: None,
                        hunk_index: 0,
                        ours_missing: versions.ours.is_none(),
                        theirs_missing: versions.theirs.is_none(),
                        result_missing: versions.working.is_none(),
                        scroll_y: 0,
                        scroll_x: 0,
                    };
                    if let Some(previous) = previous_three_way_view
                        .as_ref()
                        .filter(|previous| previous.path == path)
                    {
                        let unchanged = previous.working == view.working
                            && previous.result_missing == view.result_missing;
                        if unchanged {
                            view.result_preview = previous.result_preview;
                            view.chosen_side = previous.chosen_side;
                            view.pending_resolution = previous.pending_resolution.clone();
                        } else if previous.result_preview {
                            self.action_error = Some("Conflict file changed; preview cancelled. Review the current contents before confirming.".into());
                        }
                        view.hunk_index = previous.hunk_index;
                        view.scroll_y = previous.scroll_y;
                        view.scroll_x = previous.scroll_x;
                        if let Some(pending) = view.pending_resolution.as_ref() {
                            let result = String::from_utf8_lossy(pending.content().unwrap_or_default());
                            view.result_diff = conflict_result_diff(&base, &result, "Result");
                        }
                        let hunk_count =
                            conflict_hunks(&String::from_utf8_lossy(&view.working)).len();
                        view.hunk_index = view.hunk_index.min(hunk_count.saturating_sub(1));
                    }
                    view
                })
        });
        if self.hunk_mode
            && (self.three_way_view.is_some()
                || (previous_path != self.diff_path && self.hunk_header_indices.is_empty()))
        {
            self.hunk_mode = false;
            if self.focus == Focus::Diff {
                self.focus = Focus::Status;
            }
        }
        if self.three_way_view.is_some() && self.focus == Focus::Status {
            self.focus = Focus::Diff;
        }
    }

    #[cfg(test)]
    fn graph_lane_color(&self, id: &str) -> Option<Color> {
        self.rows.iter().find_map(|row| {
            (self.graph.commits[row.commit_index].id == id)
                .then(|| {
                    row.node_col
                        .map(|col| PALETTE[row.colors[col] % PALETTE.len()])
                })
                .flatten()
        })
    }

    fn conflict_colors(&self) -> Option<(Color, Color)> {
        self.conflict_commits
            .as_ref()
            .map(|_| (Color::LightBlue, Color::LightYellow))
    }

    fn conflict_commit_color(&self, id: &str) -> Option<Color> {
        let commits = self.conflict_commits.as_ref()?;
        let (ours, theirs) = self.conflict_colors()?;
        if id == commits.ours {
            Some(ours)
        } else if id == commits.theirs {
            Some(theirs)
        } else if commits.base.as_deref() == Some(id) {
            Some(Color::LightMagenta)
        } else {
            None
        }
    }

    fn selected_conflict_colors(&self) -> Option<(Color, Color)> {
        if self.historical_commit.is_some() || !self.status.get(self.selected_status)?.is_conflict()
        {
            return None;
        }
        self.conflict_colors()
    }

    /// Rebuilds the cached diff layout when the diff or its mode changed.
    fn ensure_diff_layout(&mut self) {
        let cache_hit = matches!(
            &self.diff_cache,
            Some((mode, _)) if *mode == self.diff_mode
        );
        if !cache_hit {
            self.diff_cache = Some((
                self.diff_mode,
                diff_layout(&self.diff_lines, self.diff_mode),
            ));
        }
    }

    /// The input line range emphasized for the selected hunk.
    fn selected_hunk_range(&self) -> Option<(usize, usize)> {
        let start = *self.hunk_header_indices.get(self.selected_diff_hunk)?;
        let end = self
            .hunk_header_indices
            .get(self.selected_diff_hunk + 1)
            .copied()
            .unwrap_or(self.diff_lines.len());
        Some((start, end))
    }

    /// Materializes one diff row with syntax highlighting and the current hunk
    /// or line selection emphasis. Only visible rows are ever materialized.
    fn materialize_diff_row(&self, row: DiffRow, width: usize) -> Line<'static> {
        let path = self
            .diff_path
            .as_ref()
            .map(GitPath::display)
            .unwrap_or(Cow::Borrowed(""));
        let mut line = render_diff_row(
            &self.diff_lines,
            row,
            width,
            path.as_ref(),
            self.diff_number_width,
            usize::from(self.diff_scroll_x),
            true,
        );
        if self.three_way_view.is_none() {
            let (old, new) = row.input_indices();
            if self.line_mode {
                if let Some(selected) = self.selected_diff_line {
                    if (old == Some(selected) || new == Some(selected))
                        && matches!(
                            self.diff_lines[selected].kind,
                            DiffKind::Added | DiffKind::Removed
                        )
                    {
                        emphasize_selected_diff_line(&mut line, self.diff_lines[selected].kind);
                    }
                }
            } else if self.hunk_mode {
                if let Some((start, end)) = self.selected_hunk_range() {
                    let in_hunk = [old, new]
                        .into_iter()
                        .flatten()
                        .any(|index| index >= start && index < end);
                    if in_hunk {
                        emphasize_selected_diff_hunk(&mut line);
                    }
                }
            }
        }
        pad_colored_lines(std::slice::from_mut(&mut line), width);
        line
    }

    fn selected_search_query(&self) -> Option<&str> {
        let state = self.search_popup.as_ref().filter(|state| state.searched)?;
        let selected = state.matches.get(state.selected)?;
        (self.historical_commit.as_deref() == Some(selected.as_str()))
            .then_some(state.query.as_str())
    }

    fn select_graph_entry(&mut self, delta: i32) {
        let has_local = !self.local_status.is_empty();
        let total = self.rows.len() + usize::from(has_local);
        if total == 0 {
            return;
        }
        let selected = if delta < 0 {
            self.selected_graph.saturating_sub(1)
        } else {
            (self.selected_graph + 1).min(total - 1)
        };
        self.select_graph_index(selected);
    }

    fn select_graph_index(&mut self, selected: usize) {
        self.action_error = None;
        let has_local = !self.local_status.is_empty();
        let total = self.rows.len() + usize::from(has_local);
        if total == 0 {
            return;
        }
        self.selected_graph = selected.min(total - 1);
        self.scroll_to_selected_graph = true;
        self.keep_selected_graph_node_visible();
        if self.pending_fixup
            && self.selected_graph != self.fixup_origin
            && self.selected_graph >= usize::from(has_local)
        {
            // Keep the local staged state while the user chooses a target.
            return;
        }
        if has_local && self.selected_graph == 0 {
            self.graph_details_deadline = None;
            self.details_loading = false;
            self.details_error = None;
            self.historical_commit = None;
            self.status = self.local_status.clone();
            self.refresh_status_indices();
            self.selected_status = self
                .section_indices(StatusSection::Unstaged)
                .first()
                .copied()
                .unwrap_or(0);
            self.selected_statuses.clear();
            if !self.status.is_empty() {
                self.selected_statuses.insert(self.selected_status);
            }
            self.status_section = StatusSection::Unstaged;
            self.refresh_diff();
            self.refresh_diff_stats();
        } else {
            let row_index = self.selected_graph - usize::from(has_local);
            let commit = &self.graph.commits[self.rows[row_index].commit_index];
            let commit_id = commit.id.clone();
            self.historical_commit = Some(commit_id);
            self.status.clear();
            self.unstaged_indices.clear();
            self.staged_indices.clear();
            self.selected_status = 0;
            self.selected_statuses.clear();
            self.status_section = StatusSection::Unstaged;
            self.diff_stats.clear();
            self.diff_hunk_counts.clear();
            self.diff_path = None;
            self.diff_lines.clear();
            self.diff_cache = None;
            self.details_loading = true;
            self.details_error = None;
            self.graph_details_deadline = Some(Instant::now() + Duration::from_millis(80));
        }
        self.details_scroll = 0;
    }

    fn selected_graph_node_col(&self) -> Option<usize> {
        let local_offset = usize::from(!self.local_status.is_empty());
        let row_index = self.selected_graph.checked_sub(local_offset)?;
        self.rows.get(row_index)?.node_col
    }

    fn keep_selected_graph_node_visible(&mut self) {
        self.scroll_x = self
            .selected_graph_node_col()
            .map_or(0, |node_col| self.scroll_x.min(node_col));
    }

    /// Note that Figit just changed repository state itself, so watcher
    /// events from those writes are not treated as external changes.
    fn note_internal_change(&mut self) {
        self.ignore_watch_until = Some(Instant::now() + Duration::from_millis(600));
        self.refresh_generation = self.refresh_generation.wrapping_add(1);
    }

    fn refresh_status(&mut self) {
        self.note_internal_change();
        if let Some(backend) = self.backend.as_ref() {
            self.rebase_active = backend.rebase_in_progress().unwrap_or(self.rebase_active);
            if let Ok(status) = backend.status() {
                let conflict_commits = if status.iter().any(StatusEntry::is_conflict) {
                    backend.conflict_commits().ok().flatten()
                } else {
                    None
                };
                // While a commit is selected, the right pane lists that commit's
                // files; keep them rather than replacing them with the worktree.
                let pane_status = match self.historical_commit.as_deref() {
                    Some(commit) => backend
                        .commit_files(commit)
                        .unwrap_or_else(|_| status.clone()),
                    None => status.clone(),
                };
                self.active_conflict_paths = conflict_paths(&status);
                self.conflict_commits = conflict_commits;
                self.local_status = status;
                self.status = pane_status;
                self.refresh_status_indices();
                if let Some(conflict) = self.status.iter().position(StatusEntry::is_conflict) {
                    self.status_section = StatusSection::Unstaged;
                    self.selected_status = conflict;
                    self.selected_statuses.clear();
                    self.selected_statuses.insert(conflict);
                } else {
                    self.selected_statuses
                        .retain(|&index| index < self.status.len());
                    self.selected_status = self
                        .selected_status
                        .min(self.status.len().saturating_sub(1));
                }
                if self.status.is_empty() {
                    self.selected_statuses.clear();
                } else if self.selected_statuses.is_empty() {
                    self.selected_statuses.insert(self.selected_status);
                }
                self.refresh_diff();
                self.refresh_diff_stats();
            }
        }
    }

    fn refresh_status_indices(&mut self) {
        self.unstaged_indices = self
            .status
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                (e.is_conflict() || e.worktree != ' ' || e.index == '?').then_some(i)
            })
            .collect();
        self.staged_indices = self
            .status
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                (!e.is_conflict() && e.index != ' ' && e.index != '?').then_some(i)
            })
            .collect();
        self.unstaged_indices
            .sort_by(|&left, &right| self.status[left].path.cmp(&self.status[right].path));
        self.staged_indices
            .sort_by(|&left, &right| self.status[left].path.cmp(&self.status[right].path));
    }

    fn commit_actions_available(&self) -> bool {
        self.historical_commit.is_none()
            && self
                .local_status
                .iter()
                .any(|entry| !entry.is_conflict() && entry.index != ' ' && entry.index != '?')
    }

    fn reload_after_commit(&mut self) {
        let Some(path) = self.repository_path.clone() else {
            return;
        };
        if let Ok(snapshot) = load_repository_snapshot(&path) {
            self.reload_from_snapshot(snapshot);
        }
    }

    fn reload_from_snapshot(&mut self, snapshot: RepositorySnapshot) {
        self.note_internal_change();
        let RepositorySnapshot {
            graph,
            status,
            rows,
            max_lanes,
            layout: layout_engine,
            complete,
        } = snapshot;
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        self.graph_generation = self.graph_generation.wrapping_add(1);
        self.layout_engine = layout_engine;
        self.graph_complete = complete;
        self.graph_page_in_progress = false;
        self.rebase_active = backend.rebase_in_progress().unwrap_or(false);
        self.conflict_commits = if status.iter().any(StatusEntry::is_conflict) {
            backend.conflict_commits().ok().flatten()
        } else {
            None
        };
        self.active_conflict_paths = conflict_paths(&status);
        self.graph = graph;
        self.rebuild_graph_indexes();
        self.rows = rows;
        self.max_lanes = max_lanes;
        self.status = status.clone();
        self.local_status = status;
        self.historical_commit = None;
        self.graph_details_deadline = None;
        self.details_loading = false;
        self.details_error = None;
        let has_local = !self.local_status.is_empty();
        self.selected_graph = self
            .reset_boundary
            .as_ref()
            .and_then(|target| {
                self.rows
                    .iter()
                    .position(|row| self.graph.commits[row.commit_index].id == *target)
            })
            .map(|row| row + usize::from(has_local))
            .unwrap_or(0);
        self.keep_selected_graph_node_visible();
        self.refresh_status_indices();
        self.status_section = if self.unstaged_indices.is_empty() && !self.staged_indices.is_empty()
        {
            StatusSection::Staged
        } else {
            StatusSection::Unstaged
        };
        self.selected_status = self
            .section_indices(self.status_section)
            .first()
            .copied()
            .unwrap_or(0);
        self.selected_statuses.clear();
        if !self.status.is_empty() {
            self.selected_statuses.insert(self.selected_status);
        }
        self.commit_title.clear();
        self.commit_title_cursor = 0;
        self.commit_description.clear();
        self.commit_description_cursor = 0;
        self.commit_mode = CommitMode::Normal;
        self.commit_popup = false;
        self.commit_confirm = false;
        self.abort_rebase_popup = false;
        self.error_popup = None;
        self.error_scroll = 0;
        self.three_way_view = None;
        self.focus = Focus::Status;
        self.refresh_diff();
        self.refresh_diff_stats();
    }

    pub fn run(&mut self) -> Result<()> {
        crate::profile::mark("tui start");
        (self.bg, self.terminal_palette) = detect_terminal_colors();
        crate::profile::mark("terminal colours");
        let mut terminal = ratatui::init();
        install_mouse_capture_panic_hook();
        let mouse = execute!(std::io::stdout(), EnableMouseCapture);
        crate::profile::mark("terminal ready");
        let res = match mouse {
            Ok(()) => {
                self.start_update_check(false);
                let watcher = match self.start_repository_watcher() {
                    Ok(watcher) => watcher,
                    Err(error) => {
                        self.watch_error = Some(error.to_string());
                        None
                    }
                };
                crate::profile::mark("watcher ready");
                if self.loading {
                    self.refresh_deadline = Some(Instant::now());
                }
                self.event_loop(&mut terminal, watcher)
            }
            Err(error) => Err(error.into()),
        };
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        ratatui::restore();
        res
    }

    fn start_repository_watcher(&self) -> notify::Result<Option<RecommendedWatcher>> {
        let Some(backend) = self.backend.as_ref() else {
            return Ok(None);
        };
        let paths = backend
            .watch_paths()
            .map_err(|error| notify::Error::generic(&error.to_string()))?;
        let git_dir = paths
            .iter()
            .find(|path| path.file_name().is_some_and(|name| name == ".git"))
            .cloned();
        let sender = self.watch_tx.clone();
        let mut watcher = RecommendedWatcher::new(
            move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else { return };
                // `git status` refreshes `.git/index`; reacting to that write
                // would trigger another refresh that rewrites the index again,
                // rebuilding the whole graph in a tight loop. Events without a
                // path must still be treated as changes.
                if !event.paths.is_empty()
                    && event
                        .paths
                        .iter()
                        .all(|path| is_ignored_watch_path(path, git_dir.as_deref()))
                {
                    return;
                }
                match sender.try_send(()) {
                    Ok(()) | Err(TrySendError::Full(())) | Err(TrySendError::Disconnected(())) => {}
                }
            },
            NotifyConfig::default(),
        )?;
        for path in paths {
            watcher.watch(&path, RecursiveMode::Recursive)?;
        }
        Ok(Some(watcher))
    }

    fn event_loop(
        &mut self,
        terminal: &mut ratatui::DefaultTerminal,
        _watcher: Option<RecommendedWatcher>,
    ) -> Result<()> {
        let frame_time = Duration::from_millis(16);
        let mut last_frame = Instant::now() - frame_time;
        let mut first_frame = true;
        loop {
            self.poll_remote_operation();
            self.poll_update();
            self.poll_repository_refresh();
            self.poll_graph_page();
            self.poll_graph_details();
            self.poll_diff_stats();
            if last_frame.elapsed() >= frame_time {
                terminal.draw(|f| self.draw(f))?;
                last_frame = Instant::now();
                if first_frame {
                    first_frame = false;
                    crate::profile::mark("first frame");
                }
            }
            let wait = frame_time.saturating_sub(last_frame.elapsed());
            if event::poll(wait)? {
                let mut wheel: Option<(u16, u16, i32)> = None;
                if self.collect_event(event::read()?, &mut wheel) {
                    return Ok(());
                }
                // Bound event coalescing so a trackpad burst cannot starve
                // rendering, while avoiding a full redraw for every wheel tick.
                for _ in 0..31 {
                    if !event::poll(Duration::from_millis(0))? {
                        break;
                    }
                    if self.collect_event(event::read()?, &mut wheel) {
                        return Ok(());
                    }
                    if self.pending_conflict_editor.is_some() {
                        break;
                    }
                }
                if let Some(path) = self.pending_conflict_editor.take() {
                    if let Err(error) = self.run_conflict_editor(terminal, &path) {
                        self.show_rebase_error(error.to_string());
                    }
                }
                if let Some((x, y, delta)) = wheel {
                    self.handle_mouse_scroll(x, y, delta.clamp(-8, 8));
                }
            }
        }
    }

    fn collect_event(&mut self, event: Event, wheel: &mut Option<(u16, u16, i32)>) -> bool {
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

    fn handle_event(&mut self, event: Event) -> bool {
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
    fn modal_closed(&self) -> bool {
        self.graph_action.is_none()
            && !self.commit_popup
            && !self.abort_rebase_popup
            && self.rebase_plan.is_none()
            && self.error_popup.is_none()
            && self.stash_popup.is_none()
            && !self.help_popup
            && self.search_popup.is_none()
    }

    /// Returns `true` when the app should quit.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
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
        if self.remote_operation.is_some() {
            return self.handle_remote_key(key);
        }
        if key.code == KeyCode::Char('U')
            && self.graph_action.is_none()
            && !self.commit_popup
            && !self.abort_rebase_popup
            && self.stash_popup.is_none()
            && self.rebase_plan.is_none()
        {
            match self.update_status.clone() {
                UpdateStatus::Available => self.install_update(),
                UpdateStatus::Checking | UpdateStatus::Installing => {}
                _ => self.start_update_check(true),
            }
            return false;
        }
        if key.code == KeyCode::Char('?')
            && self.graph_action.is_none()
            && !self.commit_popup
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
        if self.rebase_plan.is_some() && !self.commit_popup && !self.abort_rebase_popup {
            self.handle_rebase_plan_key(key);
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
                self.refresh_diff();
                return false;
            }
        }
        if key.code == KeyCode::Char('f')
            && matches!(self.focus, Focus::Status | Focus::Diff)
            && (self.hunk_mode || self.line_mode)
        {
            self.select_file_mode();
            return false;
        }
        if matches!(self.focus, Focus::Status | Focus::Diff)
            && (key.code == KeyCode::Char('F')
                || (key.code == KeyCode::Char('f') && key.modifiers.contains(KeyModifiers::SHIFT)))
        {
            self.apply_selected_file_changes();
            return false;
        }
        if matches!(key.code, KeyCode::Char('s' | 'S'))
            && self.graph_action.is_none()
            && !self.commit_popup
            && !self.abort_rebase_popup
        {
            self.open_stash_manager();
            return false;
        }
        if self.graph_action.is_none() && !self.commit_popup && !self.abort_rebase_popup {
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
                    && !self.commit_popup
                    && !self.abort_rebase_popup =>
            {
                return true
            }
            KeyCode::Char('c')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.rebase_active
                    && self.graph_action.is_none()
                    && !self.commit_popup
                    && !self.abort_rebase_popup =>
            {
                self.continue_rebase()
            }
            KeyCode::Char('c')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.graph_action.is_none()
                    && !self.commit_popup
                    && !self.abort_rebase_popup =>
            {
                return true
            }
            KeyCode::Char('a')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.rebase_active
                    && self.graph_action.is_none()
                    && !self.commit_popup
                    && !self.abort_rebase_popup =>
            {
                self.open_abort_rebase_confirmation()
            }
            KeyCode::Char('c')
                if self.rebase_active
                    && self.rebase_edit.is_some()
                    && self.graph_action.is_none()
                    && !self.commit_popup
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
                if self.commit_popup
                    && !self.commit_confirm
                    && self.commit_field == CommitField::Title =>
            {
                self.commit_title_cursor = self.commit_title_cursor.saturating_sub(1);
            }
            KeyCode::Right
                if self.commit_popup
                    && !self.commit_confirm
                    && self.commit_field == CommitField::Title =>
            {
                self.commit_title_cursor =
                    (self.commit_title_cursor + 1).min(self.commit_title.chars().count());
            }
            KeyCode::Left
                if self.commit_popup
                    && !self.commit_confirm
                    && self.commit_field == CommitField::Description =>
            {
                self.commit_description_cursor = self.commit_description_cursor.saturating_sub(1);
            }
            KeyCode::Right
                if self.commit_popup
                    && !self.commit_confirm
                    && self.commit_field == CommitField::Description =>
            {
                self.commit_description_cursor = (self.commit_description_cursor + 1)
                    .min(self.commit_description.chars().count());
            }
            KeyCode::Up
                if self.commit_popup
                    && !self.commit_confirm
                    && self.commit_field == CommitField::Description =>
            {
                move_cursor_vertical(
                    &self.commit_description,
                    &mut self.commit_description_cursor,
                    -1,
                );
            }
            KeyCode::Down
                if self.commit_popup
                    && !self.commit_confirm
                    && self.commit_field == CommitField::Description =>
            {
                move_cursor_vertical(
                    &self.commit_description,
                    &mut self.commit_description_cursor,
                    1,
                );
            }
            KeyCode::Up | KeyCode::Down
                if self.commit_popup
                    && !self.commit_confirm
                    && self.commit_field == CommitField::Title => {}
            KeyCode::Home
                if self.commit_popup
                    && !self.commit_confirm
                    && self.commit_field == CommitField::Title =>
            {
                self.commit_title_cursor = 0;
            }
            KeyCode::End
                if self.commit_popup
                    && !self.commit_confirm
                    && self.commit_field == CommitField::Title =>
            {
                self.commit_title_cursor = self.commit_title.chars().count();
            }
            KeyCode::Home
                if self.commit_popup
                    && !self.commit_confirm
                    && self.commit_field == CommitField::Description =>
            {
                move_cursor_to_line_edge(
                    &self.commit_description,
                    &mut self.commit_description_cursor,
                    false,
                );
            }
            KeyCode::End
                if self.commit_popup
                    && !self.commit_confirm
                    && self.commit_field == CommitField::Description =>
            {
                move_cursor_to_line_edge(
                    &self.commit_description,
                    &mut self.commit_description_cursor,
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
                if self.commit_popup
                    && !self.commit_confirm
                    && self.commit_field == CommitField::Title =>
            {
                delete_at_cursor(&mut self.commit_title, self.commit_title_cursor);
            }
            KeyCode::Delete
                if self.commit_popup
                    && !self.commit_confirm
                    && self.commit_field == CommitField::Description =>
            {
                delete_at_cursor(&mut self.commit_description, self.commit_description_cursor);
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
                self.focus = match self.focus {
                    Focus::Status => Focus::Graph,
                    // The details pane is focusable so long commit messages can
                    // be scrolled; skip it when the local-changes row is selected.
                    Focus::Graph if self.historical_commit.is_some() => Focus::Details,
                    Focus::Graph => Focus::Diff,
                    Focus::Details => Focus::Diff,
                    Focus::Diff => {
                        self.ensure_status_selection();
                        Focus::Status
                    }
                    Focus::Commit => {
                        self.commit_field = if self.commit_confirm {
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
                        Focus::Commit
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
                self.open_graph_action(GraphAction::Branch)
            }
            KeyCode::Char('t')
                if self.focus == Focus::Graph && self.selected_conflict_path().is_none() =>
            {
                self.open_graph_action(GraphAction::Tag)
            }
            KeyCode::Char('r') if self.focus == Focus::Graph => {
                self.open_graph_action(GraphAction::Reset)
            }
            KeyCode::Char('c') if self.focus == Focus::Graph => {
                self.open_graph_action(GraphAction::Checkout)
            }
            KeyCode::Char('w') if self.focus == Focus::Graph => self.open_reword_popup(),
            KeyCode::Char('e')
                if self.focus == Focus::Graph && self.selected_conflict_path().is_none() =>
            {
                self.open_edit_confirmation()
            }
            KeyCode::Char('i') if self.focus == Focus::Graph => self.open_rebase_plan(),
            KeyCode::Char('R') if self.focus == Focus::Graph => {
                self.open_graph_action(GraphAction::RebaseOnto)
            }
            KeyCode::Char('o')
                if self.selected_conflict_path().is_some() && self.modal_closed() =>
            {
                self.choose_selected_conflict_side(ConflictSide::Ours);
            }
            KeyCode::Char('t')
                if self.selected_conflict_path().is_some() && self.modal_closed() =>
            {
                self.choose_selected_conflict_side(ConflictSide::Theirs);
            }
            KeyCode::Char('b')
                if self.selected_conflict_path().is_some() && self.modal_closed() =>
            {
                self.choose_selected_conflict_side(ConflictSide::Base);
            }
            KeyCode::Char('e')
                if self.selected_conflict_path().is_some() && self.modal_closed() =>
            {
                self.open_selected_conflict_editor();
            }
            KeyCode::Char('w') if self.graph_action.is_none() && !self.commit_popup => {
                self.action_error =
                    Some("Focus the Graph with Tab and select a commit first".into());
            }
            KeyCode::Char('p') if self.focus == Focus::Graph && !self.selected_current_branch() => {
                self.open_graph_action(GraphAction::CherryPick)
            }
            KeyCode::Char('x') if self.focus == Focus::Graph => {
                self.open_graph_action(GraphAction::Revert)
            }
            KeyCode::Char('u') if self.modal_closed() => self.open_undo_popup(),
            KeyCode::Char('/') if self.modal_closed() => self.open_search_popup(),
            KeyCode::Char('L') if self.diff_truncated.is_some() && self.modal_closed() => {
                self.load_full_diff()
            }
            KeyCode::Char('x')
                if self.focus == Focus::Status
                    && self.status_section == StatusSection::Unstaged =>
            {
                self.open_discard_popup()
            }
            KeyCode::Char('n') if self.focus == Focus::Graph => {
                self.open_local_branch_action(GraphAction::Rename)
            }
            KeyCode::Char('d') if self.focus == Focus::Graph => self.open_delete_action(),
            KeyCode::Char('p') if self.focus == Focus::Graph && self.selected_current_branch() => {
                self.open_graph_action(GraphAction::Pull)
            }
            KeyCode::Char('F') if self.focus == Focus::Graph => self.open_remote_action(false),
            KeyCode::Char('+') if self.focus == Focus::Graph => self.open_remote_action(true),
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
                self.confirm_selected_conflict_result();
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
                if self.commit_popup && key.modifiers.contains(KeyModifiers::CONTROL) =>
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
                self.open_local_branch_action(GraphAction::CheckoutBranch);
            }
            KeyCode::F(2) if self.commit_popup => {
                self.commit_changes();
            }
            KeyCode::Esc if self.commit_popup => {
                let return_to_graph = matches!(
                    self.commit_mode,
                    CommitMode::Reword(_) | CommitMode::Edit(_) | CommitMode::PlanReword(_)
                );
                self.commit_popup = false;
                self.commit_confirm = false;
                self.commit_mode = CommitMode::Normal;
                self.focus = if return_to_graph {
                    Focus::Graph
                } else {
                    Focus::Status
                };
            }
            KeyCode::Backspace if self.commit_popup => match self.commit_field {
                CommitField::Title => {
                    backspace_at_cursor(&mut self.commit_title, &mut self.commit_title_cursor)
                }
                CommitField::Description => backspace_at_cursor(
                    &mut self.commit_description,
                    &mut self.commit_description_cursor,
                ),
                CommitField::Commit | CommitField::Cancel => {}
            },
            KeyCode::Enter if self.commit_popup => match self.commit_field {
                CommitField::Title => self.commit_field = CommitField::Description,
                CommitField::Description => self.commit_field = CommitField::Commit,
                CommitField::Commit => self.commit_changes(),
                CommitField::Cancel => {
                    self.commit_popup = false;
                    self.commit_confirm = false;
                    self.focus = Focus::Status;
                }
            },
            KeyCode::Char(c)
                if self.commit_popup
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                match self.commit_field {
                    CommitField::Title => {
                        insert_at_cursor(&mut self.commit_title, &mut self.commit_title_cursor, c)
                    }
                    CommitField::Description => insert_at_cursor(
                        &mut self.commit_description,
                        &mut self.commit_description_cursor,
                        c,
                    ),
                    CommitField::Commit | CommitField::Cancel => {}
                }
            }
            _ if self.graph_action.is_some() || self.commit_popup || self.abort_rebase_popup => {}
            KeyCode::Char(' ') if self.selected_conflict_path().is_some() => {}
            KeyCode::Char(' ') if self.focus == Focus::Diff => {
                if self.line_mode {
                    self.apply_selected_diff_line();
                } else if self.hunk_mode {
                    self.apply_selected_diff_hunk();
                } else {
                    self.apply_selected_file_changes();
                }
            }
            KeyCode::Char(' ') if self.focus == Focus::Status => {
                if self.historical_commit.is_some() {
                    return false;
                }
                if self.line_mode {
                    self.apply_selected_diff_line();
                } else if self.hunk_mode {
                    self.apply_selected_diff_hunk();
                } else {
                    self.apply_selected_file_changes();
                }
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
                self.open_amend_popup();
            }
            KeyCode::Char('f')
                if matches!(self.focus, Focus::Status | Focus::Graph)
                    && self.commit_actions_available()
                    && self.rebase_edit.is_none() =>
            {
                self.pending_fixup = true;
                self.fixup_origin = self.selected_graph;
                self.focus = Focus::Graph;
            }
            KeyCode::Char('v') if !self.line_mode => {
                self.diff_mode = match self.diff_mode {
                    DiffMode::Unified => DiffMode::SideBySide,
                    DiffMode::SideBySide => DiffMode::Unified,
                };
                self.diff_scroll = 0;
                self.diff_scroll_x = 0;
            }
            KeyCode::Char('h')
                if matches!(self.focus, Focus::Status | Focus::Diff)
                    && self.three_way_view.is_none() =>
            {
                self.toggle_hunk_mode();
            }
            KeyCode::Char('l')
                if matches!(self.focus, Focus::Status | Focus::Diff)
                    && self.three_way_view.is_none() =>
            {
                self.toggle_line_mode();
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
                    let longest = view
                        .rows
                        .iter()
                        .flat_map(|row| {
                            [&row.base, &row.ours, &row.theirs]
                                .into_iter()
                                .filter_map(|cell| {
                                    cell.as_ref().map(|cell| cell.text.chars().count())
                                })
                        })
                        .chain(view.ours_diff.iter().map(|line| line.text.chars().count()))
                        .chain(
                            view.theirs_diff
                                .iter()
                                .map(|line| line.text.chars().count()),
                        )
                        .chain(
                            view.result_diff
                                .iter()
                                .map(|line| line.text.chars().count()),
                        )
                        .max()
                        .unwrap_or(0);
                    view.scroll_x = (view.scroll_x + 4).min(longest);
                }
            }
            KeyCode::Char('j') | KeyCode::Down if self.focus == Focus::Status => {
                self.move_status_selection(1, key.modifiers.contains(KeyModifiers::SHIFT));
            }
            KeyCode::Char('k') | KeyCode::Up if self.focus == Focus::Status => {
                self.move_status_selection(-1, key.modifiers.contains(KeyModifiers::SHIFT));
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
                    view.scroll_y = (view.scroll_y + 1).min(content_rows.saturating_sub(1));
                } else {
                    self.diff_scroll = self.diff_scroll.saturating_add(1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up if self.focus == Focus::Diff => {
                if let Some(view) = &mut self.three_way_view {
                    view.scroll_y = view.scroll_y.saturating_sub(1);
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
            KeyCode::Char('g') => {
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

    fn handle_remote_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') => true,
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Status => Focus::Graph,
                    Focus::Graph => {
                        self.ensure_status_selection();
                        Focus::Status
                    }
                    Focus::Diff | Focus::Details => Focus::Graph,
                    Focus::Commit => Focus::Graph,
                };
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

    fn move_selection_while_remote(&mut self, delta: i32, extend: bool) {
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

    fn handle_mouse(&mut self, x: u16, y: u16) {
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
            let panes = Layout::vertical([Constraint::Percentage(50), Constraint::Min(0)])
                .split(self.staging_area);
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

    fn handle_mouse_scroll(&mut self, x: u16, y: u16, delta: i32) {
        if !self.modal_closed() {
            return;
        }
        let position = Position { x, y };
        if self.diff_area.contains(position) {
            if let Some(view) = &mut self.three_way_view {
                if delta < 0 {
                    view.scroll_y = view.scroll_y.saturating_sub(1);
                } else {
                    view.scroll_y = (view.scroll_y + 1).min(view.rows.len().saturating_sub(1));
                }
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

#[path = "app/render.rs"]
mod render;

#[path = "app/workflows.rs"]
mod workflows;

/// Query the terminal background and ANSI palette. The palette lets dimmed
/// graph colors retain the user's theme hue while using a darker RGB shade.
#[cfg(unix)]
fn detect_terminal_colors() -> (Option<Color>, TerminalAnsiPalette) {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

    if enable_raw_mode().is_err() {
        return (None, [None; 16]);
    }
    let result = (|| {
        let mut out = std::io::stdout();
        out.write_all(b"\x1b]11;?\x1b\\").ok()?;
        out.flush().ok()?;

        let fd = std::io::stdin().as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Some((None, [None; 16]));
        }
        // Non-blocking reads so a non-answering terminal cannot hang the app.
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        let mut acc = Vec::new();
        let mut buf = [0u8; 256];
        // Terminals may answer an OSC query with ST (`ESC \\`) or BEL. Cap the
        // wait low: a non-responding terminal must not delay startup.
        let deadline = Instant::now() + Duration::from_millis(150);
        loop {
            if Instant::now() >= deadline {
                break;
            }
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                break;
            }
            if n == 0 {
                break;
            }
            acc.extend_from_slice(&buf[..n as usize]);
            if acc.windows(2).any(|w| w == b"\x1b\\") || acc.contains(&0x07) {
                break;
            }
        }
        let background = parse_osc11(&acc);

        let palette_query =
            b"\x1b]4;0;?;1;?;2;?;3;?;4;?;5;?;6;?;7;?;8;?;9;?;10;?;11;?;12;?;13;?;14;?;15;?\x1b\\";
        let palette_start = acc.len();
        if out.write_all(palette_query).is_err() || out.flush().is_err() {
            unsafe {
                libc::fcntl(fd, libc::F_SETFL, flags);
            }
            return Some((background, [None; 16]));
        }
        let palette_deadline = Instant::now() + Duration::from_millis(60);
        while Instant::now() < palette_deadline {
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                break;
            }
            if n == 0 {
                break;
            }
            acc.extend_from_slice(&buf[..n as usize]);
            let palette = parse_osc4(&acc[palette_start..]);
            if (0..=15).all(|index| palette[index].is_some()) {
                break;
            }
        }
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, flags);
        }
        Some((background, parse_osc4(&acc[palette_start..])))
    })();
    let _ = disable_raw_mode();
    result.unwrap_or((None, [None; 16]))
}

#[cfg(not(unix))]
fn detect_terminal_colors() -> (Option<Color>, TerminalAnsiPalette) {
    (None, [None; 16])
}

/// Parse an OSC 11 response, e.g. `\x1b]11;rgb:0a0a/0b0b/0c0c\x1b\\`,
/// `rgba:...`, or `#RRGGBB`. Tolerates 16-bit and 8-bit channel widths.
pub fn parse_osc11(acc: &[u8]) -> Option<Color> {
    let s = String::from_utf8_lossy(acc);
    const PREFIX: &str = "\x1b]11;";
    let start = s.find(PREFIX)? + PREFIX.len();
    let rest = &s[start..];
    let end = rest
        .find("\x1b\\")
        .or_else(|| rest.find('\x07'))
        .unwrap_or(rest.len());
    let payload = rest[..end].trim();

    parse_rgb_color(payload)
}

/// Parse the responses to an OSC 4 ANSI palette query.
pub fn parse_osc4(acc: &[u8]) -> TerminalAnsiPalette {
    let mut palette = [None; 16];
    let text = String::from_utf8_lossy(acc);
    for response in text.split("\x1b]4;").skip(1) {
        let end = response
            .find("\x1b\\")
            .or_else(|| response.find('\x07'))
            .unwrap_or(response.len());
        let Some((index, value)) = response[..end].split_once(';') else {
            continue;
        };
        let Ok(index) = index.parse::<usize>() else {
            continue;
        };
        if index < palette.len() {
            palette[index] = parse_rgb_color(value.trim());
        }
    }
    palette
}

fn parse_rgb_color(payload: &str) -> Option<Color> {
    let channel = |hex: &str| {
        // 16-bit "0a0a" or 8-bit "0a" -> use the most significant byte.
        let hex = hex.trim().strip_prefix("0x").unwrap_or(hex.trim());
        let take = hex.len().clamp(1, 2);
        u8::from_str_radix(&hex[..take], 16).ok()
    };
    if let Some(hex) = payload.strip_prefix('#') {
        return Some(Color::Rgb(
            channel(hex.get(0..2)?)?,
            channel(hex.get(2..4)?)?,
            channel(hex.get(4..6)?)?,
        ));
    }
    let rgb = payload
        .strip_prefix("rgb:")
        .or_else(|| payload.strip_prefix("rgba:"))?;
    let mut channels = rgb.split('/');
    Some(Color::Rgb(
        channel(channels.next()?)?,
        channel(channels.next()?)?,
        channel(channels.next()?)?,
    ))
}

#[cfg(test)]
mod tests;

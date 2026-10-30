//! Display-column clipping, wrapping, text editing and shared text styling.

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

/// Truncate `s` to at most `max` display columns, adding a trailing ellipsis when
/// cut. `max == 0` yields an empty string.
pub(super) fn truncate(s: &str, max: usize) -> String {
    if Line::from(s).width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut width = 0;
    for ch in s.graphemes(true) {
        let cells = Line::from(ch).width();
        if width + cells > max - 1 {
            break;
        }
        out.push_str(ch);
        width += cells;
    }
    out.push('…');
    out
}

pub(super) fn truncate_start(s: &str, max: usize) -> String {
    if Line::from(s).width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut tail = Vec::new();
    let mut width = 0;
    for ch in s.graphemes(true).rev() {
        let cells = Line::from(ch).width();
        if width + cells > max - 1 {
            break;
        }
        tail.push(ch);
        width += cells;
    }
    format!("…{}", tail.into_iter().rev().collect::<String>())
}

pub(super) fn single_line_cursor(
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
pub(super) struct WrappedLine {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) ends_with_newline: bool,
}

pub(super) fn char_width(character: char) -> usize {
    Line::from(character.to_string()).width().max(1)
}

/// Wrap `text` to `width` display columns, preferring spaces but preserving
/// every character (including the spaces at the break) so rendering and cursor
/// math agree.
pub(super) fn wrap_text(text: &str, width: usize) -> Vec<WrappedLine> {
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

pub(super) fn wrapped_cursor(
    lines: &[WrappedLine],
    chars: &[char],
    cursor: usize,
) -> (usize, usize) {
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

pub(super) fn wrapped_text_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    let chars: Vec<char> = text.chars().collect();
    wrap_text(text, width)
        .into_iter()
        .map(|line| Line::from(chars[line.start..line.end].iter().collect::<String>()))
        .collect()
}

pub(super) fn wrapped_text_cursor(text: &str, cursor_index: usize, area: Rect) -> (Position, u16) {
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

pub(super) fn insert_at_cursor(text: &mut String, cursor: &mut usize, character: char) {
    let byte = text
        .char_indices()
        .nth(*cursor)
        .map_or(text.len(), |(byte, _)| byte);
    text.insert(byte, character);
    *cursor += 1;
}

pub(super) fn backspace_at_cursor(text: &mut String, cursor: &mut usize) {
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

pub(super) fn delete_at_cursor(text: &mut String, cursor: usize) {
    let Some((start, _)) = text.char_indices().nth(cursor) else {
        return;
    };
    let end = text
        .char_indices()
        .nth(cursor + 1)
        .map_or(text.len(), |(byte, _)| byte);
    text.replace_range(start..end, "");
}

pub(super) fn move_cursor_vertical(text: &str, cursor: &mut usize, delta: i32) {
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

pub(super) fn move_cursor_to_line_edge(text: &str, cursor: &mut usize, end: bool) {
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
pub(super) fn styled_key_hints(items: &[(&str, &str)], key_color: Color) -> Line<'static> {
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
pub(super) fn styled_key_hints_fitting(
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

pub(super) fn highlight_search_matches(lines: &mut [Line<'static>], query: &str) {
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

pub(super) fn pad_colored_lines<'a>(lines: &mut [Line<'a>], width: usize) {
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

/// Clip a styled line to a terminal-cell width without flattening its colors.
pub(super) fn clip_line(line: &Line<'_>, max_width: usize) -> Line<'static> {
    let mut spans = Vec::new();
    let mut used = 0;
    'outer: for span in &line.spans {
        let mut content = String::new();
        for grapheme in span.content.graphemes(true) {
            let cells = Line::from(grapheme).width();
            if used + cells > max_width {
                if !content.is_empty() {
                    spans.push(Span::styled(content, span.style));
                }
                break 'outer;
            }
            content.push_str(grapheme);
            used += cells;
        }
        if !content.is_empty() {
            spans.push(Span::styled(content, span.style));
        }
    }
    Line::from(spans).style(line.style)
}

/// Find a suffix at a display-column boundary. If the boundary cuts a wide
/// glyph, replace its visible remainder with spaces to keep columns aligned.
pub(super) fn column_suffix(text: &str, columns: usize) -> (&str, usize) {
    if columns == 0 {
        return (text, 0);
    }
    let mut used = 0;
    for (byte, grapheme) in text.grapheme_indices(true) {
        let width = Line::from(grapheme).width();
        if used >= columns && width > 0 {
            return (&text[byte..], used - columns);
        }
        used += width;
    }
    ("", used.saturating_sub(columns))
}

pub(super) fn skip_columns(text: &str, columns: usize) -> String {
    let (suffix, padding) = column_suffix(text, columns);
    format!("{}{suffix}", " ".repeat(padding))
}

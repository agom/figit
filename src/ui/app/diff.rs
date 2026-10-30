//! Diff parsing, layout and syntax-highlighted row rendering.
//!
//! This engine does not access App, Git, or terminal lifecycle state.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use similar::{ChangeTag, TextDiff};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style as SynStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use unicode_segmentation::UnicodeSegmentation;

use super::text::{column_suffix, skip_columns};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DiffMode {
    Unified,
    SideBySide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DiffKind {
    Meta,
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone)]
pub(super) struct DiffLine {
    pub(super) old_no: Option<u32>,
    pub(super) new_no: Option<u32>,
    pub(super) text: String,
    pub(super) kind: DiffKind,
}

pub(super) fn parse_diff(diff: &str) -> Vec<DiffLine> {
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

pub(super) fn diff_hunk_headers(lines: &[DiffLine]) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            (line.kind == DiffKind::Meta && line.text.starts_with("@@")).then_some(index)
        })
        .collect()
}

#[cfg(test)]
pub(super) fn mark_selected_diff_hunk(lines: &mut [Line<'static>], selected_hunk: usize) {
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

pub(super) fn emphasize_selected_diff_hunk(line: &mut Line<'static>) {
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

pub(super) fn emphasize_selected_diff_line(line: &mut Line<'static>, kind: DiffKind) {
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

pub(super) fn diff_color(kind: DiffKind) -> Color {
    match kind {
        DiffKind::Meta => Color::LightCyan,
        DiffKind::Context => Color::Gray,
        DiffKind::Added => Color::LightGreen,
        DiffKind::Removed => Color::LightRed,
    }
}

pub(super) fn diff_background(kind: DiffKind) -> Option<Color> {
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
pub(super) fn render_conflict_diff(
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
pub(super) enum DiffRow {
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
    pub(super) fn input_indices(self) -> (Option<usize>, Option<usize>) {
        match self {
            DiffRow::Unified { index, .. } => (Some(index), None),
            DiffRow::Side { old, new } => (old, new),
        }
    }
}

/// Lays a diff out into rows without doing any syntax highlighting.
pub(super) fn diff_layout(lines: &[DiffLine], mode: DiffMode) -> Vec<DiffRow> {
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

#[cfg(test)]
pub(super) fn render_diff(
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
pub(super) fn render_diff_row(
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
    let code = line.text.get(1..).unwrap_or(&line.text);
    let (content_spans, scroll_padding) = styled_suffix(
        highlight_code(code, path, line.kind, highlight),
        offset_x,
        width.saturating_sub(number_width + 1),
    );
    let mut spans = vec![Span::styled(number_text, Style::default().fg(number_color))];
    let mut marker_style = Style::default().fg(diff_color(line.kind));
    if let Some(bg) = diff_background(line.kind) {
        marker_style = marker_style.bg(bg);
    }
    spans.push(Span::styled(marker.to_string(), marker_style));
    if scroll_padding > 0 {
        spans.push(Span::styled(
            " ".repeat(scroll_padding),
            Style::default().bg(diff_background(line.kind).unwrap_or(Color::Reset)),
        ));
    }
    let visible_content = content_spans
        .iter()
        .map(|span| Line::from(span.content.as_ref()).width())
        .sum::<usize>();
    spans.extend(content_spans);
    let used = number_width + 1 + scroll_padding + visible_content;
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
    let code_spans = highlight_code_with_inline(full_code, path, line.kind, highlight, inline);
    let (code_spans, padding) = styled_suffix(code_spans, offset_x, usize::MAX);
    rendered.push(Span::styled(" ".repeat(padding), prefix_style));
    rendered.extend(code_spans);
    Line::from(rendered)
}

/// Highlight the complete source line before cropping it horizontally. Parsing
/// only the visible suffix loses lexical context when it begins inside a token.
fn highlight_code_with_inline(
    code: &str,
    path: &str,
    kind: DiffKind,
    highlight: bool,
    ranges: Option<&[(usize, usize)]>,
) -> Vec<Span<'static>> {
    let spans = highlight_code(code, path, kind, highlight);
    let Some(ranges) = ranges.filter(|ranges| !ranges.is_empty()) else {
        return spans;
    };
    let Some(background) = bright_diff_background(kind) else {
        return spans;
    };

    let mut output = Vec::with_capacity(spans.len() + ranges.len());
    let mut span_start = 0;
    for span in spans {
        let text = span.content.to_string();
        let span_end = span_start + text.len();
        let mut cursor = 0;
        for &(range_start, range_end) in ranges {
            if range_end <= span_start + cursor {
                continue;
            }
            if range_start >= span_end {
                break;
            }
            let start = range_start.saturating_sub(span_start).min(text.len());
            let end = range_end.saturating_sub(span_start).min(text.len());
            if start > cursor {
                output.push(Span::styled(text[cursor..start].to_string(), span.style));
            }
            let changed_start = start.max(cursor);
            if changed_start < end {
                let mut style = span.style;
                style.bg = Some(background);
                output.push(Span::styled(text[changed_start..end].to_string(), style));
                cursor = end;
            }
        }
        if cursor < text.len() {
            output.push(Span::styled(text[cursor..].to_string(), span.style));
        }
        span_start = span_end;
    }
    output
}

/// Keep syntax styles while removing a display-column prefix and limiting the
/// result to a visible width. A partially cut wide glyph becomes padding.
fn styled_suffix(
    spans: Vec<Span<'static>>,
    offset: usize,
    max_width: usize,
) -> (Vec<Span<'static>>, usize) {
    let mut skipped = offset;
    let mut width = 0;
    let mut padding = 0;
    let mut output = Vec::with_capacity(spans.len());
    for span in spans {
        let text = span.content.to_string();
        let span_width = Line::from(text.as_str()).width();
        if skipped > 0 && skipped >= span_width {
            skipped -= span_width;
            continue;
        }
        let (suffix, partial) = column_suffix(&text, skipped);
        skipped = 0;
        if partial > 0 {
            let fill = partial.min(max_width.saturating_sub(width));
            if fill > 0 {
                output.push(Span::styled(" ".repeat(fill), span.style));
                padding += fill;
                width += fill;
            }
        }
        let available = max_width.saturating_sub(width);
        let mut byte_end = 0;
        let mut text_width = 0;
        for (byte, grapheme) in suffix.grapheme_indices(true) {
            let cells = Line::from(grapheme).width();
            if text_width + cells > available {
                break;
            }
            byte_end = byte + grapheme.len();
            text_width += cells;
        }
        if byte_end > 0 {
            output.push(Span::styled(suffix[..byte_end].to_string(), span.style));
            width += text_width;
        }
        if byte_end < suffix.len() || width >= max_width {
            break;
        }
    }
    (output, padding)
}

type ChangeRanges = Vec<(usize, usize)>;

/// Paired removed/added lines that differ this much are left unemphasized.
/// Highlighting unrelated lines token by token is just noise.
const INLINE_EMPHASIS_MIN_RATIO: f32 = 0.4;

pub(super) fn inline_change_ranges(old: &str, new: &str) -> (ChangeRanges, ChangeRanges) {
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

pub(super) fn syntax_highlight(code: &str, path: &str, fallback: Color) -> Vec<Span<'static>> {
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
    if syntax_ref.name == "Plain Text" {
        return vec![Span::styled(
            code.to_string(),
            Style::default().fg(fallback),
        )];
    }
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

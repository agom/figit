//! Conflict markers, source alignment and aligned text columns.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use similar::{DiffTag, TextDiff};

use super::diff::syntax_highlight;
use super::text::{skip_columns, truncate, truncate_start};
use crate::backend::ConflictSide;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ThreeWayCell {
    pub(super) number: usize,
    pub(super) text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ThreeWayRow {
    pub(super) base: Option<ThreeWayCell>,
    pub(super) ours: Option<ThreeWayCell>,
    pub(super) theirs: Option<ThreeWayCell>,
    pub(super) result: Option<ThreeWayCell>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConflictHunk {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) ours: String,
    pub(super) base: Option<String>,
    pub(super) theirs: String,
}

pub(super) struct AlignedSide {
    pub(super) lines: Vec<Option<ThreeWayCell>>,
    pub(super) insertions: Vec<Vec<ThreeWayCell>>,
}

pub(super) fn conflict_hunks(text: &str) -> Vec<ConflictHunk> {
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

pub(super) fn replace_conflict_hunk(
    text: &str,
    hunk_index: usize,
    side: ConflictSide,
) -> Option<String> {
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

pub(super) fn display_lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

pub(super) fn fit_to_cells(text: &str, width: usize) -> String {
    truncate(text, width)
}

pub(super) fn align_conflict_side(base: &str, side: &str) -> AlignedSide {
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

pub(super) fn three_way_column(
    cell: Option<&ThreeWayCell>,
    base: Option<&ThreeWayCell>,
    path: &str,
    width: usize,
    horizontal_scroll: usize,
    color: Color,
    number_width: usize,
) -> Line<'static> {
    let number_width = number_width.min(width);
    let text_width = width.saturating_sub(number_width);
    let changed = cell.is_some_and(|cell| base.is_none_or(|base| cell.text != base.text));
    let number = cell.map_or_else(
        || " ".repeat(number_width),
        |cell| {
            format!(
                "{:>width$} ",
                cell.number,
                width = number_width.saturating_sub(1)
            )
        },
    );
    let number = truncate_start(&number, number_width);
    let text = cell.map_or_else(String::new, |cell| {
        fit_to_cells(&skip_columns(&cell.text, horizontal_scroll), text_width)
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

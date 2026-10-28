//! Loading, caching and selection of the active diff.

use std::borrow::Cow;

use super::*;

/// Diffs longer than this are truncated before parsing, which is what keeps
/// very large files from stalling the UI. Press `L` to load the full diff on
/// demand.
pub(super) const MAX_RENDERED_DIFF_LINES: usize = 20_000;

impl App {
    /// Replace the parsed diff, keeping the derived caches (hunk headers and
    /// the rendered-line cache) in sync. Always go through this rather than
    /// assigning `diff_lines` directly.
    pub(super) fn set_diff_lines(&mut self, lines: Vec<DiffLine>) {
        self.hunk_header_indices = diff_hunk_headers(&lines);
        self.diff_lines = lines;
        self.diff_cache = None;
    }

    pub(super) fn apply_diff_text(&mut self, diff: String) {
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

    pub(super) fn load_full_diff(&mut self) {
        if self.diff_truncated.is_none() {
            return;
        }
        self.diff_full = true;
        self.refresh_diff();
    }

    pub(super) fn refresh_diff(&mut self) {
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
            let versions = self.backend.as_ref()?.conflict_versions(&path).ok().flatten()?;
            if let Some(previous) = previous_three_way_view.filter(|view| view.path == path) {
                if previous.versions == versions { return Some(previous); }
                if previous.result_preview {
                    self.action_error = Some("Conflict file changed; preview cancelled. Review the current contents before confirming.".into());
                }
                let mut view = ThreeWayView::new(path, versions);
                view.hunk_index = previous.hunk_index.min(view.hunks.len().saturating_sub(1));
                view.scroll_y = previous.scroll_y;
                view.scroll_x = previous.scroll_x;
                view.clamp_scroll();
                Some(view)
            } else {
                Some(ThreeWayView::new(path, versions))
            }
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

    /// Rebuilds the cached diff layout when the diff or its mode changed.
    pub(super) fn ensure_diff_layout(&mut self) {
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
    pub(super) fn selected_hunk_range(&self) -> Option<(usize, usize)> {
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
    pub(super) fn materialize_diff_row(&self, row: DiffRow, width: usize) -> Line<'static> {
        let path = self
            .diff_path
            .as_ref()
            .map(GitPath::display)
            .unwrap_or(Cow::Borrowed(""));
        let show_marker = self.hunk_mode || self.line_mode;
        let mut selected_marker = false;
        let mut line = render_diff_row(
            &self.diff_lines,
            row,
            width.saturating_sub(if show_marker { 2 } else { 0 }),
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
                        selected_marker = true;
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
                        selected_marker = old == Some(start) || new == Some(start);
                        emphasize_selected_diff_hunk(&mut line);
                    }
                }
            }
        }
        if show_marker {
            line.spans.insert(
                0,
                Span::styled(
                    if selected_marker { "▸ " } else { "  " },
                    Style::default()
                        .fg(Color::LightCyan)
                        .bg(line.style.bg.unwrap_or(Color::Reset)),
                ),
            );
        }
        pad_colored_lines(std::slice::from_mut(&mut line), width);
        line
    }
}

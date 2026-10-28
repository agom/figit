//! Graph indexes, branch reachability and selection.

use super::*;

#[derive(Clone)]
pub(super) struct GraphSelectionSnapshot {
    pub(super) commit_id: Option<String>,
    pub(super) row: usize,
    pub(super) had_local_row: bool,
    pub(super) selected_local_row: bool,
}

pub(super) fn current_branch_ids(graph: &GraphModel) -> HashSet<String> {
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

impl App {
    pub(super) fn rebuild_graph_indexes(&mut self) {
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

    pub(super) fn select_graph_entry(&mut self, delta: i32) {
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

    pub(super) fn select_graph_index(&mut self, selected: usize) {
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

    pub(super) fn selected_graph_node_col(&self) -> Option<usize> {
        let local_offset = usize::from(!self.local_status.is_empty());
        let row_index = self.selected_graph.checked_sub(local_offset)?;
        self.rows.get(row_index)?.node_col
    }

    pub(super) fn keep_selected_graph_node_visible(&mut self) {
        self.scroll_x = self
            .selected_graph_node_col()
            .map_or(0, |node_col| self.scroll_x.min(node_col));
    }
}

impl App {
    pub(super) fn selected_commit_id(&self) -> Option<String> {
        let has_local = !self.local_status.is_empty();
        if self.selected_graph < usize::from(has_local) {
            return None;
        }
        let row = self.selected_graph - usize::from(has_local);
        self.rows
            .get(row)
            .map(|row| self.graph.commits[row.commit_index].id.clone())
    }

    /// Human-readable label for the selected commit: a branch/tag name when one
    /// points at it, otherwise the short id.
    pub(super) fn selected_commit_label(&self) -> String {
        let Some(id) = self.selected_commit_id() else {
            return String::new();
        };
        let short = id.chars().take(7).collect::<String>();
        let reference = self.graph_display_refs.get(&id).and_then(|refs| {
            refs.iter()
                .find(|reference| matches!(reference.kind, RefKind::LocalBranch | RefKind::Tag))
                .or_else(|| refs.first())
        });
        match reference {
            Some(reference) => format!("{} ({short})", reference.name),
            None => short,
        }
    }

    pub(super) fn selected_current_branch(&self) -> bool {
        self.graph.head_name.is_some()
            && self
                .graph
                .head_id
                .as_ref()
                .is_some_and(|head| self.selected_commit_id().as_deref() == Some(head.as_str()))
    }

    pub(super) fn selected_local_branch(&self) -> Option<String> {
        let commit = self.selected_commit_id()?;
        let branches = self
            .graph
            .refs
            .iter()
            .filter(|reference| {
                reference.kind == crate::core::model::RefKind::LocalBranch && reference.id == commit
            })
            .collect::<Vec<_>>();
        if self.graph.head_id.as_deref() == Some(commit.as_str()) {
            if let Some(head_name) = self.graph.head_name.as_deref() {
                if branches.iter().any(|branch| branch.name == head_name) {
                    return Some(head_name.to_string());
                }
            }
        }
        branches
            .into_iter()
            .min_by(|left, right| left.name.cmp(&right.name))
            .map(|reference| reference.name.clone())
    }

    /// A tag pointing at the selected commit, used to push releases.
    pub(super) fn selected_tag(&self) -> Option<String> {
        let commit = self.selected_commit_id()?;
        self.graph
            .refs
            .iter()
            .filter(|reference| {
                reference.kind == crate::core::model::RefKind::Tag && reference.id == commit
            })
            .map(|reference| reference.name.clone())
            .min()
    }

    pub(super) fn restore_graph_selection(&mut self, selection: GraphSelectionSnapshot) {
        self.focus = Focus::Graph;
        let has_local = !self.local_status.is_empty();
        let local_offset = usize::from(has_local);
        let selected = if selection.selected_local_row && has_local {
            Some(0)
        } else {
            selection
                .commit_id
                .as_ref()
                .and_then(|id| {
                    self.rows
                        .iter()
                        .position(|row| self.graph.commits[row.commit_index].id == *id)
                })
                .or_else(|| {
                    (!self.rows.is_empty()).then(|| {
                        selection
                            .row
                            .saturating_sub(usize::from(selection.had_local_row))
                            .min(self.rows.len() - 1)
                            + local_offset
                    })
                })
        };
        if let Some(selected) = selected {
            self.select_graph_index(selected);
        } else {
            self.selected_graph = 0;
        }
    }
}

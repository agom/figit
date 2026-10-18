use crate::core::model::{Commit, GraphModel};
use std::collections::HashSet;

/// Glyph drawn in a single lane column of a graph row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellGlyph {
    Empty,
    Vertical,   // │  lane passing through the row
    Node,       // ●  centered commit node
    MergeNode,  // ▲  merge commit node
    MergeRight, // ╯  lane arcing into the node from the right
    Horizontal, // ─  connector spanning toward a new lane
    BranchDown, // ╮  a new lane arcing down from the node
}

impl CellGlyph {
    pub fn char(self) -> char {
        match self {
            CellGlyph::Empty => ' ',
            CellGlyph::Vertical => '│',
            CellGlyph::Node => '●',
            CellGlyph::MergeNode => '▲',
            CellGlyph::MergeRight => '╯',
            CellGlyph::Horizontal => '─',
            CellGlyph::BranchDown => '╮',
        }
    }
}

#[derive(Debug, Clone)]
pub struct GraphRow {
    /// Glyph per lane column (left-aligned).
    pub cells: Vec<CellGlyph>,
    /// Palette index per cell: which branch's color each glyph belongs to.
    /// Horizontal merge connectors use the color of the merged-in branch they
    /// lead to, not the column they cross; everything else uses the color of
    /// the lane it sits on.
    pub colors: Vec<usize>,
    /// Column where the commit node is drawn, if any.
    pub node_col: Option<usize>,
    /// Index of the commit within `GraphModel::commits`.
    pub commit_index: usize,
}

/// Number of lane columns used by the widest row.
pub fn max_lanes(rows: &[GraphRow]) -> usize {
    rows.iter().map(|r| r.cells.len()).max().unwrap_or(0)
}

/// One lane in the incremental column model: the commit it is connected to and
/// the stable color of the branch it carries.
///
/// Lanes track commit **ids** rather than positions so layout can be resumed
/// across history pages: a lane whose target has not been loaded yet keeps its
/// id until that commit arrives.
#[derive(Debug, Clone)]
struct Lane {
    id: String,
    color: usize,
}

/// Resumable lane layout.
///
/// Feed commits in graph order, in as many chunks as you like. The rows are
/// identical to laying the whole history out at once, and `commit_index` values
/// are global, so paging never reshuffles or recolors earlier rows.
#[derive(Debug, Default)]
pub struct Layout {
    columns: Vec<Option<Lane>>,
    next_color: usize,
    next_index: usize,
    targets: Vec<usize>,
    taken: Vec<bool>,
    extra: Vec<(usize, usize)>,
}

impl Layout {
    pub fn new() -> Self {
        Self::default()
    }

    /// Lay out the next chunk of commits.
    ///
    /// `known` restricts which parents count as reachable. Pass the full set of
    /// loaded ids once the whole history is present so grafted or
    /// shallow-clone boundaries do not leave a lane dangling below the graph;
    /// pass `None` while more pages may still arrive.
    pub fn extend(&mut self, commits: &[Commit], known: Option<&HashSet<&str>>) -> Vec<GraphRow> {
        let mut rows = Vec::with_capacity(commits.len());
        for commit in commits {
            rows.push(self.push(commit, known));
        }
        rows
    }

    fn push(&mut self, commit: &Commit, known: Option<&HashSet<&str>>) -> GraphRow {
        let ci = self.next_index;
        self.next_index += 1;

        let parents: Vec<&str> = commit
            .parents
            .iter()
            .map(String::as_str)
            .filter(|parent| known.is_none_or(|known| known.contains(parent)))
            .collect();

        self.targets.clear();
        self.targets
            .extend(self.columns.iter().enumerate().filter_map(|(i, lane)| {
                lane.as_ref()
                    .is_some_and(|lane| lane.id == commit.id)
                    .then_some(i)
            }));
        self.targets.sort_unstable();

        // A fresh tip (no converging lane) starts a new lane in the leftmost
        // gap, or appends a new lane at the right.
        let node_col = match self.targets.first().copied() {
            Some(i) => i,
            None => {
                let color = alloc_color(&mut self.next_color);
                if let Some(i) = self.columns.iter().position(Option::is_none) {
                    self.columns[i] = Some(Lane {
                        id: commit.id.clone(),
                        color,
                    });
                    i
                } else {
                    self.columns.push(Some(Lane {
                        id: commit.id.clone(),
                        color,
                    }));
                    self.columns.len() - 1
                }
            }
        };
        let node_color = self.columns[node_col].as_ref().map_or(0, |lane| lane.color);

        // Decide where extra parents (parents[1..]) draw their lanes: reuse a
        // lane already carrying that parent, else a lane that converged into
        // this commit, else a gap to the right, else append a new lane.
        self.taken.clear();
        self.taken.resize(self.columns.len(), false);
        self.taken[node_col] = true;
        self.extra.clear();
        for (position, parent) in parents.iter().enumerate().skip(1) {
            let mut found = None;
            for (i, lane) in self.columns.iter().enumerate() {
                if !self.taken[i] && lane.as_ref().is_some_and(|lane| lane.id == *parent) {
                    found = Some(i);
                    break;
                }
            }
            if found.is_none() {
                found = self.targets.iter().copied().find(|&i| !self.taken[i]);
            }
            if found.is_none() {
                found = (node_col + 1..self.columns.len())
                    .find(|&i| self.columns[i].is_none() && !self.taken[i]);
            }
            let idx = match found {
                Some(index) => index,
                None => {
                    let color = alloc_color(&mut self.next_color);
                    self.columns.push(Some(Lane {
                        id: (*parent).to_string(),
                        color,
                    }));
                    self.taken.push(false);
                    self.columns.len() - 1
                }
            };
            if self.columns[idx].is_none() {
                let color = alloc_color(&mut self.next_color);
                self.columns[idx] = Some(Lane {
                    id: (*parent).to_string(),
                    color,
                });
            }
            self.extra.push((idx, position));
            self.taken[idx] = true;
        }

        // Render.
        let width = self.columns.len();
        let mut cells = vec![CellGlyph::Empty; width];
        let mut colors = vec![0; width];
        for i in 0..width {
            let lane = &self.columns[i];
            let is_target = self.targets.contains(&i);
            let is_extra = self.extra.iter().any(|(column, _)| *column == i);
            cells[i] = if i == node_col {
                if commit.is_merge() {
                    CellGlyph::MergeNode
                } else {
                    CellGlyph::Node
                }
            } else if lane.is_none() {
                CellGlyph::Empty // gap: a lane that already ended
            } else if is_extra && is_target {
                // A reused lane is already carrying this parent below the
                // merge. It still needs the fan-in corner at the merge row so
                // the horizontal edge visibly connects to that lane.
                CellGlyph::BranchDown
            } else if is_extra {
                CellGlyph::Empty // connector filled below
            } else if is_target {
                CellGlyph::MergeRight
            } else {
                CellGlyph::Vertical
            };
            colors[i] = lane.as_ref().map_or(0, |lane| lane.color);
        }
        // Connect the node to fresh parent lanes on its right. Horizontal
        // connectors and arcs both carry the color of the branch they fan out
        // to, matching the merged-in lane they lead into.
        if let Some(max_extra) = self.extra.iter().map(|(column, _)| *column).max() {
            if max_extra > node_col {
                for i in (node_col + 1)..=max_extra {
                    if self.targets.contains(&i) {
                        continue; // keep a reused lane as a pass-through
                    }
                    let is_extra = self.extra.iter().any(|(column, _)| *column == i);
                    cells[i] = if is_extra {
                        colors[i] = self.columns[i].as_ref().map_or(0, |lane| lane.color);
                        CellGlyph::BranchDown
                    } else {
                        // A horizontal segment belongs to the branch it leads
                        // to (the nearest extra-parent lane to its right), so
                        // the whole merge edge shares the merged branch's color.
                        let fan = (i + 1..=max_extra)
                            .find(|&j| self.extra.iter().any(|(column, _)| *column == j));
                        colors[i] = fan.map_or(node_color, |j| {
                            self.columns[j].as_ref().map_or(0, |lane| lane.color)
                        });
                        CellGlyph::Horizontal
                    };
                }
            }
        }
        cells[node_col] = if commit.is_merge() {
            CellGlyph::MergeNode
        } else {
            CellGlyph::Node
        };

        // Compact the column state for the next (parent) rows: the node lane
        // continues with the first parent, extra-parent lanes with their
        // parents, converging lanes that were not reused are freed (their
        // column becomes a gap), and pass-through lanes keep their column so
        // nothing ever shifts or changes color mid-run.
        let first_parent = parents.first().copied();
        for i in 0..self.columns.len() {
            let color = self.columns[i].as_ref().map(|lane| lane.color);
            if i == node_col {
                self.columns[i] = first_parent.and_then(|parent| {
                    color.map(|color| Lane {
                        id: parent.to_string(),
                        color,
                    })
                });
            } else if let Some((_, position)) = self.extra.iter().find(|(column, _)| *column == i) {
                self.columns[i] = color.map(|color| Lane {
                    id: parents[*position].to_string(),
                    color,
                });
            } else if self.targets.contains(&i) {
                self.columns[i] = None;
            }
        }
        while self.columns.last().is_some_and(Option::is_none) {
            self.columns.pop();
        }

        GraphRow {
            cells,
            colors,
            node_col: Some(node_col),
            commit_index: ci,
        }
    }
}

/// Compute the lane layout for the whole graph.
///
/// One row per commit, in `GraphModel` order (children before parents).
///
/// Lane assignment uses a stable incremental column model: each column tracks
/// the id of the commit it is currently connected to. Columns are never
/// reshuffled, so a branch keeps both its column and its color for its whole
/// lifetime — freeing a lane turns the column into an empty gap instead of
/// shifting its neighbours (which would make lines jump and change color
/// mid-run). Trailing gaps are trimmed so the graph stays tight on the right,
/// and new lanes (fresh tips, merge parents) reuse the leftmost gap to keep the
/// graph narrow. A merge's extra parents reuse the lanes that converged into
/// the merge; fresh tips append new lanes at the right when no gap is free.
pub fn layout(graph: &GraphModel) -> Vec<GraphRow> {
    let known: HashSet<&str> = graph.commits.iter().map(|c| c.id.as_str()).collect();
    Layout::new().extend(&graph.commits, Some(&known))
}

/// Allocate the next stable branch color id.
fn alloc_color(next: &mut usize) -> usize {
    let c = *next;
    *next += 1;
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(id: &str, parents: &[&str]) -> Commit {
        Commit {
            id: id.to_string(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            summary: id.to_string(),
            message: id.to_string(),
            author_name: "T".into(),
            author_email: "t@e".into(),
            timestamp: 0,
            committer_timestamp: 0,
            timezone_offset: 0,
            committer_timezone_offset: 0,
        }
    }

    fn graph(commits: Vec<Commit>) -> GraphModel {
        GraphModel {
            commits,
            refs: vec![],
            branch_tracking: Vec::new(),
            head_id: None,
            head_name: None,
        }
    }

    fn rows_text(rows: &[GraphRow]) -> Vec<String> {
        rows.iter()
            .map(|r| r.cells.iter().map(|c| c.char()).collect::<String>())
            .collect()
    }

    #[test]
    fn chunked_layout_matches_whole_graph_layout() {
        // Paging must not change any row: laying the history out in chunks has
        // to produce exactly the same cells, colors, and node columns.
        let commits = vec![
            commit("m12", &["m10", "t12"]),
            commit("m10", &["m9", "t10"]),
            commit("t12", &["t11"]),
            commit("t11", &["t10"]),
            commit("t10", &["base"]),
            commit("m9", &["base"]),
            commit("base", &[]),
        ];
        let whole = layout(&graph(commits.clone()));

        for chunk_size in [1usize, 2, 3, 6] {
            let mut engine = Layout::new();
            let mut paged = Vec::new();
            for chunk in commits.chunks(chunk_size) {
                paged.extend(engine.extend(chunk, None));
            }
            assert_eq!(paged.len(), whole.len(), "chunk size {chunk_size}");
            for (index, (a, b)) in paged.iter().zip(&whole).enumerate() {
                assert_eq!(
                    a.cells, b.cells,
                    "cells differ at row {index} (chunk {chunk_size})"
                );
                assert_eq!(
                    a.colors, b.colors,
                    "colors differ at row {index} (chunk {chunk_size})"
                );
                assert_eq!(
                    a.node_col, b.node_col,
                    "node differs at row {index} (chunk {chunk_size})"
                );
                assert_eq!(a.commit_index, index);
            }
        }
    }

    #[test]
    fn linear_is_single_column() {
        // topo order: c -> b -> a (children first)
        let g = graph(vec![
            commit("c", &["b"]),
            commit("b", &["a"]),
            commit("a", &[]),
        ]);
        let rows = layout(&g);
        assert_eq!(rows_text(&rows), vec!["●", "●", "●"]);
        for row in &rows {
            assert_eq!(row.node_col, Some(0));
        }
    }

    #[test]
    fn two_branches_merge() {
        // topo order: merge -> left -> right -> base
        let g = graph(vec![
            commit("merge", &["left", "right"]),
            commit("left", &["base"]),
            commit("right", &["base"]),
            commit("base", &[]),
        ]);
        let rows = layout(&g);
        let text = rows_text(&rows);
        assert_eq!(text, vec!["▲╮", "●│", "│●", "●╯"]);
    }

    #[test]
    fn horizontal_connector_uses_merged_branch_color() {
        // Nested merges produce a `─` connector crossing another branch's lane:
        //   m12 = merge(m10, t12)   m10 = merge(m9, t10)
        //   t12 -> t11 -> t10       m9 -> base, t10 -> base
        // topo order (children first): m12, m10, t12, t11, t10, m9, base
        let g = graph(vec![
            commit("m12", &["m10", "t12"]),
            commit("m10", &["m9", "t10"]),
            commit("t12", &["t11"]),
            commit("t11", &["t10"]),
            commit("t10", &["base"]),
            commit("m9", &["base"]),
            commit("base", &[]),
        ]);
        let rows = layout(&g);
        // m10's row: the connector reaches the merged-in parent lane.
        let text = rows_text(&rows);
        assert_eq!(text[1], "▲─╮");
        // The connector and `╮` arc carry the merged-in branch's color (t10),
        // and differ from the node's own branch (m10).
        assert_ne!(
            rows[1].colors[1], rows[1].colors[0],
            "fan-in must differ from the node's branch"
        );
    }

    #[test]
    fn octopus_merge_spans_three_parents() {
        // topo order: merge -> p1 -> p2 -> p3 -> base
        let g = graph(vec![
            commit("merge", &["p1", "p2", "p3"]),
            commit("p1", &["base"]),
            commit("p2", &["base"]),
            commit("p3", &["base"]),
            commit("base", &[]),
        ]);
        let rows = layout(&g);
        let text = rows_text(&rows);
        assert_eq!(text, vec!["▲╮╮", "●││", "│●│", "││●", "●╯╯"]);
    }

    #[test]
    fn freed_lanes_are_reused_keeping_graph_narrow() {
        // A merge with three converging children (c1, c2, c3) but only two
        // parents (p1, p2): c3's lane is freed at the merge, then an older
        // branch f reuses that freed column instead of widening the graph.
        // topo order (children first): c1, c2, c3, m, p1, p2, f, base
        let g = graph(vec![
            commit("c1", &["m"]),
            commit("c2", &["m"]),
            commit("c3", &["m"]),
            commit("m", &["p1", "p2"]),
            commit("p1", &["base"]),
            commit("p2", &["base"]),
            commit("f", &["base"]),
            commit("base", &[]),
        ]);
        let rows = layout(&g);
        let text = rows_text(&rows);
        assert_eq!(text[3], "▲╮╯"); // merge: node, p2 fan-in, freed lane
                                    // `f` reuses the freed rightmost column rather than appending a 4th.
        assert_eq!(text[6], "││●");
        let lanes = max_lanes(&rows);
        assert!(
            lanes <= 3,
            "graph must stay at most 3 lanes wide, got {lanes}"
        );
    }

    #[test]
    fn freed_lanes_become_stable_gaps() {
        // A lane that ends mid-graph leaves a stable gap: the columns to its
        // right never shift, so no vertical line ever jumps or changes color.
        // Trunk T0..T4. M = merge(T3, A1) puts A's lane at col1; branch B
        // (forks T0) runs at col2. After A ends, col1 is a gap and B stays put.
        let g = graph(vec![
            commit("m", &["t3", "a1"]),
            commit("a1", &["a0"]),
            commit("a0", &["t2"]),
            commit("b1", &["b0"]),
            commit("b0", &["t0"]),
            commit("t4", &["t3"]),
            commit("t3", &["t2"]),
            commit("t2", &["t1"]),
            commit("t1", &["t0"]),
            commit("t0", &[]),
        ]);
        let rows = layout(&g);
        let text = rows_text(&rows);
        assert_eq!(text[0], "▲╮"); // merge: node + A's fresh lane
        assert_eq!(text[2], "│●"); // a0 still at col1
        assert_eq!(text[6], "●││╯"); // t3: t4's line arcs in (rightmost, trimmed)
                                     // After A's lane ends (t2 row), col1 is a gap while B stays at col2.
        assert_eq!(text[8], "● │"); // internal gap at col1, B untouched at col2
        assert_eq!(text[9], "● ╯"); // B arcs into t0, gap remains at col1
        for (ri, row) in rows.iter().enumerate() {
            // B (the rightmost active lane, col2) must never appear at a
            // different column: no shifting.
            if matches!(
                row.cells.get(2),
                Some(CellGlyph::Vertical) | Some(CellGlyph::Node)
            ) {
                assert_eq!(
                    row.colors[2], rows[3].colors[2],
                    "row {ri}: B's lane changed color (shifted)"
                );
            }
        }
    }

    #[test]
    fn no_column_changes_color_without_a_break() {
        // A vertical line must never change branch color between consecutive
        // rows: a color switch implies a lane shift, which the stable-column
        // model forbids. Build a merge-heavy topology and scan every column.
        let mut cs: Vec<Commit> = Vec::new();
        let mut add = |id: &str, parents: &[&str]| cs.push(commit(id, parents));
        add("m", &["t12", "a2"]);
        add("a2", &["a1"]);
        add("a1", &["a0"]);
        add("a0", &["t11"]);
        for (branch, fork) in [("b", "t2"), ("c", "t1"), ("d", "t0"), ("e", "t3")] {
            for i in (0..5).rev() {
                let id = format!("{branch}{i}");
                let parent = if i == 4 {
                    branch.to_string()
                } else {
                    format!("{branch}{}", i + 1)
                };
                // last one forks from the trunk
                let parent = if i == 0 { fork.to_string() } else { parent };
                add(&id, &[&parent]);
            }
        }
        for i in 0..=14 {
            let id = format!("t{i}");
            let parent = if i == 0 {
                vec![]
            } else {
                vec![format!("t{}", i - 1)]
            };
            let ps: Vec<&str> = parent.iter().map(|s| s.as_str()).collect();
            add(&id, &ps);
        }
        let g = graph(cs);
        let rows = layout(&g);
        for ri in 1..rows.len() {
            let (prev, cur) = (&rows[ri - 1], &rows[ri]);
            for ci in 0..cur.cells.len().min(prev.cells.len()) {
                if prev.cells[ci] == CellGlyph::Vertical
                    && cur.cells[ci] == CellGlyph::Vertical
                    && prev.colors[ci] != cur.colors[ci]
                {
                    panic!(
                        "row {ri} col {ci}: vertical line changes color {} -> {} with no break",
                        prev.colors[ci], cur.colors[ci]
                    );
                }
            }
        }
    }

    #[test]
    fn all_columns_carry_glyph_or_empty() {
        let g = graph(vec![
            commit("m", &["a", "b"]),
            commit("a", &["base"]),
            commit("b", &["base"]),
            commit("base", &[]),
        ]);
        for row in layout(&g) {
            for cell in &row.cells {
                assert!(cell.char() != '\0');
            }
        }
    }
}

//! Debug tool: verify that lane (column) assignment for fan-out branch tips
//! matches temporal (commit time) order.
//!
//! Usage: `cargo run --example timeprobe`

use figit::core::model::{Commit, GraphModel};
use figit::core::topo::topo_sort;
use figit::graph::layout::layout;

fn commit(id: &str, parents: &[&str], timestamp: i64) -> Commit {
    Commit {
        id: id.to_string(),
        parents: parents.iter().map(|p| p.to_string()).collect(),
        summary: id.to_string(),
        message: id.to_string(),
        author_name: "T".into(),
        author_email: "t@e".into(),
        timestamp,
        committer_timestamp: timestamp,
        timezone_offset: 0,
        committer_timezone_offset: 0,
    }
}

fn main() {
    // Mainline m0..m15 (15 commits, ts = 100 + i). Several remote branches fork
    // off the mainline at various commits and are left as tips (like origin/*).
    // Their tip timestamps are the "branch freshness".
    let mut cs: Vec<Commit> = Vec::new();
    let mut add = |c: Commit| cs.push(c);

    // remote branch tips: (id, parent-mainline-id, tip-time). fork at mainline i
    let remotes: Vec<(&str, &str, i64)> = vec![
        ("r1", "m14", 116), // forks near top, most recent tip
        ("r2", "m12", 114),
        ("r3", "m10", 112),
        ("r4", "m8", 110),
        ("r5", "m6", 108),
        ("r6", "m4", 106),
        ("r7", "m2", 104),
        ("r8", "m14", 130), // old fork point (m14) but NEWEST tip overall
    ];
    for (id, parent, ts) in &remotes {
        // two-commit branch: tip `id` -> `id-b` -> fork point on mainline
        let b = format!("{id}-b");
        add(commit(&b, &[*parent], ts - 1));
        add(commit(id, &[&b], *ts));
    }

    // mainline, children-first order later (we emit parents first then topo sort)
    for i in 0..=15 {
        let id = format!("m{i}");
        let parent = if i == 0 {
            vec![]
        } else {
            vec![format!("m{}", i - 1)]
        };
        let ps: Vec<&str> = parent.iter().map(|s| s.as_str()).collect();
        add(commit(&id, &ps, 100 + i as i64));
    }

    let sorted = topo_sort(cs);
    let idx: std::collections::HashMap<String, usize> = sorted
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.clone(), i))
        .collect();
    println!("Emission order (top first) with timestamps:");
    for (i, c) in sorted.iter().enumerate() {
        println!("  {i:>2} {:<4} ts={}", c.id, c.timestamp);
    }
    let g = GraphModel {
        commits: sorted,
        refs: vec![],
        branch_tracking: vec![],
        head_id: Some("m15".to_string()),
        head_name: Some("main".to_string()),
    };
    let rows = layout(&g);

    // For each remote tip, find its lane column
    println!("\nRemote tip lanes (left-to-right = column order):");
    let mut tip_cols: Vec<(String, i64, usize)> = Vec::new();
    for r in &remotes {
        let ci = idx[r.0];
        let row = &rows[ci];
        let col = row.node_col.unwrap();
        let ts = g.commits[ci].timestamp;
        tip_cols.push((r.0.to_string(), ts, col));
    }
    tip_cols.sort_by_key(|t| t.2);
    for (id, ts, col) in &tip_cols {
        println!("  col {col}: {id} ts={ts}");
    }
    let time_asc = tip_cols.windows(2).all(|w| w[0].1 <= w[1].1);
    let time_desc = tip_cols.windows(2).all(|w| w[0].1 >= w[1].1);
    println!("\nColumn order is time-ASCENDING: {time_asc}");
    println!("Column order is time-DESCENDING: {time_desc}");
}

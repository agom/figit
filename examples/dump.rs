//! Headless debug tool: print the rendered graph rows for a repo, one per line.
//!
//! Usage: `cargo run --example dump -- [path] [rows]`

use std::path::PathBuf;

const REMOTE_GLYPH: &str = "\u{f09b}";
const TAG_GLYPH: &str = "\u{f04f9}";
const PILL_LEFT: char = '\u{e0b6}'; // 
const PILL_RIGHT: char = '\u{e0b4}'; // 

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path: PathBuf = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let limit: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);

    let backend = figit::backend::open(&path)?;
    let graph = backend.build_graph()?;
    println!(
        "{} commits, {} refs, head={:?}",
        graph.len(),
        graph.refs.len(),
        graph.head_name
    );

    let rows = figit::graph::layout::layout(&graph);
    for row in rows.iter().take(limit) {
        let commit = &graph.commits[row.commit_index];
        let lane: String = row.cells.iter().map(|c| c.char()).collect();
        let labels: Vec<String> = graph
            .display_refs(&commit.id)
            .iter()
            .map(|r| {
                let text = match (r.kind, r.has_remote) {
                    (figit::core::model::RefKind::Tag, _) => format!("{TAG_GLYPH} {}", r.name),
                    (_, true) => format!("{REMOTE_GLYPH} {}", r.name),
                    (_, false) => r.name.clone(),
                };
                format!("{PILL_LEFT} {text} {PILL_RIGHT}")
            })
            .collect();
        let prefix = if labels.is_empty() {
            String::new()
        } else {
            format!("[{}] ", labels.join(", "))
        };
        let short: String = commit.id.chars().take(9).collect();
        println!("{lane:<4}{prefix}{}  {short}", commit.summary);
    }
    Ok(())
}

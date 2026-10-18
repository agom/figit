use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use figit::core::model::{GraphModel, RefKind};

fn run_fixture(name: &str, args: &[&str]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(format!("{name}.sh"));
    let status = Command::new("bash")
        .arg(&script)
        .arg(dir.path())
        .args(args)
        .status()
        .expect("run fixture script");
    assert!(status.success(), "fixture {name} failed");
    dir
}

fn build(path: &Path) -> GraphModel {
    let backend = figit::backend::open(path).expect("open backend");
    backend.build_graph().expect("build graph")
}

/// 10 branches × 20 commits. Shared across the layout sanity tests because
/// building the fixture takes a few seconds.
fn multi_branch_fixture() -> &'static Path {
    static DIR: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    DIR.get_or_init(|| run_fixture("multi-branch", &["10", "20"]))
        .path()
}

/// Topological order invariant: parents always appear after their children.
fn assert_topo_order(graph: &GraphModel) {
    let index = graph.commit_index();
    for commit in &graph.commits {
        let pos = index[commit.id.as_str()];
        for parent in &commit.parents {
            if let Some(&p) = index.get(parent.as_str()) {
                assert!(
                    p > pos,
                    "parent {parent} must come after child {}",
                    commit.id
                );
            }
        }
    }
}

fn assert_unique_ids(graph: &GraphModel) {
    let ids: HashSet<&str> = graph.commits.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids.len(), graph.commits.len(), "duplicate commit ids");
}

fn assert_refs_point_to_known_commits(graph: &GraphModel) {
    let index = graph.commit_index();
    for r in &graph.refs {
        assert!(
            index.contains_key(r.id.as_str()),
            "ref {} points at unknown commit {}",
            r.name,
            r.id
        );
    }
    if let Some(head) = &graph.head_id {
        assert!(
            index.contains_key(head.as_str()),
            "HEAD points at unknown commit"
        );
    }
}

#[test]
fn unborn_repository_loads_as_an_empty_complete_history() {
    let dir = TempDir::new().expect("create empty repository");
    let initialized = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main"])
        .current_dir(dir.path())
        .status()
        .expect("initialize empty repository");
    assert!(initialized.success());

    let backend = figit::backend::open(dir.path()).expect("open empty repository");
    let page = backend.graph_window(0, 20).expect("load empty history");
    assert!(page.commits.is_empty());
    assert!(page.complete);
    assert_eq!(page.head_id, None);
    assert_eq!(page.head_name.as_deref(), Some("main"));
}

#[test]
fn linear_repo() {
    let dir = run_fixture("linear", &["5"]);
    let graph = build(dir.path());
    assert_eq!(graph.commits.len(), 5);
    assert_eq!(graph.commits[0].summary, "commit 5");
    assert_eq!(graph.commits[4].summary, "commit 1");
    // first commit is the root
    assert!(graph.commits[4].parents.is_empty());
    // all others have exactly one parent
    for c in &graph.commits[..4] {
        assert_eq!(c.parents.len(), 1);
    }
    assert_topo_order(&graph);
    assert_unique_ids(&graph);
    // the main branch ref points at the newest commit
    let main_ref = graph
        .refs
        .iter()
        .find(|r| r.name == "main" && r.kind == RefKind::LocalBranch)
        .expect("main ref");
    assert_eq!(main_ref.id, graph.commits[0].id);
    assert_refs_point_to_known_commits(&graph);
}

#[test]
fn branch_merge_has_single_merge_commit() {
    let dir = run_fixture("branch-merge", &[]);
    let graph = build(dir.path());
    let merges: Vec<_> = graph.commits.iter().filter(|c| c.is_merge()).collect();
    assert_eq!(merges.len(), 1, "expected exactly one merge commit");
    assert_eq!(merges[0].summary, "merge feature");
    assert_eq!(merges[0].parents.len(), 2);
    assert_topo_order(&graph);
    assert_unique_ids(&graph);

    let names: HashSet<&str> = graph
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::LocalBranch)
        .map(|r| r.name.as_str())
        .collect();
    assert!(names.contains("main"));
    assert!(names.contains("feature"));
    assert_refs_point_to_known_commits(&graph);
}

#[test]
fn octopus_merge_has_three_parents() {
    let dir = run_fixture("octopus", &[]);
    let graph = build(dir.path());
    let octopus = graph
        .commits
        .iter()
        .find(|c| c.parents.len() >= 3)
        .expect("octopus commit with 3+ parents");
    assert_eq!(octopus.summary, "octopus merge a b c");
    assert!(octopus.parents.len() >= 3);
    assert_topo_order(&graph);
    assert_unique_ids(&graph);
    assert_refs_point_to_known_commits(&graph);
}

#[test]
fn multi_branch_repo_is_large_and_sane() {
    let dir = multi_branch_fixture();
    let graph = build(dir);
    assert!(
        graph.commits.len() >= 200,
        "expected >= 200 commits, got {}",
        graph.commits.len()
    );
    assert!(
        graph
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::LocalBranch)
            .count()
            >= 10,
        "expected >= 10 local branches"
    );
    assert_topo_order(&graph);
    assert_unique_ids(&graph);
    assert_refs_point_to_known_commits(&graph);
}

#[test]
fn many_branches_do_not_widen_the_graph() {
    // The fixture nests 10 branches one on top of another, which genuinely
    // keeps several lines in flight at once. Lane reuse must keep the graph
    // bounded well below one lane per branch (10).
    let dir = multi_branch_fixture();
    let graph = build(dir);
    let rows = figit::graph::layout::layout(&graph);
    let lanes = figit::graph::layout::max_lanes(&rows);
    assert!(
        lanes <= 8,
        "graph widened to {lanes} lanes for {} commits / 10 branches",
        graph.commits.len()
    );
}

#[test]
fn tags_and_detached_head() {
    let dir = run_fixture("tags", &[]);
    let graph = build(dir.path());
    assert_eq!(graph.commits.len(), 3);

    let tag_names: Vec<&str> = graph
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::Tag)
        .map(|r| r.name.as_str())
        .collect();
    assert!(tag_names.contains(&"lightweight-tag"));
    assert!(tag_names.contains(&"annotated-tag"));

    // both tags point at the same (newest) commit
    let newest = &graph.commits[0];
    for r in graph.refs.iter().filter(|r| r.kind == RefKind::Tag) {
        assert_eq!(
            r.id, newest.id,
            "tag {} should point at newest commit",
            r.name
        );
    }

    // HEAD is detached: no branch name, but head_id present
    assert!(graph.head_id.is_some());
    assert_eq!(graph.head_name, None);
    assert_refs_point_to_known_commits(&graph);
}

#[test]
fn head_branch_name_is_reported() {
    let dir = run_fixture("linear", &["3"]);
    let graph = build(dir.path());
    assert_eq!(graph.head_name.as_deref(), Some("main"));
    assert_eq!(graph.head_id, Some(graph.commits[0].id.clone()));
}

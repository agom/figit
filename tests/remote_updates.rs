#[allow(dead_code)]
mod common;

use common::{configure_peer, configure_user, git_ok};
use figit::backend::{self, PullStrategy};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    local: PathBuf,
    peer: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("remote.git");
        let local = temp.path().join("local");
        let peer = temp.path().join("peer");
        git_ok(
            temp.path(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        );
        git_ok(
            temp.path(),
            &["clone", remote.to_str().unwrap(), local.to_str().unwrap()],
        );
        configure_user(&local);
        commit(&local, "base.txt", "base");
        git_ok(&local, &["push", "-u", "origin", "main"]);
        git_ok(
            temp.path(),
            &["clone", remote.to_str().unwrap(), peer.to_str().unwrap()],
        );
        configure_peer(&peer);
        Self {
            _temp: temp,
            local,
            peer,
        }
    }
}

fn commit(path: &Path, file: &str, text: &str) -> String {
    std::fs::write(path.join(file), text).unwrap();
    git_ok(path, &["add", file]);
    git_ok(path, &["commit", "-qm", text]);
    git_ok(path, &["rev-parse", "HEAD"])
}

#[test]
fn pull_strategies_reconcile_divergence_and_override_ff_only_without_changing_config() {
    for strategy in [
        PullStrategy::Configured,
        PullStrategy::FastForward,
        PullStrategy::Rebase,
        PullStrategy::Merge,
    ] {
        let f = Fixture::new();
        let local_tip = commit(&f.local, "local.txt", "local");
        let remote_tip = commit(&f.peer, "remote.txt", "remote");
        git_ok(&f.peer, &["push"]);
        git_ok(&f.local, &["config", "pull.ff", "only"]);
        let backend = backend::open(&f.local).unwrap();
        let result = backend.pull("main", strategy);
        if matches!(
            strategy,
            PullStrategy::Configured | PullStrategy::FastForward
        ) {
            assert!(result.is_err());
            assert_eq!(git_ok(&f.local, &["rev-parse", "HEAD"]), local_tip);
        } else {
            result.unwrap();
            assert!(f.local.join("local.txt").exists());
            assert!(f.local.join("remote.txt").exists());
            git_ok(
                &f.local,
                &["merge-base", "--is-ancestor", &remote_tip, "HEAD"],
            );
            let parents = git_ok(&f.local, &["show", "-s", "--format=%P", "HEAD"]);
            assert_eq!(
                parents.split_whitespace().count(),
                if strategy == PullStrategy::Merge {
                    2
                } else {
                    1
                }
            );
        }
        assert_eq!(git_ok(&f.local, &["config", "pull.ff"]), "only");
    }
}

#[test]
fn pull_rejects_a_changed_checkout_and_preserves_conflicts_for_resolution() {
    let f = Fixture::new();
    let backend = backend::open(&f.local).unwrap();
    git_ok(&f.local, &["checkout", "-qb", "other"]);
    assert!(backend
        .pull("main", PullStrategy::Merge)
        .unwrap_err()
        .to_string()
        .contains("checked-out branch changed"));
    git_ok(&f.local, &["checkout", "main"]);
    commit(&f.local, "base.txt", "ours");
    commit(&f.peer, "base.txt", "theirs");
    git_ok(&f.peer, &["push"]);
    assert!(backend.pull("main", PullStrategy::Rebase).is_err());
    assert!(backend.rebase_in_progress().unwrap());
    assert!(backend
        .status()
        .unwrap()
        .iter()
        .any(|entry| entry.is_conflict()));
    backend.abort_rebase().unwrap();
}

#[test]
fn inactive_branch_updates_without_checkout_and_respects_renamed_upstream() {
    let f = Fixture::new();
    git_ok(&f.local, &["branch", "-m", "main", "tracking"]);
    git_ok(&f.local, &["checkout", "-qb", "work"]);
    std::fs::write(f.local.join("base.txt"), "unstaged work").unwrap();
    std::fs::write(f.local.join("staged.txt"), "staged work").unwrap();
    git_ok(&f.local, &["add", "staged.txt"]);
    let before = git_ok(&f.local, &["status", "--porcelain"]);
    let head = git_ok(&f.local, &["rev-parse", "HEAD"]);
    let backend = backend::open(&f.local).unwrap();
    let plan = backend.prepare_branch_update("tracking").unwrap();
    let tip = commit(&f.peer, "remote.txt", "remote change after confirmation");
    git_ok(&f.peer, &["push"]);
    backend.update_branch(&plan, false).unwrap();
    assert_eq!(git_ok(&f.local, &["rev-parse", "tracking"]), tip);
    assert_eq!(git_ok(&f.local, &["branch", "--show-current"]), "work");
    assert_eq!(git_ok(&f.local, &["rev-parse", "HEAD"]), head);
    assert_eq!(git_ok(&f.local, &["status", "--porcelain"]), before);
    assert_eq!(
        std::fs::read_to_string(f.local.join("base.txt")).unwrap(),
        "unstaged work"
    );
}

#[test]
fn reset_inactive_branch_discards_only_its_commits_and_fast_forward_refuses() {
    let f = Fixture::new();
    let local_tip = commit(&f.local, "local.txt", "local");
    git_ok(&f.local, &["checkout", "-qb", "work"]);
    let remote_tip = commit(&f.peer, "remote.txt", "remote");
    git_ok(&f.peer, &["push"]);
    let backend = backend::open(&f.local).unwrap();
    let plan = backend.prepare_branch_update("main").unwrap();
    assert!(backend
        .update_branch(&plan, false)
        .unwrap_err()
        .to_string()
        .contains("cannot fast-forward"));
    assert_eq!(git_ok(&f.local, &["rev-parse", "main"]), local_tip);
    backend.update_branch(&plan, true).unwrap();
    assert_eq!(git_ok(&f.local, &["rev-parse", "main"]), remote_tip);
    assert_eq!(git_ok(&f.local, &["rev-parse", "HEAD"]), local_tip);
    assert!(f.local.join("local.txt").exists());
    assert!(!f.local.join("remote.txt").exists());
}

#[test]
fn branch_update_refuses_stale_confirmation_and_another_worktree() {
    let f = Fixture::new();
    let backend = backend::open(&f.local).unwrap();
    let plan = backend.prepare_branch_update("main").unwrap();
    let tip = commit(&f.local, "local.txt", "new local commit");
    assert!(backend.update_branch(&plan, true).is_err());
    assert_eq!(git_ok(&f.local, &["rev-parse", "HEAD"]), tip);
    git_ok(&f.local, &["checkout", "-qb", "work"]);
    let plan = backend.prepare_branch_update("main").unwrap();
    let linked = f._temp.path().join("linked");
    git_ok(
        &f.local,
        &["worktree", "add", linked.to_str().unwrap(), "main"],
    );
    assert!(backend
        .prepare_branch_update("main")
        .unwrap_err()
        .to_string()
        .contains("another worktree"));
    assert!(backend.update_branch(&plan, true).is_err());
    assert_eq!(git_ok(&linked, &["rev-parse", "HEAD"]), tip);
}

#[test]
fn current_branch_fast_forward_preserves_edits_and_reset_updates_files() {
    let f = Fixture::new();
    let tip = commit(&f.peer, "remote.txt", "remote");
    git_ok(&f.peer, &["push"]);
    std::fs::write(f.local.join("base.txt"), "dirty").unwrap();
    let backend = backend::open(&f.local).unwrap();
    let plan = backend.prepare_branch_update("main").unwrap();
    backend.update_branch(&plan, false).unwrap();
    assert_eq!(git_ok(&f.local, &["rev-parse", "HEAD"]), tip);
    assert_eq!(
        std::fs::read_to_string(f.local.join("base.txt")).unwrap(),
        "dirty"
    );
    let plan = backend.prepare_branch_update("main").unwrap();
    backend.update_branch(&plan, true).unwrap();
    assert!(git_ok(&f.local, &["status", "--porcelain"]).is_empty());
}

#[test]
fn updates_use_custom_fetch_mappings_and_refuse_missing_remote_branches() {
    let f = Fixture::new();
    git_ok(
        &f.local,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/company/*",
        ],
    );
    git_ok(&f.local, &["fetch", "origin"]);
    git_ok(&f.local, &["checkout", "-qb", "work"]);
    let backend = backend::open(&f.local).unwrap();
    let plan = backend.prepare_branch_update("main").unwrap();
    assert_eq!(plan.upstream, "company/main");
    let tip = commit(&f.peer, "remote.txt", "custom mapping");
    git_ok(&f.peer, &["push"]);
    backend.update_branch(&plan, false).unwrap();
    assert_eq!(git_ok(&f.local, &["rev-parse", "main"]), tip);
    let plan = backend.prepare_branch_update("main").unwrap();
    git_ok(&f.peer, &["push", "origin", "HEAD:other"]);
    // The bare repository ordinarily protects deletion of its HEAD branch.
    git_ok(
        &f.peer,
        &[
            "--git-dir",
            f._temp.path().join("remote.git").to_str().unwrap(),
            "symbolic-ref",
            "HEAD",
            "refs/heads/other",
        ],
    );
    git_ok(&f.peer, &["push", "origin", ":main"]);
    assert!(backend.update_branch(&plan, true).is_err());
    assert_eq!(git_ok(&f.local, &["rev-parse", "main"]), tip);
}

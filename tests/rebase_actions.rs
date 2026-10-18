mod common;

use std::path::Path;
use std::process::Command;

use common::{configure_peer, configure_user, git, git_ok};
use figit::backend::{
    self, ConflictSide, DiffLineSelection, DiffTarget, RebaseAction, RebaseOutcome,
};
use figit::core::model::GitPath;
use tempfile::TempDir;

fn repository() -> (TempDir, String, String) {
    let temp = tempfile::tempdir().expect("create repository directory");
    let path = temp.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);

    std::fs::write(path.join("file.txt"), "root\n").unwrap();
    git_ok(path, &["add", "file.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "root commit"]);

    std::fs::write(path.join("file.txt"), "target\n").unwrap();
    git_ok(path, &["commit", "--quiet", "-am", "target commit"]);
    let target = git_ok(path, &["rev-parse", "HEAD"]);

    std::fs::write(path.join("file.txt"), "tip\n").unwrap();
    git_ok(path, &["commit", "--quiet", "-am", "tip commit"]);
    let tip = git_ok(path, &["rev-parse", "HEAD"]);

    (temp, target, tip)
}

fn merge_repository() -> (TempDir, String) {
    let temp = tempfile::tempdir().expect("create merge repository directory");
    let path = temp.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);

    std::fs::write(path.join("root.txt"), "root\n").unwrap();
    git_ok(path, &["add", "root.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "root commit"]);

    git_ok(path, &["checkout", "--quiet", "-b", "side"]);
    std::fs::write(path.join("side.txt"), "side\n").unwrap();
    git_ok(path, &["add", "side.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "side commit"]);

    git_ok(path, &["checkout", "--quiet", "main"]);
    std::fs::write(path.join("main.txt"), "main\n").unwrap();
    git_ok(path, &["add", "main.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "main commit"]);
    git_ok(
        path,
        &["merge", "--quiet", "--no-ff", "-m", "merge target", "side"],
    );
    let target = git_ok(path, &["rev-parse", "HEAD"]);

    std::fs::write(path.join("tip.txt"), "tip\n").unwrap();
    git_ok(path, &["add", "tip.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "tip commit"]);

    (temp, target)
}

fn tui_fixture_repository() -> TempDir {
    let temp = tempfile::tempdir().expect("create TUI fixture clone");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/tui-e2e.sh");
    let status = Command::new("bash")
        .arg(fixture)
        .arg(temp.path())
        .status()
        .expect("run TUI fixture builder");
    assert!(status.success(), "TUI fixture builder failed");
    git_ok(temp.path(), &["reset", "--hard", "HEAD"]);
    git_ok(temp.path(), &["clean", "-f", "--", "z-discard.txt"]);
    temp
}

fn fixture_commit(path: &Path, subject: &str) -> String {
    git_ok(
        path,
        &[
            "log",
            "--all",
            "--format=%H",
            "--fixed-strings",
            "--grep",
            subject,
        ],
    )
}

fn conflicted_repository() -> TempDir {
    let temp = tempfile::tempdir().expect("create conflict repository");
    let path = temp.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    std::fs::write(path.join("file.txt"), "base\n").unwrap();
    git_ok(path, &["add", "file.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "base"]);

    git_ok(path, &["checkout", "--quiet", "-b", "feature"]);
    std::fs::write(path.join("file.txt"), "theirs\n").unwrap();
    git_ok(path, &["commit", "--quiet", "-am", "theirs"]);

    git_ok(path, &["checkout", "--quiet", "main"]);
    std::fs::write(path.join("file.txt"), "ours\n").unwrap();
    git_ok(path, &["commit", "--quiet", "-am", "ours"]);
    let merge = git(path, &["merge", "--no-ff", "-m", "conflict", "feature"]);
    assert!(!merge.status.success(), "fixture merge should conflict");
    temp
}

#[test]
fn reword_replays_target_and_descendants_with_new_message() {
    let (temp, target, old_tip) = repository();
    let backend = backend::open(temp.path()).expect("open repository");

    let outcome = backend
        .rebase_commit(
            &target,
            &RebaseAction::Reword {
                title: "new target title".into(),
                description: "new target body".into(),
            },
        )
        .expect("reword commit");

    assert_eq!(outcome, RebaseOutcome::Completed);
    let new_tip = git_ok(temp.path(), &["rev-parse", "HEAD"]);
    assert_ne!(new_tip, old_tip);
    assert_eq!(
        git_ok(temp.path(), &["show", "-s", "--format=%B", &target]),
        "target commit"
    );
    let rewritten_message = git_ok(temp.path(), &["show", "-s", "--format=%B", "HEAD~1"]);
    assert_eq!(rewritten_message, "new target title\n\nnew target body");
    assert_eq!(
        git_ok(temp.path(), &["log", "--format=%s", "-3"]),
        "tip commit\nnew target title\nroot commit"
    );
}

#[test]
fn interactive_plan_rewords_and_preserves_the_rest_of_the_todo() {
    let (temp, target, _) = repository();
    let backend = backend::open(temp.path()).expect("open repository");
    let mut plan = backend
        .prepare_rebase_plan(&target)
        .expect("build todo plan");

    let target_entry = plan
        .blocks
        .iter_mut()
        .find_map(|block| match block {
            figit::backend::RebasePlanBlock::Commits(entries) => {
                entries.iter_mut().find(|entry| entry.id == target)
            }
            _ => None,
        })
        .expect("selected commit in todo");
    target_entry.action = figit::backend::RebaseTodoAction::Reword;
    target_entry.title = "planned title".into();
    target_entry.description = "planned body".into();

    let outcome = backend
        .execute_rebase_plan(&plan)
        .expect("execute todo plan");

    assert_eq!(outcome, RebaseOutcome::Completed);
    assert_eq!(
        git_ok(temp.path(), &["log", "--format=%s", "-3"]),
        "tip commit\nplanned title\nroot commit"
    );
    assert_eq!(
        git_ok(temp.path(), &["show", "-s", "--format=%B", "HEAD~1"]),
        "planned title\n\nplanned body"
    );
}

#[test]
fn interactive_plan_reorders_commits_inside_a_linear_sequence() {
    let temp = tempfile::tempdir().expect("create repository");
    let path = temp.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    std::fs::write(path.join("root.txt"), "root\n").unwrap();
    git_ok(path, &["add", "root.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "root commit"]);
    std::fs::write(path.join("one.txt"), "one\n").unwrap();
    git_ok(path, &["add", "one.txt"]);
    git_ok(
        path,
        &["commit", "--quiet", "-m", "first independent commit"],
    );
    let first = git_ok(path, &["rev-parse", "HEAD"]);
    std::fs::write(path.join("two.txt"), "two\n").unwrap();
    git_ok(path, &["add", "two.txt"]);
    git_ok(
        path,
        &["commit", "--quiet", "-m", "second independent commit"],
    );
    let backend = backend::open(path).expect("open repository");
    let mut plan = backend
        .prepare_rebase_plan(&first)
        .expect("build todo plan");
    let block = plan
        .blocks
        .iter_mut()
        .find_map(|block| match block {
            figit::backend::RebasePlanBlock::Commits(entries) if entries.len() == 2 => {
                Some(entries)
            }
            _ => None,
        })
        .expect("two commits in one reorderable sequence");
    block.swap(0, 1);

    backend
        .execute_rebase_plan(&plan)
        .expect("execute reordered plan");

    assert_eq!(
        git_ok(path, &["log", "--format=%s", "-3"]),
        "first independent commit\nsecond independent commit\nroot commit"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("one.txt")).unwrap(),
        "one\n"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("two.txt")).unwrap(),
        "two\n"
    );
}

#[test]
fn interactive_plan_supports_squash_fixup_and_drop_actions() {
    for action in [
        figit::backend::RebaseTodoAction::Squash,
        figit::backend::RebaseTodoAction::Fixup,
        figit::backend::RebaseTodoAction::Drop,
    ] {
        let (temp, target, tip) = repository();
        let backend = backend::open(temp.path()).expect("open repository");
        let mut plan = backend
            .prepare_rebase_plan(&target)
            .expect("build todo plan");
        let entry = plan
            .blocks
            .iter_mut()
            .find_map(|block| match block {
                figit::backend::RebasePlanBlock::Commits(entries) => {
                    entries.iter_mut().find(|entry| entry.id == tip)
                }
                _ => None,
            })
            .expect("tip commit in todo");
        entry.action = action.clone();

        backend
            .execute_rebase_plan(&plan)
            .expect("execute commit action");

        assert_eq!(git_ok(temp.path(), &["rev-list", "--count", "HEAD"]), "2");
        assert_eq!(
            std::fs::read_to_string(temp.path().join("file.txt")).unwrap(),
            if action == figit::backend::RebaseTodoAction::Drop {
                "target\n"
            } else {
                "tip\n"
            }
        );
        if action == figit::backend::RebaseTodoAction::Fixup {
            assert_eq!(
                git_ok(temp.path(), &["show", "-s", "--format=%s", "HEAD"]),
                "target commit"
            );
        }
        if action == figit::backend::RebaseTodoAction::Drop {
            assert_eq!(
                git_ok(temp.path(), &["show", "-s", "--format=%s", "HEAD"]),
                "target commit"
            );
        }
    }
}

#[test]
fn planned_reword_survives_an_earlier_edit_pause() {
    let (temp, target, tip) = repository();
    let backend = backend::open(temp.path()).expect("open repository");
    let mut plan = backend
        .prepare_rebase_plan(&target)
        .expect("build todo plan");
    let entries = plan
        .blocks
        .iter_mut()
        .find_map(|block| match block {
            figit::backend::RebasePlanBlock::Commits(entries) if entries.len() == 2 => {
                Some(entries)
            }
            _ => None,
        })
        .expect("two commits in todo");
    entries[0].action = figit::backend::RebaseTodoAction::Edit;
    entries[1].action = figit::backend::RebaseTodoAction::Reword;
    entries[1].title = "planned tip".into();
    entries[1].description = "message saved across edit".into();

    let RebaseOutcome::StoppedAtCommit {
        target: stopped,
        parents,
        title,
        description,
    } = backend
        .execute_rebase_plan(&plan)
        .expect("start planned rebase")
    else {
        panic!("first planned action is edit");
    };
    backend
        .finish_rebase_edit(&stopped, &parents, &title, &description)
        .expect("finish first edit");
    assert_eq!(
        backend
            .continue_rebase()
            .expect("continue to planned reword"),
        RebaseOutcome::Completed
    );

    assert_eq!(
        git_ok(temp.path(), &["show", "-s", "--format=%B", "HEAD"]),
        "planned tip\n\nmessage saved across edit"
    );
    assert_ne!(git_ok(temp.path(), &["rev-parse", "HEAD"]), tip);
}

#[test]
fn reword_message_editor_does_not_consume_a_later_squash_editor() {
    let (temp, target, tip) = repository();
    let backend = backend::open(temp.path()).expect("open repository");
    let mut plan = backend
        .prepare_rebase_plan(&target)
        .expect("build todo plan");
    let entries = plan
        .blocks
        .iter_mut()
        .find_map(|block| match block {
            figit::backend::RebasePlanBlock::Commits(entries) if entries.len() == 2 => {
                Some(entries)
            }
            _ => None,
        })
        .expect("two commits in todo");
    entries[0].action = figit::backend::RebaseTodoAction::Reword;
    entries[0].title = "planned target".into();
    entries[1].action = figit::backend::RebaseTodoAction::Squash;

    backend
        .execute_rebase_plan(&plan)
        .expect("execute mixed reword/squash plan");

    assert_eq!(git_ok(temp.path(), &["rev-list", "--count", "HEAD"]), "2");
    assert_eq!(
        git_ok(temp.path(), &["show", "-s", "--format=%s", "HEAD"]),
        "planned target"
    );
    assert_ne!(git_ok(temp.path(), &["rev-parse", "HEAD"]), tip);
}

#[test]
fn fixture_rebase_plan_can_swap_the_commandline_and_multi_file_commits() {
    let temp = tui_fixture_repository();

    let first = fixture_commit(temp.path(), "ullamco laboris");
    let multi_file = fixture_commit(temp.path(), "nisi ut aliquip");
    let backend = backend::open(temp.path()).expect("open fixture repository");
    let mut plan = backend
        .prepare_rebase_plan(&first)
        .expect("build fixture rebase plan");
    let sequence = plan
        .blocks
        .iter_mut()
        .find_map(|block| match block {
            figit::backend::RebasePlanBlock::Commits(entries)
                if entries.iter().any(|entry| entry.id == first)
                    && entries.iter().any(|entry| entry.id == multi_file) =>
            {
                Some(entries)
            }
            _ => None,
        })
        .expect("both mainline commits share a reorderable sequence");
    let first_index = sequence.iter().position(|entry| entry.id == first).unwrap();
    let multi_index = sequence
        .iter()
        .position(|entry| entry.id == multi_file)
        .unwrap();
    assert_eq!(
        multi_index,
        first_index + 1,
        "fixture commits should be adjacent"
    );
    sequence.swap(first_index, multi_index);

    backend
        .execute_rebase_plan(&plan)
        .expect("reorder fixture commits");

    assert_eq!(
        git_ok(temp.path(), &["log", "--first-parent", "--format=%s", "-8"]),
        "merge esse cillum\nvoluptate velit\nin reprehenderit\nduis aute irure\nex ea commodo\nullamco laboris\nnisi ut aliquip\nmerge magna aliqua"
    );
    assert_eq!(
        git_ok(temp.path(), &["show", "--format=", "--name-only", "HEAD~5"]),
        "cli.txt"
    );
    assert_eq!(
        git_ok(temp.path(), &["show", "--format=", "--name-only", "HEAD~6"]),
        "CHANGELOG.md\ndeploy.txt\nsrc/deploy.rs\nsrc/main.rs\nsrc/rollout.rs"
    );
}

#[test]
fn reordered_consecutive_same_file_commits_can_be_resolved() {
    let temp = tui_fixture_repository();
    let path = temp.path();
    let ready = fixture_commit(path, "ex ea commodo");
    let blocked = fixture_commit(path, "duis aute irure");
    let backend = backend::open(path).expect("open fixture repository");
    let mut plan = backend
        .prepare_rebase_plan(&ready)
        .expect("prepare conflicting plan");
    let sequence = plan
        .blocks
        .iter_mut()
        .find_map(|block| match block {
            figit::backend::RebasePlanBlock::Commits(entries)
                if entries.iter().any(|entry| entry.id == ready)
                    && entries.iter().any(|entry| entry.id == blocked) =>
            {
                Some(entries)
            }
            _ => None,
        })
        .expect("same-file commits share a sequence");
    let ready_index = sequence.iter().position(|entry| entry.id == ready).unwrap();
    let blocked_index = sequence
        .iter()
        .position(|entry| entry.id == blocked)
        .unwrap();
    sequence.swap(ready_index, blocked_index);

    assert!(
        backend
            .execute_rebase_plan(&plan)
            .unwrap_err()
            .to_string()
            .contains("could not apply"),
        "the reordered patch should conflict because it depends on the previous file contents"
    );
    assert!(backend.rebase_in_progress().unwrap());
    let conflicts = backend
        .status()
        .unwrap()
        .into_iter()
        .filter(|entry| entry.is_conflict())
        .map(|entry| entry.path)
        .collect::<Vec<_>>();
    assert!(conflicts.contains(&GitPath::from("src/deploy.rs")));
    assert!(!conflicts.contains(&GitPath::from("src/main.rs")));
    let versions = backend
        .conflict_versions(&GitPath::from("src/deploy.rs"))
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(versions.base.as_deref().unwrap()).contains("Preview"));
    assert!(String::from_utf8_lossy(versions.ours.as_deref().unwrap())
        .contains("== DeployStatus::Ready"));
    assert!(String::from_utf8_lossy(versions.theirs.as_deref().unwrap()).contains("approved"));
    let commits = backend.conflict_commits().unwrap().unwrap();
    assert_eq!(commits.ours, git_ok(path, &["rev-parse", "HEAD"]));
    assert_eq!(commits.theirs, blocked);
    assert_eq!(
        commits.base,
        Some(git_ok(path, &["merge-base", "HEAD", "REBASE_HEAD"]))
    );
    let conflict_diff = backend
        .diff(
            &GitPath::from("src/deploy.rs"),
            figit::backend::DiffTarget::Worktree,
        )
        .expect("read deploy conflict diff");
    assert!(conflict_diff.contains("ours (current base)"));
    assert!(conflict_diff.contains("theirs (incoming commit)"));
    assert!(conflict_diff.contains("Preview"));
    assert!(conflict_diff.contains("approved"));

    // Pick the incoming approval-aware implementation. The earlier preview
    // commit then conflicts against it; keep the approval-aware result so the
    // final source and call site remain consistent.
    backend
        .resolve_conflict(&GitPath::from("src/deploy.rs"), ConflictSide::Theirs)
        .unwrap();
    assert!(backend.continue_rebase().is_err());
    assert!(backend
        .status()
        .unwrap()
        .iter()
        .any(|entry| entry.path == "src/deploy.rs" && entry.is_conflict()));
    backend
        .resolve_conflict(&GitPath::from("src/deploy.rs"), ConflictSide::Ours)
        .unwrap();
    assert_eq!(backend.continue_rebase().unwrap(), RebaseOutcome::Completed);
    let deploy_source = std::fs::read_to_string(path.join("src/deploy.rs")).unwrap();
    assert!(deploy_source.contains("DeployStatus::Preview"));
    assert!(deploy_source.contains("approved"));
    assert!(std::fs::read_to_string(path.join("src/main.rs"))
        .unwrap()
        .contains("can_deploy(status, approved)"));
}

#[test]
fn reordered_rollout_policy_commits_conflict_in_source() {
    let temp = tui_fixture_repository();
    let path = temp.path();
    let percentages = fixture_commit(path, "in reprehenderit");
    let health_window = fixture_commit(path, "voluptate velit");
    let backend = backend::open(path).expect("open fixture repository");
    let mut plan = backend
        .prepare_rebase_plan(&percentages)
        .expect("prepare rollout policy plan");
    let sequence = plan
        .blocks
        .iter_mut()
        .find_map(|block| match block {
            figit::backend::RebasePlanBlock::Commits(entries)
                if entries.iter().any(|entry| entry.id == percentages)
                    && entries.iter().any(|entry| entry.id == health_window) =>
            {
                Some(entries)
            }
            _ => None,
        })
        .expect("rollout policy commits share a reorderable sequence");
    let percentages_index = sequence
        .iter()
        .position(|entry| entry.id == percentages)
        .unwrap();
    let health_window_index = sequence
        .iter()
        .position(|entry| entry.id == health_window)
        .unwrap();
    sequence.swap(percentages_index, health_window_index);

    assert!(backend
        .execute_rebase_plan(&plan)
        .unwrap_err()
        .to_string()
        .contains("could not apply"));
    let conflicts = backend
        .status()
        .unwrap()
        .into_iter()
        .filter(|entry| entry.is_conflict())
        .map(|entry| entry.path)
        .collect::<Vec<_>>();
    assert!(
        conflicts.contains(&GitPath::from("src/rollout.rs")),
        "reordering the policy edits should conflict in rollout.rs: {conflicts:?}"
    );
    assert!(
        conflicts.contains(&GitPath::from("src/main.rs")),
        "the reordered policy also changes its call site: {conflicts:?}"
    );
    let versions = backend
        .conflict_versions(&GitPath::from("src/rollout.rs"))
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(versions.ours.as_deref().unwrap())
        .contains("max_unavailable: usize"));
    assert!(String::from_utf8_lossy(versions.theirs.as_deref().unwrap())
        .contains("minimum_healthy_seconds"));
    backend.abort_rebase().expect("abort conflict test rebase");
}

#[test]
fn interactive_plan_keeps_merge_rows_fixed_and_preserves_topology() {
    let (temp, merge) = merge_repository();
    let backend = backend::open(temp.path()).expect("open repository");
    let plan = backend
        .prepare_rebase_plan(&merge)
        .expect("prepare merge todo");
    assert!(plan.blocks.iter().any(|block| {
        matches!(block, figit::backend::RebasePlanBlock::Merge(entry) if entry.id == merge)
    }));

    assert_eq!(
        backend
            .execute_rebase_plan(&plan)
            .expect("execute merge-preserving plan"),
        RebaseOutcome::Completed
    );
    let rewritten_merge = git_ok(temp.path(), &["rev-list", "--parents", "-n", "1", "HEAD~1"]);
    assert_eq!(rewritten_merge.split_whitespace().count(), 3);
    assert_eq!(
        git_ok(temp.path(), &["log", "--format=%s", "-2"]),
        "tip commit\nmerge target"
    );
}

#[test]
fn edit_stops_rebase_at_selected_commit() {
    let (temp, target, _) = repository();
    let backend = backend::open(temp.path()).expect("open repository");

    let outcome = backend
        .rebase_commit(&target, &RebaseAction::Edit)
        .expect("start edit rebase");

    let RebaseOutcome::StoppedAtCommit { parents, .. } = outcome else {
        panic!("edit action should stop at the selected commit");
    };
    assert_eq!(
        git_ok(temp.path(), &["show", "-s", "--format=%s", "HEAD"]),
        "root commit"
    );
    let state_path = git_ok(temp.path(), &["rev-parse", "--git-path", "rebase-merge"]);
    assert!(
        temp.path().join(state_path).exists(),
        "edit should leave Git's rebase state active"
    );
    assert!(
        !git(temp.path(), &["diff", "--cached", "--quiet"])
            .status
            .success(),
        "the edited commit's changes should be staged"
    );
    std::fs::write(temp.path().join("edit.txt"), "added during edit\n").unwrap();
    git_ok(temp.path(), &["add", "edit.txt"]);
    backend
        .finish_rebase_edit(&target, &parents, "target commit", "")
        .expect("create replacement commit from staged edit");
    assert_eq!(
        backend.continue_rebase().expect("continue edited rebase"),
        RebaseOutcome::Completed
    );
    assert_eq!(
        git_ok(temp.path(), &["log", "--format=%s", "-3"]),
        "tip commit\ntarget commit\nroot commit"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("edit.txt")).unwrap(),
        "added during edit\n"
    );
}

#[test]
fn reword_supports_root_commit() {
    let (temp, _, _) = repository();
    let root = git_ok(temp.path(), &["rev-list", "--max-parents=0", "HEAD"]);
    let backend = backend::open(temp.path()).expect("open repository");

    let outcome = backend
        .rebase_commit(
            &root,
            &RebaseAction::Reword {
                title: "renamed root".into(),
                description: String::new(),
            },
        )
        .expect("reword root commit");

    assert_eq!(outcome, RebaseOutcome::Completed);
    assert_eq!(
        git_ok(temp.path(), &["log", "--format=%s", "-3"]),
        "tip commit\ntarget commit\nrenamed root"
    );
}

#[test]
fn reword_recreates_merge_commit_and_its_topology() {
    let (temp, target) = merge_repository();
    let backend = backend::open(temp.path()).expect("open repository");

    let outcome = backend
        .rebase_commit(
            &target,
            &RebaseAction::Reword {
                title: "renamed merge".into(),
                description: String::new(),
            },
        )
        .expect("reword merge commit");

    assert_eq!(outcome, RebaseOutcome::Completed);
    assert_eq!(
        git_ok(temp.path(), &["show", "-s", "--format=%s", "HEAD~1"]),
        "renamed merge"
    );
    let rewritten_merge = git_ok(temp.path(), &["rev-list", "--parents", "-n", "1", "HEAD~1"]);
    assert_eq!(
        rewritten_merge.split_whitespace().count(),
        3,
        "merge topology should be preserved"
    );
}

#[test]
fn edit_can_stop_at_a_merge_commit() {
    let (temp, target) = merge_repository();
    let backend = backend::open(temp.path()).expect("open repository");

    let outcome = backend
        .rebase_commit(&target, &RebaseAction::Edit)
        .expect("edit merge commit");

    let RebaseOutcome::StoppedAtCommit { parents, .. } = outcome else {
        panic!("edit action should stop at the selected merge commit");
    };
    assert_eq!(
        parents.len(),
        2,
        "edit state should retain both merge parents"
    );
    assert!(!git(temp.path(), &["diff", "--cached", "--quiet"])
        .status
        .success());
    std::fs::write(temp.path().join("edit.txt"), "merge edit\n").unwrap();
    git_ok(temp.path(), &["add", "edit.txt"]);
    backend
        .finish_rebase_edit(&target, &parents, "merge target", "")
        .expect("create replacement merge commit");
    assert_eq!(
        backend
            .continue_rebase()
            .expect("continue after merge edit"),
        RebaseOutcome::Completed
    );
    let rewritten_merge = git_ok(temp.path(), &["rev-list", "--parents", "-n", "1", "HEAD~1"]);
    assert_eq!(
        rewritten_merge.split_whitespace().count(),
        3,
        "merge topology should be retained"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("edit.txt")).unwrap(),
        "merge edit\n"
    );
}

#[test]
fn abort_rebase_restores_original_head_and_worktree() {
    let (temp, target, original_head) = repository();
    let backend = backend::open(temp.path()).expect("open repository");
    backend
        .rebase_commit(&target, &RebaseAction::Edit)
        .expect("stop for edit");

    assert!(backend.rebase_in_progress().expect("check rebase state"));
    backend.abort_rebase().expect("abort rebase");

    assert!(!backend
        .rebase_in_progress()
        .expect("check cleared rebase state"));
    assert_eq!(git_ok(temp.path(), &["rev-parse", "HEAD"]), original_head);
    assert!(git_ok(temp.path(), &["status", "--porcelain"]).is_empty());
}

#[test]
fn rebase_rewrite_refuses_dirty_worktree() {
    let (temp, target, _) = repository();
    std::fs::write(temp.path().join("uncommitted.txt"), "keep me\n").unwrap();
    let backend = backend::open(temp.path()).expect("open repository");

    let error = backend
        .rebase_commit(
            &target,
            &RebaseAction::Reword {
                title: "new title".into(),
                description: String::new(),
            },
        )
        .expect_err("dirty worktree must be protected");

    assert!(error.to_string().contains("commit or stash"));
    assert!(temp.path().join("uncommitted.txt").exists());
}

#[test]
fn conflict_side_choice_stages_the_resolution() {
    for (side, expected) in [
        (ConflictSide::Base, "base\n"),
        (ConflictSide::Ours, "ours\n"),
        (ConflictSide::Theirs, "theirs\n"),
    ] {
        let temp = conflicted_repository();
        let backend = backend::open(temp.path()).expect("open repository");
        let conflicts = backend.status().expect("read conflict status");
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].is_conflict());
        let versions = backend
            .conflict_versions(&GitPath::from("file.txt"))
            .unwrap()
            .unwrap();
        assert_eq!(versions.base.as_deref(), Some(b"base\n".as_slice()));
        assert_eq!(versions.ours.as_deref(), Some(b"ours\n".as_slice()));
        assert_eq!(versions.theirs.as_deref(), Some(b"theirs\n".as_slice()));
        let conflict_diff = backend
            .diff(
                &GitPath::from("file.txt"),
                figit::backend::DiffTarget::Worktree,
            )
            .expect("read conflict diff");
        assert!(
            conflict_diff.contains("ours = current base"),
            "{conflict_diff}"
        );
        assert!(conflict_diff.contains("theirs = incoming commit"));
        assert!(conflict_diff.contains("Working file with conflict markers"));

        backend
            .resolve_conflict(&GitPath::from("file.txt"), side)
            .expect("choose conflict side");

        assert_eq!(
            std::fs::read_to_string(temp.path().join("file.txt")).unwrap(),
            expected
        );
        let resolved = backend.status().expect("read resolved status");
        assert!(resolved.iter().all(|entry| !entry.is_conflict()));
        if side != ConflictSide::Ours {
            assert_eq!(resolved.len(), 1);
            assert_eq!(resolved[0].index, 'M');
            assert_eq!(resolved[0].worktree, ' ');
        } else {
            assert!(
                resolved.is_empty(),
                "choosing ours may leave no staged tree diff"
            );
        }
        git_ok(temp.path(), &["merge", "--abort"]);
    }
}

#[test]
fn choosing_conflict_side_previews_without_staging_until_confirmed() {
    let temp = conflicted_repository();
    let backend = backend::open(temp.path()).expect("open repository");

    backend
        .choose_conflict_side(&GitPath::from("file.txt"), ConflictSide::Theirs)
        .expect("preview incoming side");

    assert_eq!(
        std::fs::read_to_string(temp.path().join("file.txt")).unwrap(),
        "theirs\n"
    );
    let unresolved = backend
        .status()
        .expect("conflict remains until confirmation");
    assert_eq!(unresolved.len(), 1);
    assert!(unresolved[0].is_conflict());

    backend
        .stage(&GitPath::from("file.txt"))
        .expect("confirm result by staging");
    let confirmed = backend.status().expect("read confirmed status");
    assert!(confirmed.iter().all(|entry| !entry.is_conflict()));
    assert_eq!(confirmed.len(), 1);
    assert_eq!(confirmed[0].index, 'M');
}

#[test]
fn index_patches_stage_and_unstage_only_the_selected_hunk() {
    let temp = tempfile::tempdir().expect("create hunk staging repository");
    let path = temp.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    let base = "first base\ncontext 1\ncontext 2\ncontext 3\ncontext 4\ncontext 5\ncontext 6\ncontext 7\ncontext 8\ncontext 9\ncontext 10\nsecond base\ncontext 11\ncontext 12\ncontext 13\ncontext 14\ncontext 15\ncontext 16\ncontext 17\ncontext 18\ncontext 19\ncontext 20\nthird base\n";
    std::fs::write(path.join("file.txt"), base).unwrap();
    git_ok(path, &["add", "file.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "base file"]);
    std::fs::write(
        path.join("file.txt"),
        base.replace("first base", "first ours")
            .replace("third base", "third ours"),
    )
    .unwrap();
    let backend = backend::open(path).expect("open repository");

    let diff = backend
        .diff(&GitPath::from("file.txt"), DiffTarget::Worktree)
        .unwrap();
    assert_eq!(
        diff.lines().filter(|line| line.starts_with("@@")).count(),
        2
    );
    assert_eq!(
        backend
            .diff_hunk_counts(&GitPath::from("file.txt"))
            .unwrap(),
        (0, 2)
    );
    backend
        .apply_hunk(&GitPath::from("file.txt"), DiffTarget::Worktree, 0)
        .expect("stage first hunk");
    let staged = git_ok(path, &["diff", "--cached", "--", "file.txt"]);
    let unstaged = git_ok(path, &["diff", "--", "file.txt"]);
    assert!(staged.contains("+first ours"));
    assert!(!staged.contains("+third ours"));
    assert!(unstaged.contains("+third ours"));
    assert!(!unstaged.contains("+first ours"));
    assert_eq!(
        backend
            .diff_hunk_counts(&GitPath::from("file.txt"))
            .unwrap(),
        (1, 1)
    );

    backend
        .apply_hunk(&GitPath::from("file.txt"), DiffTarget::Index, 0)
        .expect("unstage the selected cached hunk");
    assert!(git_ok(path, &["diff", "--cached", "--", "file.txt"]).is_empty());
    let unstaged = git_ok(path, &["diff", "--", "file.txt"]);
    assert!(unstaged.contains("+first ours"));
    assert!(unstaged.contains("+third ours"));
    assert_eq!(
        backend
            .diff_hunk_counts(&GitPath::from("file.txt"))
            .unwrap(),
        (0, 2)
    );
}

#[test]
fn index_patch_can_stage_and_unstage_one_changed_line() {
    let temp = tempfile::tempdir().expect("create line staging repository");
    let path = temp.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    std::fs::write(path.join("file.txt"), "alpha\nbeta\ngamma\n").unwrap();
    git_ok(path, &["add", "file.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "base file"]);
    std::fs::write(
        path.join("file.txt"),
        "alpha\nbeta changed\ngamma changed\n",
    )
    .unwrap();
    let backend = backend::open(path).expect("open repository");
    let selected = DiffLineSelection {
        old_line: None,
        new_line: Some(2),
        text: "+beta changed".into(),
    };

    backend
        .apply_diff_line(&GitPath::from("file.txt"), DiffTarget::Worktree, &selected)
        .expect("stage the selected added line only");
    let staged = git_ok(path, &["diff", "--cached", "--", "file.txt"]);
    let unstaged = git_ok(path, &["diff", "--", "file.txt"]);
    assert!(
        staged.contains("+beta changed"),
        "selected line missing from index:\n{staged}"
    );
    assert!(!staged.contains("+gamma changed"));
    assert!(unstaged.contains("gamma changed"));

    let staged_diff = backend
        .diff(&GitPath::from("file.txt"), DiffTarget::Index)
        .unwrap();
    let new_line = staged_diff
        .lines()
        .find(|line| line.starts_with("+beta changed"))
        .expect("staged added line")
        .to_string();
    let selected = DiffLineSelection {
        old_line: None,
        new_line: Some(3),
        text: new_line,
    };
    backend
        .apply_diff_line(&GitPath::from("file.txt"), DiffTarget::Index, &selected)
        .unwrap_or_else(|error| panic!("unstage the selected added line: {error}\n{staged_diff}"));
    assert!(git_ok(path, &["diff", "--cached", "--", "file.txt"]).is_empty());
    let unstaged = git_ok(path, &["diff", "--", "file.txt"]);
    assert!(unstaged.contains("+beta changed"));
    assert!(unstaged.contains("+gamma changed"));

    let selected = DiffLineSelection {
        old_line: Some(2),
        new_line: None,
        text: "-beta".into(),
    };
    backend
        .apply_diff_line(&GitPath::from("file.txt"), DiffTarget::Worktree, &selected)
        .expect("stage the selected removed line");
    let staged = git_ok(path, &["diff", "--cached", "--", "file.txt"]);
    assert!(staged.contains("-beta"));
    assert!(!staged.contains("-gamma"));
    backend
        .apply_diff_line(&GitPath::from("file.txt"), DiffTarget::Index, &selected)
        .expect("unstage the selected removed line");
    let staged = git_ok(path, &["diff", "--cached", "--", "file.txt"]);
    assert!(
        staged.is_empty(),
        "removed line should be restored to the index base:\n{staged}"
    );
}

#[test]
fn stash_push_apply_pop_and_drop_preserve_worktree_contents() {
    let temp = tempfile::tempdir().expect("create stash repository");
    let path = temp.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    std::fs::write(path.join("tracked.txt"), "base\n").unwrap();
    git_ok(path, &["add", "tracked.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "base"]);
    std::fs::write(path.join("tracked.txt"), "changed\n").unwrap();
    std::fs::write(path.join("untracked.txt"), "new file\n").unwrap();
    let backend = backend::open(path).expect("open stash repository");

    assert!(backend.stash_list().unwrap().is_empty());
    backend
        .stash_push("before experiment", true)
        .expect("stash tracked and untracked changes");
    let stashes = backend.stash_list().unwrap();
    assert_eq!(stashes.len(), 1);
    assert!(stashes[0].message.contains("before experiment"));
    assert_eq!(
        std::fs::read_to_string(path.join("tracked.txt")).unwrap(),
        "base\n"
    );
    assert!(!path.join("untracked.txt").exists());

    backend
        .stash_apply(&stashes[0].reference)
        .expect("apply stash without dropping it");
    assert_eq!(backend.stash_list().unwrap().len(), 1);
    assert_eq!(
        std::fs::read_to_string(path.join("tracked.txt")).unwrap(),
        "changed\n"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("untracked.txt")).unwrap(),
        "new file\n"
    );

    std::fs::write(path.join("tracked.txt"), "base\n").unwrap();
    std::fs::remove_file(path.join("untracked.txt")).unwrap();
    let applied = backend.stash_list().unwrap()[0].reference.clone();
    backend
        .stash_pop(&applied)
        .expect("pop stash onto clean worktree");
    assert!(backend.stash_list().unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(path.join("tracked.txt")).unwrap(),
        "changed\n"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("untracked.txt")).unwrap(),
        "new file\n"
    );

    std::fs::write(path.join("tracked.txt"), "drop me\n").unwrap();
    backend.stash_push("drop test", false).unwrap();
    let entry = backend.stash_list().unwrap().remove(0);
    backend
        .stash_drop(&entry.reference)
        .expect("drop selected stash");
    assert!(backend.stash_list().unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(path.join("tracked.txt")).unwrap(),
        "base\n"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("untracked.txt")).unwrap(),
        "new file\n"
    );
}

#[test]
fn reflog_undo_moves_head_and_preserves_index_and_worktree_edits() {
    let (temp, previous, current) = repository();
    let path = temp.path();
    let backend = backend::open(path).expect("open repository");
    assert_eq!(git_ok(path, &["rev-parse", "HEAD"]), current);

    std::fs::write(path.join("file.txt"), "staged edit\n").unwrap();
    backend
        .stage(&GitPath::from("file.txt"))
        .expect("stage an edit");
    std::fs::write(path.join("file.txt"), "unstaged edit\n").unwrap();
    let index_before = git_ok(path, &["write-tree"]);
    let unstaged_before = git_ok(path, &["diff", "--", "file.txt"]);

    let preview = backend
        .undo_options()
        .expect("read HEAD reflog")
        .into_iter()
        .find(|entry| entry.depth == 1)
        .expect("latest commit should be undoable");
    assert_eq!(preview.current_id, current);
    assert_eq!(preview.previous_id, previous);
    assert!(preview.latest_message.contains("tip commit"));

    backend
        .undo_ref_change(&preview)
        .expect("move HEAD back without resetting the index or worktree");
    assert_eq!(git_ok(path, &["rev-parse", "HEAD"]), previous);
    assert_eq!(git_ok(path, &["write-tree"]), index_before);
    assert_eq!(git_ok(path, &["diff", "--", "file.txt"]), unstaged_before);
    assert_eq!(
        std::fs::read_to_string(path.join("file.txt")).unwrap(),
        "unstaged edit\n"
    );
}

#[test]
fn reflog_undo_restores_the_branch_before_checkout() {
    let (temp, _, _) = repository();
    let path = temp.path();
    git_ok(path, &["checkout", "--quiet", "-b", "undo-target"]);
    let backend = backend::open(path).expect("open repository");
    let preview = backend
        .undo_options()
        .expect("read HEAD reflog")
        .into_iter()
        .find(|entry| entry.depth == 1)
        .expect("checkout should be undoable");
    assert!(preview
        .latest_message
        .starts_with("checkout: moving from main to undo-target"));
    assert_eq!(preview.restore_branch.as_deref(), Some("main"));

    backend
        .undo_ref_change(&preview)
        .expect("check out the branch that was active previously");
    assert_eq!(git_ok(path, &["branch", "--show-current"]), "main");
}

#[test]
fn reflog_undo_rejects_a_stale_confirmation() {
    let (temp, _, _) = repository();
    let path = temp.path();
    let backend = backend::open(path).expect("open repository");
    let preview = backend
        .undo_options()
        .expect("read HEAD reflog")
        .into_iter()
        .find(|entry| entry.depth == 1)
        .expect("latest commit should be undoable");

    std::fs::write(path.join("file.txt"), "external commit\n").unwrap();
    git_ok(path, &["commit", "--quiet", "-am", "external commit"]);
    let current = git_ok(path, &["rev-parse", "HEAD"]);
    let error = backend
        .undo_ref_change(&preview)
        .expect_err("the confirmation should be invalidated by a new commit");

    assert!(error.to_string().contains("history changed"));
    assert_eq!(git_ok(path, &["rev-parse", "HEAD"]), current);
}

#[test]
fn reflog_undo_rebase_uses_the_original_branch_tip() {
    let temp = tempfile::tempdir().expect("create rebase undo repository");
    let path = temp.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    std::fs::write(path.join("root.txt"), "root\n").unwrap();
    git_ok(path, &["add", "root.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "root"]);

    git_ok(path, &["checkout", "--quiet", "-b", "feature"]);
    std::fs::write(path.join("feature.txt"), "feature\n").unwrap();
    git_ok(path, &["add", "feature.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "feature commit"]);
    let original_feature = git_ok(path, &["rev-parse", "HEAD"]);

    git_ok(path, &["checkout", "--quiet", "main"]);
    std::fs::write(path.join("main.txt"), "main\n").unwrap();
    git_ok(path, &["add", "main.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "main commit"]);
    git_ok(path, &["checkout", "--quiet", "feature"]);
    git_ok(path, &["rebase", "--quiet", "main"]);
    let backend = backend::open(path).expect("open repository");

    let preview = backend
        .undo_options()
        .expect("read HEAD reflog")
        .into_iter()
        .find(|entry| entry.depth == 1)
        .expect("completed rebase should be undoable");
    assert!(preview.latest_message.starts_with("rebase (finish):"));
    assert_eq!(preview.previous_id, original_feature);

    backend
        .undo_ref_change(&preview)
        .expect("restore the branch tip from before rebase");
    assert_eq!(git_ok(path, &["branch", "--show-current"]), "feature");
    assert_eq!(git_ok(path, &["rev-parse", "HEAD"]), original_feature);
}

#[test]
fn reflog_undo_can_select_an_older_entry() {
    let (temp, _, _) = repository();
    let path = temp.path();
    let backend = backend::open(path).expect("open repository");
    let older = git_ok(path, &["rev-parse", "HEAD~2"]);
    let option = backend
        .undo_options()
        .expect("list HEAD reflog entries")
        .into_iter()
        .find(|entry| entry.depth == 2)
        .expect("second previous reflog entry should be available");
    assert_eq!(option.previous_id, older);

    backend
        .undo_ref_change(&option)
        .expect("restore the selected older reflog entry");
    assert_eq!(git_ok(path, &["rev-parse", "HEAD"]), older);
}

#[test]
fn fetch_updates_remote_tracking_refs_and_graph() {
    let temp = tempfile::tempdir().expect("create fetch test directory");
    let root = temp.path();
    let remote = root.join("remote.git");
    let local = root.join("local");
    let peer = root.join("peer");
    let remote_arg = remote.to_str().unwrap();
    let local_arg = local.to_str().unwrap();
    let peer_arg = peer.to_str().unwrap();

    git_ok(
        root,
        &[
            "init",
            "--quiet",
            "--bare",
            "--initial-branch=main",
            remote_arg,
        ],
    );
    git_ok(
        root,
        &["init", "--quiet", "--initial-branch=main", local_arg],
    );
    configure_user(&local);
    std::fs::write(local.join("base.txt"), "base\n").unwrap();
    git_ok(&local, &["add", "base.txt"]);
    git_ok(&local, &["commit", "--quiet", "-m", "base"]);
    git_ok(&local, &["remote", "add", "origin", remote_arg]);
    git_ok(
        &local,
        &["push", "--quiet", "--set-upstream", "origin", "main"],
    );
    let initial_remote_head = git_ok(&local, &["rev-parse", "refs/remotes/origin/main"]);
    git_ok(&local, &["remote", "add", "upstream", remote_arg]);
    git_ok(&local, &["config", "branch.main.remote", "upstream"]);
    git_ok(&local, &["config", "branch.main.merge", "refs/heads/main"]);
    git_ok(&local, &["config", "branch.main.pushRemote", "origin"]);
    let backend = backend::open(&local).expect("open local repository");
    assert_eq!(
        backend.default_remote(false).unwrap().as_deref(),
        Some("upstream")
    );
    assert_eq!(
        backend.default_remote(true).unwrap().as_deref(),
        Some("origin")
    );
    assert_eq!(
        backend
            .default_remote_for_branch("main", true)
            .unwrap()
            .as_deref(),
        Some("origin")
    );

    std::fs::write(local.join("local.txt"), "local only\n").unwrap();
    git_ok(&local, &["add", "local.txt"]);
    git_ok(&local, &["commit", "--quiet", "-m", "local update"]);
    backend
        .push("upstream", "main", 0)
        .expect("push to the selected remote");
    let pushed_head = git_ok(&local, &["rev-parse", "HEAD"]);
    assert_eq!(
        git_ok(&local, &["rev-parse", "refs/remotes/upstream/main"]),
        pushed_head
    );
    assert_eq!(
        git_ok(&local, &["rev-parse", "refs/remotes/origin/main"]),
        initial_remote_head
    );

    std::fs::write(local.join("local-second.txt"), "another local commit\n").unwrap();
    git_ok(&local, &["add", "local-second.txt"]);
    git_ok(&local, &["commit", "--quiet", "-m", "another local update"]);

    git_ok(root, &["clone", "--quiet", remote_arg, peer_arg]);
    configure_peer(&peer);
    std::fs::write(peer.join("remote.txt"), "fetched\n").unwrap();
    git_ok(&peer, &["add", "remote.txt"]);
    git_ok(&peer, &["commit", "--quiet", "-m", "remote update"]);
    git_ok(&peer, &["push", "--quiet"]);
    let remote_head = git_ok(&peer, &["rev-parse", "HEAD"]);

    backend.fetch("upstream").expect("fetch selected remote");

    assert_eq!(
        git_ok(&local, &["rev-parse", "refs/remotes/upstream/main"]),
        remote_head
    );
    assert_eq!(
        git_ok(&local, &["rev-parse", "refs/remotes/origin/main"]),
        initial_remote_head
    );
    assert_eq!(
        git_ok(&local, &["config", "branch.main.remote"]),
        "upstream"
    );
    assert_eq!(
        git_ok(&local, &["config", "branch.main.pushRemote"]),
        "origin"
    );
    let graph = backend.build_graph().expect("rebuild graph after fetch");
    assert!(graph.commits.iter().any(|commit| commit.id == remote_head));
    assert!(graph
        .refs
        .iter()
        .any(|reference| { reference.name == "upstream/main" && reference.id == remote_head }));
    let tracking = graph
        .tracking_for_branch("main")
        .expect("main should have upstream tracking data");
    assert_eq!(tracking.upstream, "upstream/main");
    assert_eq!((tracking.ahead, tracking.behind), (1, 1));
}

#[test]
fn repository_without_remotes_has_no_remote_choices() {
    let (temp, _, _) = repository();
    let backend = backend::open(temp.path()).expect("open repository");
    assert!(backend
        .remotes()
        .expect("list configured remotes")
        .is_empty());
}

#[test]
fn remote_branch_rename_and_delete_use_the_configured_upstream() {
    let repository = tempfile::tempdir().expect("create repository");
    let path = repository.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    std::fs::write(path.join("file.txt"), "base\n").unwrap();
    git_ok(path, &["add", "file.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "base"]);

    let remote_dir = tempfile::tempdir().expect("create remote directory");
    let remote_path = remote_dir.path().join("upstream.git");
    git_ok(
        path,
        &[
            "init",
            "--quiet",
            "--bare",
            "--initial-branch=main",
            remote_path.to_str().unwrap(),
        ],
    );
    git_ok(
        path,
        &["remote", "add", "upstream", remote_path.to_str().unwrap()],
    );
    git_ok(path, &["branch", "feature"]);
    git_ok(path, &["checkout", "--quiet", "feature"]);
    git_ok(
        path,
        &["push", "--quiet", "--set-upstream", "upstream", "feature"],
    );
    git_ok(path, &["checkout", "--quiet", "main"]);

    let backend = backend::open(path).expect("open repository");
    backend
        .rename_branch("feature", "renamed", true)
        .expect("rename local and upstream branch");
    assert!(git(
        path,
        &["show-ref", "--verify", "--quiet", "refs/heads/renamed"]
    )
    .status
    .success());
    assert!(!git(
        path,
        &["show-ref", "--verify", "--quiet", "refs/heads/feature"]
    )
    .status
    .success());
    assert!(git(
        &remote_path,
        &["show-ref", "--verify", "--quiet", "refs/heads/renamed"]
    )
    .status
    .success());
    assert!(!git(
        &remote_path,
        &["show-ref", "--verify", "--quiet", "refs/heads/feature"]
    )
    .status
    .success());
    assert_eq!(
        git_ok(path, &["config", "branch.renamed.remote"]),
        "upstream"
    );
    assert_eq!(
        git_ok(path, &["config", "branch.renamed.merge"]),
        "refs/heads/renamed"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        git_ok(path, &["branch", "protected", "main"]);
        git_ok(path, &["checkout", "--quiet", "protected"]);
        git_ok(
            path,
            &["push", "--quiet", "--set-upstream", "upstream", "protected"],
        );
        git_ok(path, &["checkout", "--quiet", "main"]);
        let hook = remote_path.join("hooks/pre-receive");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&hook, permissions).unwrap();

        assert!(backend
            .rename_branch("protected", "protected-new", true)
            .is_err());
        assert!(git(
            path,
            &["show-ref", "--verify", "--quiet", "refs/heads/protected"]
        )
        .status
        .success());
        assert!(!git(
            path,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                "refs/heads/protected-new"
            ]
        )
        .status
        .success());
        assert!(git(
            &remote_path,
            &["show-ref", "--verify", "--quiet", "refs/heads/protected"]
        )
        .status
        .success());

        assert!(backend.delete_branch("protected", true).is_err());
        assert!(git(
            path,
            &["show-ref", "--verify", "--quiet", "refs/heads/protected"]
        )
        .status
        .success());
        assert!(git(
            &remote_path,
            &["show-ref", "--verify", "--quiet", "refs/heads/protected"]
        )
        .status
        .success());
        std::fs::remove_file(hook).unwrap();
    }

    git_ok(path, &["checkout", "--quiet", "main"]);
    backend
        .delete_branch("renamed", true)
        .expect("delete local and upstream branch");
    assert!(!git(
        path,
        &["show-ref", "--verify", "--quiet", "refs/heads/renamed"]
    )
    .status
    .success());
    assert!(!git(
        &remote_path,
        &["show-ref", "--verify", "--quiet", "refs/heads/renamed"]
    )
    .status
    .success());
}

#[test]
fn oversized_files_are_not_diffed() {
    let temp = tempfile::tempdir().expect("create oversized repository");
    let path = temp.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    // Force a low threshold so the test stays small and fast.
    git_ok(path, &["config", "core.bigFileThreshold", "1k"]);
    std::fs::write(path.join("big.txt"), "small\n").unwrap();
    std::fs::write(path.join("small.txt"), "small\n").unwrap();
    git_ok(path, &["add", "big.txt", "small.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "base"]);
    let big = "x".repeat(4096);
    std::fs::write(path.join("big.txt"), format!("{big}\n")).unwrap();
    std::fs::write(path.join("small.txt"), "changed\n").unwrap();
    let backend = backend::open(path).expect("open repository");

    let diff = backend
        .diff(&GitPath::from("big.txt"), DiffTarget::Worktree)
        .expect("diff oversized file");
    assert!(
        diff.contains("larger than"),
        "oversized files should explain themselves instead of returning content:\n{diff}"
    );
    assert!(
        !diff.contains(&big),
        "oversized content must not be included"
    );
    assert_eq!(
        backend
            .diff_hunk_counts(&GitPath::from("big.txt"))
            .expect("hunk counts"),
        (0, 0)
    );
    // Git itself reports no line counts above `core.bigFileThreshold`, so the
    // row shows zeros rather than a misleading count.
    assert_eq!(
        backend
            .diff_stats(&GitPath::from("big.txt"), false)
            .expect("stats"),
        (0, 0)
    );

    // A large staged blob must be guarded even if the worktree copy is small.
    std::fs::write(path.join("big.txt"), format!("{big}\n")).unwrap();
    git_ok(path, &["add", "big.txt"]);
    std::fs::write(path.join("big.txt"), "small again\n").unwrap();
    let staged = backend
        .diff(&GitPath::from("big.txt"), DiffTarget::Index)
        .expect("diff oversized staged blob");
    assert!(
        staged.contains("larger than"),
        "staged blob was diffed:\n{staged}"
    );
    assert_eq!(
        backend
            .diff_hunk_counts(&GitPath::from("big.txt"))
            .expect("staged hunk counts"),
        (0, 0)
    );
    assert_eq!(
        backend
            .diff_stats(&GitPath::from("big.txt"), true)
            .expect("staged stats"),
        (0, 0)
    );

    // Untracked paths take a separate --no-index path and need the same guard.
    std::fs::write(path.join("loose.txt"), format!("{big}\n")).unwrap();
    let loose = backend
        .diff(&GitPath::from("loose.txt"), DiffTarget::Worktree)
        .expect("diff oversized untracked file");
    assert!(
        loose.contains("larger than"),
        "untracked file was diffed:\n{loose}"
    );
    assert!(
        !loose.contains(&big),
        "oversized untracked content was included"
    );
    assert_eq!(
        backend
            .diff_hunk_counts(&GitPath::from("loose.txt"))
            .expect("untracked hunk counts"),
        (0, 0)
    );
    assert_eq!(
        backend
            .diff_stats(&GitPath::from("loose.txt"), false)
            .expect("untracked stats"),
        (0, 0)
    );

    // A file under the limit is diffed normally.
    let small = backend
        .diff(&GitPath::from("small.txt"), DiffTarget::Worktree)
        .expect("diff small file");
    assert!(
        small.contains("+changed"),
        "small files still diff:\n{small}"
    );
}

#[test]
fn oversized_conflict_stages_are_not_loaded_into_the_resolver() {
    let temp = tempfile::tempdir().expect("create conflict repository");
    let path = temp.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    git_ok(path, &["config", "core.bigFileThreshold", "1k"]);
    std::fs::write(path.join("large.txt"), "base\n").unwrap();
    git_ok(path, &["add", "large.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "base"]);

    let ours = format!("{}\n", "ours".repeat(1024));
    let theirs = format!("{}\n", "theirs".repeat(1024));
    git_ok(path, &["checkout", "--quiet", "-b", "feature"]);
    std::fs::write(path.join("large.txt"), &theirs).unwrap();
    git_ok(path, &["commit", "--quiet", "-am", "theirs"]);
    git_ok(path, &["checkout", "--quiet", "main"]);
    std::fs::write(path.join("large.txt"), &ours).unwrap();
    git_ok(path, &["commit", "--quiet", "-am", "ours"]);
    assert!(!git(path, &["merge", "--no-ff", "feature"]).status.success());

    let backend = backend::open(path).expect("open conflicted repository");
    let diff = backend
        .diff(&GitPath::from("large.txt"), DiffTarget::Worktree)
        .expect("large conflict notice");
    assert!(
        diff.contains("larger than"),
        "conflict diff was loaded:\n{diff}"
    );
    assert!(backend
        .conflict_versions(&GitPath::from("large.txt"))
        .expect("large conflict stages")
        .is_none());
}

#[test]
fn untracked_files_render_as_all_added_diffs() {
    let temp = tempfile::tempdir().expect("create untracked repository");
    let path = temp.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    std::fs::write(path.join("tracked.txt"), "tracked\n").unwrap();
    git_ok(path, &["add", "tracked.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "base"]);
    std::fs::write(path.join("brand-new.txt"), "alpha\nbeta\n").unwrap();
    let backend = backend::open(path).expect("open repository");

    let diff = backend
        .diff(&GitPath::from("brand-new.txt"), DiffTarget::Worktree)
        .expect("diff untracked file");
    assert!(
        diff.contains("@@ -0,0 +1,2 @@"),
        "expected an all-added hunk:\n{diff}"
    );
    assert!(
        diff.contains("+alpha") && diff.contains("+beta"),
        "content missing:\n{diff}"
    );
    assert!(
        !diff.contains("-alpha"),
        "untracked files have no deletions:\n{diff}"
    );

    assert_eq!(
        backend
            .diff_stats(&GitPath::from("brand-new.txt"), false)
            .expect("numstat"),
        (2, 0)
    );
    assert_eq!(
        backend
            .diff_hunk_counts(&GitPath::from("brand-new.txt"))
            .expect("hunk counts"),
        (0, 1)
    );
    // A tracked file keeps using the ordinary diff path.
    std::fs::write(path.join("tracked.txt"), "tracked changed\n").unwrap();
    let tracked = backend
        .diff(&GitPath::from("tracked.txt"), DiffTarget::Worktree)
        .expect("diff tracked file");
    assert!(tracked.contains("-tracked") && tracked.contains("+tracked changed"));

    // Hunk-staging an untracked file stages the whole new file.
    backend
        .apply_hunk(&GitPath::from("brand-new.txt"), DiffTarget::Worktree, 0)
        .expect("stage the untracked hunk");
    let staged = git_ok(
        path,
        &["diff", "--cached", "--numstat", "--", "brand-new.txt"],
    );
    assert_eq!(staged, "2\t0\tbrand-new.txt");
    assert!(git_ok(path, &["status", "--porcelain", "--", "brand-new.txt"]).starts_with("A "));
}

#[test]
fn status_diff_stats_batch_tracked_paths_and_keep_pathspecs_literal() {
    let temp = tempfile::tempdir().expect("create repository");
    let path = temp.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    std::fs::write(path.join("alpha.txt"), "alpha base\n").unwrap();
    std::fs::write(path.join("literal[1].txt"), "bracket base\n").unwrap();
    git_ok(path, &["add", "."]);
    git_ok(path, &["commit", "--quiet", "-m", "base"]);

    std::fs::write(path.join("alpha.txt"), "alpha worktree\n").unwrap();
    std::fs::write(path.join("literal[1].txt"), "bracket staged\n").unwrap();
    git_ok(path, &["add", "--", "literal[1].txt"]);
    std::fs::write(path.join("literal[1].txt"), "bracket worktree\n").unwrap();
    std::fs::write(path.join("loose.txt"), "one\ntwo\n").unwrap();

    let backend = backend::open(path).expect("open repository");
    let status = backend.status().expect("read status");
    let stats = backend
        .status_diff_stats(&status)
        .expect("collect batched stats");
    let stats_for = |path: &str, staged: bool| {
        stats
            .iter()
            .find(|stat| stat.path == path && stat.staged == staged)
            .map(|stat| (stat.added, stat.removed))
    };

    assert_eq!(stats_for("alpha.txt", false), Some((1, 1)));
    assert_eq!(stats_for("alpha.txt", true), Some((0, 0)));
    assert_eq!(stats_for("literal[1].txt", true), Some((1, 1)));
    assert_eq!(stats_for("literal[1].txt", false), Some((1, 1)));
    assert_eq!(stats_for("loose.txt", false), Some((2, 0)));

    let literal_path = GitPath::from("literal[1].txt");
    assert!(backend
        .diff(&literal_path, DiffTarget::Worktree)
        .unwrap()
        .contains("+bracket worktree"));
    backend
        .stage(&literal_path)
        .expect("pathspec metacharacters remain literal when staging");
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_paths_remain_operable_without_lossy_round_tripping() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let temp = tempfile::tempdir().expect("create repository");
    let path = temp.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    std::fs::write(path.join("base.txt"), "base\n").unwrap();
    git_ok(path, &["add", "base.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "base"]);

    let name = OsString::from_vec(b"non-utf8-\xff.txt".to_vec());
    std::fs::write(path.join(&name), "untracked content\n").unwrap();
    let backend = backend::open(path).expect("open repository");
    let status = backend.status().expect("read status");
    let entry = status
        .iter()
        .find(|entry| entry.path.as_bytes() == b"non-utf8-\xff.txt")
        .expect("retain the original filename bytes");
    assert!(entry.path.display().contains("\\xFF"));

    let diff = backend
        .diff(&entry.path, DiffTarget::Worktree)
        .expect("diff non-UTF-8 path");
    assert!(diff.contains("+untracked content"));
    assert_eq!(backend.diff_stats(&entry.path, false).unwrap(), (1, 0));
    backend.stage(&entry.path).expect("stage non-UTF-8 path");
    assert!(git(path, &["diff", "--cached", "--name-only", "-z"])
        .stdout
        .windows(b"non-utf8-\xff.txt".len())
        .any(|window| window == b"non-utf8-\xff.txt"));
}

#[test]
fn commit_diff_stats_batch_reports_every_changed_path() {
    let temp = tempfile::tempdir().expect("create diff stats repository");
    let path = temp.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    std::fs::write(path.join("one.txt"), "before\nkeep\n").unwrap();
    std::fs::write(path.join("two.txt"), "remove me\n").unwrap();
    git_ok(path, &["add", "one.txt", "two.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "base"]);
    std::fs::write(path.join("one.txt"), "after\nkeep\nextra\n").unwrap();
    std::fs::remove_file(path.join("two.txt")).unwrap();
    git_ok(path, &["add", "one.txt", "two.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "change two files"]);

    let backend = backend::open(path).expect("open diff stats repository");
    let head = git_ok(path, &["rev-parse", "HEAD"]);
    let stats = backend
        .commit_diff_stats(&head)
        .expect("read batched numstat");
    let stats = stats
        .into_iter()
        .map(|(path, added, removed)| (path, (added, removed)))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(stats.get(&GitPath::from("one.txt")), Some(&(2, 1)));
    assert_eq!(stats.get(&GitPath::from("two.txt")), Some(&(0, 1)));
}

#[test]
fn merge_commit_files_diff_and_stats_compare_against_first_parent() {
    let (temp, merge_commit) = merge_repository();
    let backend = backend::open(temp.path()).expect("open merge repository");

    let files = backend
        .commit_files(&merge_commit)
        .expect("list merge changes");
    assert!(
        files.iter().any(|entry| entry.path == "side.txt"),
        "merge's first-parent changes should include side.txt: {files:?}"
    );
    let diff = backend
        .commit_diff(&merge_commit, &GitPath::from("side.txt"))
        .expect("get first-parent merge diff");
    assert!(
        diff.contains("+side"),
        "merge diff should show the incoming file:\n{diff}"
    );
    let stats = backend
        .commit_diff_stats(&merge_commit)
        .expect("get first-parent merge numstat");
    assert!(
        stats
            .iter()
            .any(|(path, added, removed)| { path == "side.txt" && *added == 1 && *removed == 0 }),
        "merge stats should include the incoming file: {stats:?}"
    );
}

#[test]
fn root_commit_files_and_diff_are_not_empty() {
    let (temp, _, _) = repository();
    let path = temp.path();
    let root = git_ok(path, &["rev-parse", "HEAD~2"]);
    let backend = backend::open(path).expect("open repository");

    assert!(backend
        .commit_files(&root)
        .expect("list root files")
        .iter()
        .any(|entry| entry.path == "file.txt"));
    assert!(backend
        .commit_diff(&root, &GitPath::from("file.txt"))
        .expect("diff root file")
        .contains("+root"));
}

#[test]
fn graph_pages_are_exact_prefixes_of_the_full_graph() {
    let (temp, _, _) = repository();
    let backend = backend::open(temp.path()).expect("open repository");
    let full = backend.build_graph().expect("build complete graph");

    // A one-commit page is a prefix and reports that more history remains.
    let first = backend.graph_window(0, 1).expect("first page");
    assert_eq!(first.commits.len(), 1);
    assert_eq!(first.commits[0].id, full.commits[0].id);
    assert!(!first.complete, "one commit cannot be the whole history");
    assert_eq!(first.refs, full.refs);
    assert_eq!(first.head_id, full.head_id);

    // Paging through in chunks reassembles the full order exactly.
    let mut paged = Vec::new();
    let mut skip = 0;
    loop {
        let page = backend.graph_window(skip, 2).expect("page");
        let count = page.commits.len();
        paged.extend(page.commits.into_iter().map(|commit| commit.id));
        skip += count;
        if page.complete {
            break;
        }
        assert!(count > 0, "incomplete page must make progress");
    }
    assert_eq!(
        paged,
        full.commits
            .iter()
            .map(|commit| commit.id.clone())
            .collect::<Vec<_>>()
    );

    // The final page reports completion.
    let last = backend
        .graph_window(full.commits.len().saturating_sub(1), 5)
        .expect("last page");
    assert!(last.complete);
    assert_eq!(last.commits.len(), 1);

    // Merge histories page consistently too.
    let (merge_temp, _) = merge_repository();
    let merge_backend = backend::open(merge_temp.path()).expect("open merge repository");
    let merge_full = merge_backend.build_graph().expect("build full merge graph");
    let merge_page = merge_backend.graph_window(0, 2).expect("merge page");
    assert_eq!(
        merge_page
            .commits
            .iter()
            .map(|commit| &commit.id)
            .collect::<Vec<_>>(),
        merge_full
            .commits
            .iter()
            .take(2)
            .map(|commit| &commit.id)
            .collect::<Vec<_>>(),
        "pages must follow the complete graph order"
    );
}

/// Perf harness for the blocking diff load (refresh_diff) and the
/// status/diff-stats refresh. Run explicitly:
/// `cargo test --release --test rebase_actions bench_backend_paths -- --ignored --nocapture`
#[test]
#[ignore = "perf measurement; run with --ignored --nocapture"]
fn bench_backend_paths() {
    use std::time::Instant;

    let repository = TempDir::new().expect("repository");
    let path = repository.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);

    let big = (0..20_000)
        .map(|i| format!("line {i}\n"))
        .collect::<String>();
    std::fs::write(path.join("big.txt"), &big).unwrap();
    for i in 0..300 {
        std::fs::write(path.join(format!("f{i}.txt")), "a\nb\nc\n").unwrap();
    }
    git_ok(path, &["add", "-A"]);
    git_ok(path, &["commit", "--quiet", "-m", "base"]);

    let big_changed = (0..20_000)
        .map(|i| {
            if i % 3 == 0 {
                format!("changed {i}\n")
            } else {
                format!("line {i}\n")
            }
        })
        .collect::<String>();
    std::fs::write(path.join("big.txt"), &big_changed).unwrap();
    for i in 0..300 {
        std::fs::write(path.join(format!("f{i}.txt")), "a\nB\nc\nD\n").unwrap();
    }

    let backend = backend::open(path).expect("open");
    let entries = backend.status().expect("status");
    let big_path = GitPath::from("big.txt");

    let started = Instant::now();
    let diff = backend
        .diff(&big_path, DiffTarget::Worktree)
        .expect("diff big.txt");
    let diff_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    let width = backend
        .file_line_number_width(&big_path, None)
        .expect("line width");
    let width_ms = started.elapsed().as_secs_f64() * 1000.0;
    println!(
        "BENCH diff big.txt bytes={} diff={diff_ms:.2}ms line_number_width={width_ms:.2}ms (nw={width})",
        diff.len()
    );

    let small_path = GitPath::from("f0.txt");
    let started = Instant::now();
    let _ = backend.diff(&small_path, DiffTarget::Worktree).unwrap();
    let small_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    let _ = backend.file_line_number_width(&small_path, None).unwrap();
    let small_width_ms = started.elapsed().as_secs_f64() * 1000.0;
    println!(
        "BENCH diff f0.txt (4 lines) diff={small_ms:.2}ms line_number_width={small_width_ms:.2}ms"
    );

    for n in [25usize, 100, 300] {
        let subset = &entries[..n.min(entries.len())];
        let started = Instant::now();
        let stats = backend
            .status_diff_stats(subset)
            .expect("status diff stats");
        let stats_ms = started.elapsed().as_secs_f64() * 1000.0;
        let started = Instant::now();
        let hunks = backend.status_hunk_counts(subset).expect("hunk counts");
        let hunks_ms = started.elapsed().as_secs_f64() * 1000.0;
        println!(
            "BENCH refresh entries={n} status_diff_stats={stats_ms:.2}ms ({} stats) hunk_counts={hunks_ms:.2}ms ({} hunks)",
            stats.len(),
            hunks.len(),
        );
    }
}

#[test]
fn status_hunk_counts_match_per_file_counts() {
    let repository = TempDir::new().expect("repository");
    let path = repository.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);

    let base = (1..=20).map(|i| format!("line {i}\n")).collect::<String>();
    std::fs::write(path.join("a.txt"), &base).unwrap();
    std::fs::write(path.join("b.txt"), "one\ntwo\n").unwrap();
    std::fs::write(path.join("with space.txt"), "alpha\nbeta\n").unwrap();
    git_ok(path, &["add", "-A"]);
    git_ok(path, &["commit", "--quiet", "-m", "base"]);

    // Two separate worktree hunks in a.txt.
    let two_hunks = base
        .replace("line 2\n", "line 2 changed\n")
        .replace("line 18\n", "line 18 changed\n");
    std::fs::write(path.join("a.txt"), two_hunks).unwrap();
    // Staged change in b.txt.
    std::fs::write(path.join("b.txt"), "one\ntwo\nthree\n").unwrap();
    git_ok(path, &["add", "b.txt"]);
    // Worktree change in a path with spaces.
    std::fs::write(path.join("with space.txt"), "alpha\nBETA\n").unwrap();
    // Untracked file.
    std::fs::write(path.join("untracked.txt"), "new\ncontent\n").unwrap();

    let backend = backend::open(path).expect("open");
    let entries = backend.status().expect("status");
    let batched = backend.status_hunk_counts(&entries).expect("batched");
    let batched = batched
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();

    for entry in &entries {
        let single = backend.diff_hunk_counts(&entry.path).expect("single");
        assert_eq!(
            batched.get(&entry.path).copied(),
            Some(single),
            "hunk counts disagree for {} (batched={:?} single={:?})",
            entry.path.display(),
            batched.get(&entry.path),
            single
        );
    }
    assert_eq!(
        batched.get(&GitPath::from("a.txt")).copied(),
        Some((0, 2)),
        "a.txt should report two unstaged hunks"
    );
    assert_eq!(
        batched.get(&GitPath::from("b.txt")).copied(),
        Some((1, 0)),
        "b.txt should report one staged hunk"
    );
}

#[test]
fn push_tag_publishes_the_tag_to_the_remote() {
    let root = TempDir::new().expect("root");
    let remote = root.path().join("remote.git");
    let local = root.path().join("local");
    git_ok(
        root.path(),
        &["init", "--quiet", "--bare", remote.to_str().unwrap()],
    );
    git_ok(
        root.path(),
        &[
            "clone",
            "--quiet",
            remote.to_str().unwrap(),
            local.to_str().unwrap(),
        ],
    );
    configure_user(&local);
    std::fs::write(local.join("f.txt"), "a\n").unwrap();
    git_ok(&local, &["add", "f.txt"]);
    git_ok(&local, &["commit", "--quiet", "-m", "base"]);
    git_ok(&local, &["tag", "v9"]);

    let backend = backend::open(&local).expect("open");
    backend.push_tag("origin", "v9", 0).expect("push tag");
    assert_eq!(
        git_ok(&remote, &["rev-parse", "refs/tags/v9"]),
        git_ok(&local, &["rev-parse", "HEAD"])
    );
}

#[test]
fn delete_tag_removes_it_locally_and_remotely() {
    let root = TempDir::new().expect("root");
    let remote = root.path().join("remote.git");
    let local = root.path().join("local");
    git_ok(
        root.path(),
        &["init", "--quiet", "--bare", remote.to_str().unwrap()],
    );
    git_ok(
        root.path(),
        &[
            "clone",
            "--quiet",
            remote.to_str().unwrap(),
            local.to_str().unwrap(),
        ],
    );
    configure_user(&local);
    std::fs::write(local.join("f.txt"), "a\n").unwrap();
    git_ok(&local, &["add", "f.txt"]);
    git_ok(&local, &["commit", "--quiet", "-m", "base"]);
    git_ok(&local, &["tag", "v9"]);
    let backend = backend::open(&local).expect("open");
    backend.push_tag("origin", "v9", 0).expect("push tag");

    backend.delete_tag("v9", false).expect("delete local tag");
    assert!(
        !git(&local, &["show-ref", "--verify", "--quiet", "refs/tags/v9"])
            .status
            .success()
    );
    assert!(
        git(
            &remote,
            &["show-ref", "--verify", "--quiet", "refs/tags/v9"]
        )
        .status
        .success(),
        "local deletion should leave the remote tag alone"
    );

    git_ok(&local, &["tag", "v9"]);
    backend.push_tag("origin", "v9", 0).expect("push tag again");
    backend.delete_tag("v9", true).expect("delete tag remotely");
    assert!(
        !git(&local, &["show-ref", "--verify", "--quiet", "refs/tags/v9"])
            .status
            .success()
    );
    assert!(!git(
        &remote,
        &["show-ref", "--verify", "--quiet", "refs/tags/v9"]
    )
    .status
    .success());
}

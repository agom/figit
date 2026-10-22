//! Shared helpers for the integration test binaries.
//!
//! This directory module is compiled into each test binary that declares
//! `mod common;`, so it is not treated as a standalone test target.

use std::path::Path;
use std::process::{Command, Output};

pub fn git(path: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("run git")
}

pub fn git_ok(path: &Path, args: &[&str]) -> String {
    let output = git(path, args);
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Configure the identity used by the primary test repository.
pub fn configure_user(path: &Path) {
    git_ok(path, &["config", "user.name", "Figit Test"]);
    git_ok(path, &["config", "user.email", "figit@example.com"]);
}

/// Configure a distinct identity for peer/clone repositories.
pub fn configure_peer(path: &Path) {
    git_ok(path, &["config", "user.name", "Figit Peer"]);
    git_ok(path, &["config", "user.email", "peer@example.com"]);
}

pub fn review_fixture(scenario: &str) -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("review fixture directory");
    let output = Command::new("bash")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/review-regressions.sh"))
        .arg(temp.path())
        .arg(scenario)
        .output()
        .expect("build review fixture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    temp
}

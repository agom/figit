use anyhow::{ensure, Result};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    pub unavailable: bool,
}

pub(crate) fn list(root: &Path) -> Result<Vec<Worktree>> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["worktree", "list", "--porcelain", "-z"])
        .output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let mut trees = Vec::new();
    for field in output.stdout.split(|b| *b == 0) {
        if let Some(path) = field.strip_prefix(b"worktree ") {
            trees.push(Worktree {
                path: PathBuf::from(OsStr::from_bytes(path)),
                branch: None,
                head: String::new(),
                unavailable: false,
            });
        } else if let Some(tree) = trees.last_mut() {
            if let Some(name) = field.strip_prefix(b"branch refs/heads/") {
                tree.branch = Some(String::from_utf8_lossy(name).into_owned());
            } else if let Some(head) = field.strip_prefix(b"HEAD ") {
                tree.head = String::from_utf8_lossy(head).chars().take(8).collect();
            } else if field == b"bare" || field.starts_with(b"prunable") {
                tree.unavailable = true;
            }
        }
    }
    Ok(trees)
}

pub(crate) enum Source {
    Local(String),
    Remote(String),
    Commit(String),
}

impl Source {
    pub fn name(&self) -> &str {
        match self {
            Self::Local(s) | Self::Remote(s) | Self::Commit(s) => s,
        }
    }
}

pub(crate) fn create(root: &Path, source: &Source, branch: &str, path: &Path) -> Result<()> {
    let mut git = Command::new("git");
    git.current_dir(root).args(["worktree", "add"]);
    if matches!(source, Source::Local(_)) {
        git.arg("--no-guess-remote");
    } else if branch.is_empty() {
        git.arg("--detach");
    } else {
        if matches!(source, Source::Remote(_)) {
            git.arg("--track");
        }
        git.args(["-b", branch]);
    }
    let start = match source {
        Source::Remote(remote) => format!("refs/remotes/{remote}"),
        _ => source.name().to_owned(),
    };
    let output = git.arg("--").arg(path).arg(start).output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

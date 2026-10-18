use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context, Result};
use similar::{DiffTag, TextDiff};

use crate::backend::{
    ConflictSide, ConflictVersions, DiffLineSelection, DiffTarget, GraphWindow, PathDiffStats,
    RebaseAction, RebaseOutcome, RebasePlan, RebasePlanBlock, RebasePlanEntry, RebaseTodoAction,
    UndoInfo,
};
use crate::core::model::{
    BranchTracking, Commit, GitPath, GraphModel, Oid, RefKind, RefLabel, StatusEntry,
};

#[path = "gix/helpers.rs"]
mod helpers;
use helpers::*;

/// Git's documented fallback for `core.bigFileThreshold`.
const GIT_DEFAULT_BIG_FILE_THRESHOLD: u64 = 512 * 1024 * 1024;
/// Even a small fraction of a very large file is unreadable in a terminal, so
/// diffs are skipped at this size regardless of the configured threshold.
const TUI_DIFF_SIZE_LIMIT: u64 = 8 * 1024 * 1024;

/// Blob sizes gathered in one batch, used to decide which entries are too
/// large to diff.
struct BlobSizes {
    index: HashMap<GitPath, u64>,
    head: HashMap<GitPath, u64>,
    worktree_large: HashSet<GitPath>,
}

/// Count `@@` hunk headers per file in a unified diff, keyed by path bytes.
/// Deletions key off the `--- a/` line; everything else off `+++ b/`.
fn parse_hunk_counts(diff: &[u8]) -> HashMap<Vec<u8>, usize> {
    let mut counts: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut current: Option<Vec<u8>> = None;
    // Git appends a tab after the path on `---`/`+++` lines when the name
    // contains a space, so strip it before matching against the real path.
    let path_after = |rest: &[u8]| rest.strip_suffix(b"\t").unwrap_or(rest).to_vec();
    for line in diff.split(|&byte| byte == b'\n') {
        if line.starts_with(b"diff --git ") {
            current = None;
        } else if let Some(path) = line.strip_prefix(b"+++ b/") {
            current = Some(path_after(path));
        } else if let Some(path) = line.strip_prefix(b"--- a/") {
            if current.is_none() {
                current = Some(path_after(path));
            }
        } else if line.starts_with(b"@@") {
            if let Some(path) = &current {
                *counts.entry(path.clone()).or_insert(0) += 1;
            }
        }
    }
    counts
}

pub struct GixBackend {
    repo: gix::Repository,
    workdir: PathBuf,
    big_file_limit: std::sync::OnceLock<u64>,
}

impl GixBackend {
    pub fn open(path: &Path) -> Result<Self> {
        let repo = gix::discover(path)
            .with_context(|| format!("not a git repository: {}", path.display()))?;
        let workdir = repo
            .workdir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf());
        Ok(Self {
            repo,
            workdir,
            big_file_limit: std::sync::OnceLock::new(),
        })
    }

    /// Resolve the remote to use for fetch/push given the branch whose config
    /// should win, mirroring Git's precedence: `branch.<name>.pushRemote`,
    /// `remote.pushDefault`, then `branch.<name>.remote`, then `origin` or the
    /// first configured remote.
    fn resolve_remote(&self, branch: Option<&str>, for_push: bool) -> Result<Option<String>> {
        let remotes = super::GitBackend::remotes(self)?;
        if remotes.is_empty() {
            return Ok(None);
        }
        let mut config_keys = Vec::new();
        if let Some(branch) = branch {
            if for_push {
                config_keys.push(format!("branch.{branch}.pushRemote"));
                config_keys.push("remote.pushDefault".to_string());
            }
            config_keys.push(format!("branch.{branch}.remote"));
        } else if for_push {
            config_keys.push("remote.pushDefault".to_string());
        }
        for key in config_keys {
            let output = git_output(&self.workdir, &["config", "--get", &key])?;
            if output.status.success() {
                let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if remotes.contains(&remote) {
                    return Ok(Some(remote));
                }
            }
        }
        Ok(remotes
            .iter()
            .find(|remote| remote.as_str() == "origin")
            .or_else(|| remotes.first())
            .cloned())
    }

    fn head(&self) -> Result<(Option<Oid>, Option<String>)> {
        match self.repo.head() {
            Ok(head) => {
                let id = head.id().map(|id| id.to_string());
                let name = head.referent_name().map(|n| short_ref_name(n.as_bstr()));
                Ok((id, name))
            }
            Err(_) => Ok((None, None)), // unborn HEAD (no commits yet)
        }
    }

    fn collect_refs(&self) -> Result<Vec<RefLabel>> {
        let mut refs = Vec::new();
        let platform = self.repo.references().context("list references")?;
        for reference in platform.all().context("iterate references")? {
            let mut reference = reference.map_err(|e| anyhow::anyhow!("invalid reference: {e}"))?;

            let full = reference.name().as_bstr().to_vec();
            let kind = if full.starts_with(b"refs/heads/") {
                Some(RefKind::LocalBranch)
            } else if full.starts_with(b"refs/remotes/") {
                Some(RefKind::RemoteBranch)
            } else if full.starts_with(b"refs/tags/") {
                Some(RefKind::Tag)
            } else {
                None
            };
            let Some(kind) = kind else { continue };

            // Peel tags to the commit they ultimately point at.
            let target = reference.peel_to_id().context("peel ref")?;
            if self.repo.find_commit(target).is_err() {
                continue; // not a commit (e.g. tag on a blob)
            }
            refs.push(RefLabel {
                kind,
                name: short_ref_name(reference.name().as_bstr()),
                id: target.to_string(),
            });
        }
        Ok(refs)
    }

    fn collect_branch_tracking(&self) -> Result<Vec<BranchTracking>> {
        let output = git_output_checked(
            &self.workdir,
            &[
                "for-each-ref",
                "--format=%(refname)%09%(upstream)%09%(upstream:track)",
                "refs/heads",
            ],
        )?;
        let refs = String::from_utf8_lossy(&output.stdout);
        let mut tracking = Vec::new();
        for line in refs.lines() {
            let mut fields = line.splitn(3, '\t');
            let (Some(branch), Some(upstream), Some(track)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if upstream.is_empty() {
                continue;
            }
            let track = track
                .trim()
                .strip_prefix('[')
                .and_then(|track| track.strip_suffix(']'))
                .unwrap_or_default();
            if track == "gone" {
                continue;
            }
            let mut ahead = 0;
            let mut behind = 0;
            for direction in track.split(", ") {
                if let Some(count) = direction.strip_prefix("ahead ") {
                    ahead = count.parse().unwrap_or(0);
                } else if let Some(count) = direction.strip_prefix("behind ") {
                    behind = count.parse().unwrap_or(0);
                }
            }
            tracking.push(BranchTracking {
                branch: branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch)
                    .to_string(),
                upstream: upstream
                    .strip_prefix("refs/remotes/")
                    .unwrap_or(upstream)
                    .to_string(),
                ahead,
                behind,
            });
        }
        Ok(tracking)
    }

    fn branch_upstream_remote(&self, branch: &str) -> Result<String> {
        let refname = format!("refs/heads/{branch}");
        let output = git_output_checked(
            &self.workdir,
            &["for-each-ref", "--format=%(upstream:remotename)", &refname],
        )?;
        let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if remote.is_empty() || remote == "." {
            anyhow::bail!("branch '{branch}' has no remote upstream");
        }
        Ok(remote)
    }

    fn commit_parents(&self, target: &str) -> Result<Vec<String>> {
        let id = parse_oid(target)?;
        let commit = self
            .repo
            .find_commit(id)
            .with_context(|| format!("find commit {target}"))?;
        Ok(commit
            .parent_ids()
            .map(|parent| parent.to_string())
            .collect())
    }

    /// Whether `path` is untracked (present in the worktree, absent from HEAD).
    fn is_untracked(&self, path: &GitPath) -> Result<bool> {
        let output = git_output_with_path_checked(
            &self.workdir,
            &["ls-files", "-o", "--exclude-standard", "-z", "--"],
            path,
        )?;
        Ok(!output.stdout.is_empty())
    }

    /// `git diff` reports nothing for an untracked path, so synthesize an
    /// all-added diff against `/dev/null`. `--no-index` exits 1 when the files
    /// differ, which is the expected case here.
    fn untracked_diff(&self, path: &GitPath) -> Result<String> {
        let output = git_output_with_path(
            &self.workdir,
            &[
                "diff",
                "--no-index",
                "--no-color",
                "--unified=3",
                "--",
                "/dev/null",
            ],
            path,
        )?;
        if !output.status.success() && output.status.code() != Some(1) {
            anyhow::bail!(
                "git diff --no-index failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Effective per-file diff limit: the smaller of `core.bigFileThreshold`
    /// (git's own default is 512 MiB) and the size a terminal can usefully
    /// render. Files above it are not diffed at all. Resolved once per
    /// repository because it costs a `git config` invocation.
    fn diff_size_limit(&self) -> u64 {
        *self.big_file_limit.get_or_init(|| {
            let configured =
                git_output(&self.workdir, &["config", "--get", "core.bigFileThreshold"])
                    .ok()
                    .filter(|output| output.status.success())
                    .and_then(|output| {
                        parse_git_size(String::from_utf8_lossy(&output.stdout).trim())
                    });
            configured
                .unwrap_or(GIT_DEFAULT_BIG_FILE_THRESHOLD)
                .min(TUI_DIFF_SIZE_LIMIT)
        })
    }

    fn blob_size(&self, revision: &str, path: &GitPath) -> Option<u64> {
        let spec = revision_path(revision, path).ok()?;
        let output = Command::new("git")
            .args(["cat-file", "-s"])
            .arg(spec)
            .current_dir(&self.workdir)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8_lossy(&output.stdout).trim().parse().ok()
    }

    /// Sizes for a list of `revision:path` specs in a single
    /// `git cat-file --batch-check`. Positional: `None` marks a missing object.
    fn batch_blob_sizes(&self, specs: &[OsString]) -> Vec<Option<u64>> {
        if specs.is_empty() {
            return Vec::new();
        }
        let mut sizes = vec![None; specs.len()];
        let mut child = match Command::new("git")
            .args(["cat-file", "--batch-check"])
            .current_dir(&self.workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => return sizes,
        };
        if let Some(stdin) = child.stdin.as_mut() {
            for spec in specs {
                let _ = stdin.write_all(spec.as_encoded_bytes());
                let _ = stdin.write_all(b"\n");
            }
        }
        drop(child.stdin.take());
        let Ok(output) = child.wait_with_output() else {
            return sizes;
        };
        for (index, line) in String::from_utf8_lossy(&output.stdout)
            .lines()
            .take(sizes.len())
            .enumerate()
        {
            // `<object> <type> <size>` for hits, `<input> missing` for misses.
            sizes[index] = line.rsplit(' ').next().and_then(|field| field.parse().ok());
        }
        sizes
    }

    /// Blob sizes needed to decide which entries are too large to diff, using
    /// one `cat-file` call for the index blobs and one for the HEAD blobs.
    fn blob_sizes_for(&self, entries: &[StatusEntry]) -> BlobSizes {
        let mut index_specs = Vec::new();
        let mut index_paths = Vec::new();
        let mut head_specs = Vec::new();
        let mut head_paths = Vec::new();
        let mut worktree_large = HashSet::new();
        for entry in entries {
            if entry.is_conflict() || entry.index == '?' {
                continue;
            }
            if let Ok(spec) = revision_path(":", &entry.path) {
                index_specs.push(spec);
                index_paths.push(entry.path.clone());
            }
            if let Ok(spec) = revision_path("HEAD:", &entry.path) {
                head_specs.push(spec);
                head_paths.push(entry.path.clone());
            }
            if self.worktree_is_too_large(&entry.path) {
                worktree_large.insert(entry.path.clone());
            }
        }
        let pair = |paths: Vec<GitPath>, sizes: Vec<Option<u64>>| {
            paths
                .into_iter()
                .zip(sizes)
                .filter_map(|(path, size)| size.map(|size| (path, size)))
                .collect::<HashMap<_, _>>()
        };
        BlobSizes {
            index: pair(index_paths, self.batch_blob_sizes(&index_specs)),
            head: pair(head_paths, self.batch_blob_sizes(&head_specs)),
            worktree_large,
        }
    }

    /// `@@` counts per path for one side of the diff, in a single `git diff`.
    fn batch_hunk_counts(
        &self,
        paths: &[GitPath],
        staged: bool,
    ) -> Result<HashMap<Vec<u8>, usize>> {
        if paths.is_empty() {
            return Ok(HashMap::new());
        }
        let mut command = Command::new("git");
        command.args([
            "-c",
            "core.quotePath=false",
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--no-renames",
            "--unified=3",
        ]);
        if staged {
            command.arg("--cached");
        }
        command.arg("--");
        for path in paths {
            command.arg(path.to_os_string()?);
        }
        let output = command
            .current_dir(&self.workdir)
            .output()
            .with_context(|| "run git diff")?;
        if !output.status.success() {
            anyhow::bail!(
                "git diff failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(parse_hunk_counts(&output.stdout))
    }

    /// Cheap check (one `stat`, no subprocess) for the common case of a huge
    /// tracked file edited in the worktree.
    fn worktree_is_too_large(&self, path: &GitPath) -> bool {
        path.to_path_buf().is_ok_and(|path| {
            fs::metadata(self.workdir.join(path))
                .is_ok_and(|metadata| metadata.len() > self.diff_size_limit())
        })
    }

    fn index_blob_is_too_large(&self, path: &GitPath) -> bool {
        self.blob_size(":", path)
            .is_some_and(|size| size > self.diff_size_limit())
    }

    fn head_blob_is_too_large(&self, path: &GitPath) -> bool {
        self.blob_size("HEAD:", path)
            .is_some_and(|size| size > self.diff_size_limit())
    }

    fn diff_is_too_large(&self, path: &GitPath, target: DiffTarget) -> bool {
        match target {
            DiffTarget::Worktree => {
                self.worktree_is_too_large(path) || self.index_blob_is_too_large(path)
            }
            DiffTarget::Index => {
                self.index_blob_is_too_large(path) || self.head_blob_is_too_large(path)
            }
        }
    }

    fn conflict_is_too_large(&self, path: &GitPath) -> bool {
        self.worktree_is_too_large(path)
            || [":1:", ":2:", ":3:"].into_iter().any(|stage| {
                self.blob_size(stage, path)
                    .is_some_and(|size| size > self.diff_size_limit())
            })
    }

    fn numstat_for_paths(
        &self,
        paths: &[GitPath],
        staged: bool,
    ) -> Result<HashMap<GitPath, (usize, usize)>> {
        if paths.is_empty() {
            return Ok(HashMap::new());
        }
        let mut command = Command::new("git");
        command.args([
            "--literal-pathspecs",
            "diff",
            "--numstat",
            "-z",
            "--no-renames",
        ]);
        if staged {
            command.arg("--cached");
        }
        command.arg("--");
        for path in paths {
            command.arg(path.to_os_string()?);
        }
        let output = command
            .current_dir(&self.workdir)
            .output()
            .context("run batched git diff --numstat")?;
        if !output.status.success() {
            anyhow::bail!(
                "git diff --numstat failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        let mut result = HashMap::with_capacity(paths.len());
        for record in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|r| !r.is_empty())
        {
            let mut fields = record.splitn(3, |byte| *byte == b'\t');
            let (Some(added), Some(removed), Some(path)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let added = std::str::from_utf8(added)
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            let removed = std::str::from_utf8(removed)
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            result.insert(GitPath::from_bytes(path.to_vec()), (added, removed));
        }
        Ok(result)
    }

    fn untracked_diff_stats(&self, path: &GitPath) -> Result<(usize, usize)> {
        if self.worktree_is_too_large(path) {
            return Ok((0, 0));
        }
        let output = git_output_with_path(
            &self.workdir,
            &["diff", "--no-index", "--numstat", "--", "/dev/null"],
            path,
        )?;
        if !output.status.success() && output.status.code() != Some(1) {
            anyhow::bail!(
                "git diff --no-index --numstat failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let first = stdout.lines().next().unwrap_or("");
        let mut fields = first.split('\t');
        let added = fields.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let removed = fields.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        Ok((added, removed))
    }

    fn commit_path_is_too_large(&self, commit: &str, path: &GitPath, parents: &[String]) -> bool {
        let previous = parents.first();
        std::iter::once(commit)
            .chain(previous.map(String::as_str))
            .any(|revision| {
                self.blob_size(&format!("{revision}:"), path)
                    .is_some_and(|size| size > self.diff_size_limit())
            })
    }

    fn commit(&self, id: &gix::ObjectId) -> Result<Commit> {
        load_commit(&self.repo, id)
    }

    /// Decode commit ids that are already in graph order, preserving that order.
    fn decode_ordered_commits(&self, ids: &[String]) -> Result<Vec<Commit>> {
        let parsed = ids
            .iter()
            .map(|id| parse_oid(id))
            .collect::<Result<Vec<_>>>()?;
        self.decode_commits(&parsed)
    }

    /// Commit ids in the graph's stable date/topological order. The id list is
    /// small compared with decoded commits and is reused for every history page.
    fn ordered_commit_ids(&self) -> Result<Vec<String>> {
        let mut args = vec!["log", "--all"];
        // An unborn symbolic HEAD is not a valid revision argument to `git
        // log`; `--all` is sufficient when there is no commit checked out.
        if self.head()?.0.is_some() {
            args.push("HEAD");
        }
        args.extend(["--date-order", "--format=%H"]);
        let output = git_output_checked(&self.workdir, &args)?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .collect())
    }

    fn decode_commits(&self, ids: &[gix::ObjectId]) -> Result<Vec<Commit>> {
        const MAX_DECODE_WORKERS: usize = 8;
        const PARALLEL_THRESHOLD: usize = 512;
        if ids.len() < PARALLEL_THRESHOLD {
            return ids.iter().map(|id| self.commit(id)).collect();
        }

        let workers = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .min(MAX_DECODE_WORKERS)
            .min(ids.len());
        if workers < 2 {
            return ids.iter().map(|id| self.commit(id)).collect();
        }
        let chunk_size = ids.len().div_ceil(workers);
        std::thread::scope(|scope| {
            let handles = ids
                .chunks(chunk_size)
                .map(|chunk| {
                    let repo = self.repo.clone();
                    scope.spawn(move || {
                        chunk
                            .iter()
                            .map(|id| load_commit(&repo, id))
                            .collect::<Result<Vec<_>>>()
                    })
                })
                .collect::<Vec<_>>();
            let mut commits = Vec::with_capacity(ids.len());
            for handle in handles {
                commits.extend(
                    handle
                        .join()
                        .map_err(|_| anyhow::anyhow!("commit decode worker panicked"))??,
                );
            }
            Ok(commits)
        })
    }

    fn rebase_stop_info(&self, fallback_target: &str) -> Result<RebaseOutcome> {
        let stopped =
            git_output_checked(&self.workdir, &["rev-list", "--parents", "-n", "1", "HEAD"])?;
        let stopped = String::from_utf8_lossy(&stopped.stdout);
        let fields: Vec<&str> = stopped.split_whitespace().collect();
        let parents: Vec<String> = fields
            .iter()
            .skip(1)
            .map(|parent| (*parent).into())
            .collect();
        let Some(first_parent) = parents.first() else {
            let _ = run_git(&self.workdir, ["rebase", "--abort"]);
            anyhow::bail!("editing a root commit is not supported yet");
        };
        let source = git_output(&self.workdir, &["rev-parse", "--verify", "REBASE_HEAD"])?;
        let target = if source.status.success() {
            String::from_utf8_lossy(&source.stdout).trim().to_string()
        } else {
            fallback_target.to_string()
        };
        let (title, description) = read_commit_message(&self.workdir, "HEAD")?;
        if let Err(error) = run_git(&self.workdir, ["reset", "--soft", first_parent]) {
            let _ = run_git(&self.workdir, ["rebase", "--abort"]);
            return Err(error).context("stage the selected commit for editing");
        }
        Ok(RebaseOutcome::StoppedAtCommit {
            target,
            parents,
            title,
            description,
        })
    }

    fn conflict_diff(&self, path: &GitPath) -> Result<String> {
        let stage_content = |stage: u8| -> Result<Option<Vec<u8>>> {
            let spec = revision_path(&format!(":{stage}:"), path)?;
            let output = git_output_with_os_arg(&self.workdir, &["show"], spec)?;
            Ok(output.status.success().then_some(output.stdout))
        };
        let base = stage_content(1)?.unwrap_or_default();
        let ours = stage_content(2)?;
        let theirs = stage_content(3)?;
        let temp = tempfile::tempdir().context("create conflict diff directory")?;
        let base_path = temp.path().join("base");
        fs::write(&base_path, base).context("write base conflict stage")?;
        let mut result =
            format!("Conflict stages for {path} (ours = current base; theirs = incoming commit)\n");
        for (label, content) in [
            ("ours (current base)", ours.as_deref()),
            ("theirs (incoming commit)", theirs.as_deref()),
        ] {
            let Some(content) = content else {
                result.push_str(&format!("\n{label}: deleted\n"));
                continue;
            };
            let side_path = temp
                .path()
                .join(label.split_whitespace().next().unwrap_or("side"));
            fs::write(&side_path, content)
                .with_context(|| format!("write {label} conflict stage"))?;
            result.push_str(&format!("\nDiff: base → {label}\n"));
            result.push_str(&diff_file_pair(&self.workdir, &base_path, &side_path)?);
        }
        if let (Some(ours), Some(theirs)) = (ours.as_deref(), theirs.as_deref()) {
            let ours_path = temp.path().join("ours-vs-theirs-ours");
            let theirs_path = temp.path().join("ours-vs-theirs-theirs");
            fs::write(&ours_path, ours).context("write ours conflict stage")?;
            fs::write(&theirs_path, theirs).context("write theirs conflict stage")?;
            result.push_str("\nDiff: ours → theirs\n");
            result.push_str(&diff_file_pair(&self.workdir, &ours_path, &theirs_path)?);
        }
        if let Ok(path_buf) = path.to_path_buf() {
            if let Ok(content) = fs::read(self.workdir.join(path_buf)) {
                result.push_str(&format!(
                    "\nWorking file with conflict markers:\n{}",
                    String::from_utf8_lossy(&content)
                ));
            }
        }
        Ok(result)
    }

    fn validate_rebase_start(&self, target: &str) -> Result<()> {
        if rebase_in_progress(&self.workdir)? {
            anyhow::bail!("a Git rebase is already in progress");
        }
        let status = git_output_checked(&self.workdir, &["status", "--porcelain=v1", "-z"])?;
        if !status.stdout.is_empty() {
            anyhow::bail!("commit or stash working tree changes before rewriting history");
        }
        let branch = git_output(&self.workdir, &["symbolic-ref", "--quiet", "HEAD"])?;
        if !branch.status.success() {
            anyhow::bail!("interactive rebase requires a checked-out branch");
        }
        let ancestor = git_output(
            &self.workdir,
            &["merge-base", "--is-ancestor", target, "HEAD"],
        )?;
        if !ancestor.status.success() {
            anyhow::bail!("selected commit is not reachable from the checked-out branch");
        }
        Ok(())
    }
}

impl super::GitBackend for GixBackend {
    fn working_directory(&self) -> &Path {
        &self.workdir
    }

    fn watch_paths(&self) -> Result<Vec<PathBuf>> {
        let git_dir = git_output_checked(&self.workdir, &["rev-parse", "--absolute-git-dir"])?;
        let git_dir = PathBuf::from(String::from_utf8_lossy(&git_dir.stdout).trim());
        let mut paths = vec![self.workdir.clone(), git_dir];
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    fn build_graph(&self) -> Result<GraphModel> {
        let profile = std::env::var_os("FIGIT_PROFILE_STARTUP").is_some();
        let total_started = Instant::now();
        let phase_started = Instant::now();
        let refs = self.collect_refs()?;
        if profile {
            eprintln!(
                "profile collect_refs: {:.3}s ({} refs)",
                phase_started.elapsed().as_secs_f64(),
                refs.len()
            );
        }
        let phase_started = Instant::now();
        let branch_tracking = self.collect_branch_tracking()?;
        if profile {
            eprintln!(
                "profile branch_tracking: {:.3}s ({} branches)",
                phase_started.elapsed().as_secs_f64(),
                branch_tracking.len()
            );
        }
        let (head_id, head_name) = self.head()?;

        let phase_started = Instant::now();
        let ids = self.ordered_commit_ids()?;
        if profile {
            eprintln!(
                "profile git log ids: {:.3}s ({} commits)",
                phase_started.elapsed().as_secs_f64(),
                ids.len()
            );
        }
        let phase_started = Instant::now();
        let commits = self.decode_ordered_commits(&ids)?;
        if profile {
            eprintln!(
                "profile commit decode: {:.3}s ({} commits)",
                phase_started.elapsed().as_secs_f64(),
                commits.len()
            );
            eprintln!(
                "profile build_graph total: {:.3}s",
                total_started.elapsed().as_secs_f64()
            );
        }

        Ok(GraphModel {
            commits,
            refs,
            branch_tracking,
            head_id,
            head_name,
        })
    }

    fn ordered_history_ids(&self) -> Result<Vec<String>> {
        self.ordered_commit_ids()
    }

    fn graph_page(&self, ids: &[String], skip: usize, limit: usize) -> Result<GraphWindow> {
        let start = skip.min(ids.len());
        let end = start.saturating_add(limit).min(ids.len());
        let complete = end == ids.len();
        let commits = self.decode_ordered_commits(&ids[start..end])?;
        let (refs, branch_tracking, head_id, head_name) = if skip == 0 {
            let (head_id, head_name) = self.head()?;
            (
                self.collect_refs()?,
                self.collect_branch_tracking()?,
                head_id,
                head_name,
            )
        } else {
            (Vec::new(), Vec::new(), None, None)
        };
        Ok(GraphWindow {
            commits,
            refs,
            branch_tracking,
            head_id,
            head_name,
            complete,
        })
    }

    fn graph_window(&self, skip: usize, limit: usize) -> Result<GraphWindow> {
        let ids = self.ordered_commit_ids()?;
        self.graph_page(&ids, skip, limit)
    }

    fn status(&self) -> Result<Vec<StatusEntry>> {
        let output = Command::new("git")
            .args(["status", "--porcelain=v1", "-z"])
            .current_dir(&self.workdir)
            .output()
            .with_context(|| "run git status")?;
        if !output.status.success() {
            anyhow::bail!(
                "git status failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        parse_status(&output.stdout)
    }

    fn diff(&self, path: &GitPath, target: DiffTarget) -> Result<String> {
        let unmerged =
            git_output_with_path_checked(&self.workdir, &["ls-files", "-u", "-z", "--"], path)?;
        if !unmerged.stdout.is_empty() {
            if self.conflict_is_too_large(path) {
                return Ok(too_large_diff_notice(path, self.diff_size_limit()));
            }
            let combined = git_output_with_path_checked(
                &self.workdir,
                &["diff", "--cc", "--no-ext-diff", "--"],
                path,
            )?;
            let mut conflict_diff = self.conflict_diff(path)?;
            let combined = String::from_utf8_lossy(&combined.stdout);
            if !combined.trim().is_empty()
                && !combined
                    .lines()
                    .all(|line| line.starts_with("* Unmerged path"))
            {
                conflict_diff.push_str("\nCombined conflict diff:\n");
                conflict_diff.push_str(&combined);
            }
            return Ok(conflict_diff);
        }
        if target == DiffTarget::Worktree && self.is_untracked(path)? {
            if self.worktree_is_too_large(path) {
                return Ok(too_large_diff_notice(path, self.diff_size_limit()));
            }
            return self.untracked_diff(path);
        }
        if self.diff_is_too_large(path, target) {
            return Ok(too_large_diff_notice(path, self.diff_size_limit()));
        }
        let mut args = vec!["diff", "--no-ext-diff", "--no-color", "--unified=3"];
        if target == DiffTarget::Index {
            args.push("--cached");
        }
        args.push("--");
        let output =
            git_output_with_path(&self.workdir, &args, path).with_context(|| "run git diff")?;
        if !output.status.success() {
            anyhow::bail!(
                "git diff failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn diff_hunk_counts(&self, path: &GitPath) -> Result<(usize, usize)> {
        let unmerged =
            git_output_with_path_checked(&self.workdir, &["ls-files", "-u", "-z", "--"], path)?;
        if !unmerged.stdout.is_empty() {
            return Ok((0, 0));
        }
        if self.is_untracked(path)? {
            if self.worktree_is_too_large(path) {
                return Ok((0, 0));
            }
            let hunks = self
                .untracked_diff(path)?
                .lines()
                .filter(|line| line.starts_with("@@"))
                .count();
            return Ok((0, hunks));
        }
        if self.diff_is_too_large(path, DiffTarget::Index)
            || self.diff_is_too_large(path, DiffTarget::Worktree)
        {
            return Ok((0, 0));
        }
        let count_hunks = |target: DiffTarget| -> Result<usize> {
            let mut args = vec!["diff", "--no-ext-diff", "--no-color", "--unified=3"];
            if target == DiffTarget::Index {
                args.push("--cached");
            }
            args.push("--");
            let output = git_output_with_path_checked(&self.workdir, &args, path)?;
            Ok(String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| line.starts_with("@@"))
                .count())
        };
        Ok((
            count_hunks(DiffTarget::Index)?,
            count_hunks(DiffTarget::Worktree)?,
        ))
    }

    fn status_hunk_counts(
        &self,
        entries: &[StatusEntry],
    ) -> Result<Vec<(GitPath, (usize, usize))>> {
        let limit = self.diff_size_limit();
        let sizes = self.blob_sizes_for(entries);
        let index_large = |path: &GitPath| {
            sizes.index.get(path).is_some_and(|size| *size > limit)
                || sizes.head.get(path).is_some_and(|size| *size > limit)
        };
        let worktree_large =
            |path: &GitPath| sizes.worktree_large.contains(path) || index_large(path);

        let eligible = entries
            .iter()
            .filter(|entry| !entry.is_conflict() && entry.index != '?')
            .filter(|entry| !index_large(&entry.path) && !worktree_large(&entry.path))
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        let staged = self.batch_hunk_counts(&eligible, true)?;
        let unstaged = self.batch_hunk_counts(&eligible, false)?;

        let mut counts = Vec::with_capacity(entries.len());
        for entry in entries {
            let value = if entry.is_conflict() {
                (0, 0)
            } else if entry.index == '?' {
                if worktree_large(&entry.path) {
                    (0, 0)
                } else {
                    let hunks = self
                        .untracked_diff(&entry.path)?
                        .lines()
                        .filter(|line| line.starts_with("@@"))
                        .count();
                    (0, hunks)
                }
            } else if index_large(&entry.path) || worktree_large(&entry.path) {
                (0, 0)
            } else {
                (
                    staged.get(entry.path.as_bytes()).copied().unwrap_or(0),
                    unstaged.get(entry.path.as_bytes()).copied().unwrap_or(0),
                )
            };
            counts.push((entry.path.clone(), value));
        }
        Ok(counts)
    }

    fn apply_hunk(&self, path: &GitPath, target: DiffTarget, hunk_index: usize) -> Result<()> {
        let diff = self.diff(path, target)?;
        let patch = extract_diff_hunk(&diff, hunk_index)?;
        apply_cached_patch(&self.workdir, &patch, target == DiffTarget::Index)
    }

    fn apply_diff_line(
        &self,
        path: &GitPath,
        target: DiffTarget,
        selected_line: &DiffLineSelection,
    ) -> Result<()> {
        let stage =
            git_output_with_path_checked(&self.workdir, &["ls-files", "-s", "-z", "--"], path)?;
        let entry = String::from_utf8_lossy(&stage.stdout);
        let metadata = entry
            .split('\t')
            .next()
            .context("read index entry for line operation")?;
        let mode = metadata.split_whitespace().next().unwrap_or_default();
        if !matches!(mode, "100644" | "100755") {
            anyhow::bail!("line staging supports regular text files only; use file or hunk mode");
        }

        let index_spec = revision_path(":", path)?;
        let index_blob = git_output_with_os_arg(&self.workdir, &["show"], index_spec)?;
        let index_text = String::from_utf8(index_blob.stdout)
            .map_err(|_| anyhow::anyhow!("line staging supports UTF-8 text files only"))?;
        let mut index_lines = split_inclusive_lines(&index_text);
        let line_content = selected_line
            .text
            .strip_prefix(if selected_line.old_line.is_some() {
                '-'
            } else {
                '+'
            })
            .context("selected diff line is missing its +/- marker")?;

        match (target, selected_line.old_line, selected_line.new_line) {
            (DiffTarget::Worktree, Some(old_line), None) => {
                let index = usize::try_from(old_line.saturating_sub(1))?;
                let existing = index_lines
                    .get(index)
                    .context("selected deletion is outside the index file")?;
                if diff_line_content(existing) != line_content {
                    anyhow::bail!("selected line is stale; refresh the diff and try again");
                }
                index_lines.remove(index);
            }
            (DiffTarget::Worktree, None, Some(new_line)) => {
                let worktree = fs::read(self.workdir.join(path.to_path_buf()?))
                    .with_context(|| format!("read working file {path}"))?;
                let filter_path = prefixed_path_arg("--path=", path)?;
                let oid = git_output_with_input_os_arg(
                    &self.workdir,
                    &["hash-object", "-w"],
                    filter_path,
                    &["--stdin"],
                    &worktree,
                )?;
                if !oid.status.success() {
                    anyhow::bail!(
                        "could not clean working file for line staging: {}",
                        String::from_utf8_lossy(&oid.stderr).trim()
                    );
                }
                let oid = String::from_utf8_lossy(&oid.stdout).trim().to_string();
                let cleaned = git_output_checked(&self.workdir, &["cat-file", "blob", &oid])?;
                let target_text = String::from_utf8(cleaned.stdout)
                    .map_err(|_| anyhow::anyhow!("line staging supports UTF-8 text files only"))?;
                let target_lines = split_inclusive_lines(&target_text);
                let target_index = usize::try_from(new_line.saturating_sub(1))?;
                let selected = target_lines
                    .get(target_index)
                    .context("selected addition is outside the working file")?;
                if diff_line_content(selected) != line_content {
                    anyhow::bail!("selected line is stale; refresh the diff and try again");
                }
                let insertion =
                    index_insertion_for_target_line(&index_text, &target_text, target_index)?;
                index_lines.insert(insertion, selected.clone());
            }
            (DiffTarget::Index, Some(old_line), None) => {
                let head_spec = revision_path("HEAD:", path)?;
                let head_blob = git_output_with_os_arg(&self.workdir, &["show"], head_spec)?;
                let head_text = String::from_utf8(head_blob.stdout)
                    .map_err(|_| anyhow::anyhow!("line staging supports UTF-8 text files only"))?;
                let head_lines = split_inclusive_lines(&head_text);
                let head_index = usize::try_from(old_line.saturating_sub(1))?;
                let selected = head_lines
                    .get(head_index)
                    .context("selected deletion is outside HEAD file")?;
                if diff_line_content(selected) != line_content {
                    anyhow::bail!("selected line is stale; refresh the diff and try again");
                }
                let insertion =
                    index_insertion_for_source_line(&head_text, &index_text, head_index)?;
                index_lines.insert(insertion, selected.clone());
            }
            (DiffTarget::Index, None, Some(new_line)) => {
                let index = usize::try_from(new_line.saturating_sub(1))?;
                let existing = index_lines
                    .get(index)
                    .context("selected staged addition is outside the index file")?;
                if diff_line_content(existing) != line_content {
                    anyhow::bail!("selected line is stale; refresh the diff and try again");
                }
                index_lines.remove(index);
            }
            _ => anyhow::bail!("selected diff row is not an added or removed line"),
        }

        let updated = index_lines.concat();
        let oid = git_output_with_input(
            &self.workdir,
            &["hash-object", "-w", "--stdin"],
            updated.as_bytes(),
        )?;
        if !oid.status.success() {
            anyhow::bail!(
                "could not write staged line blob: {}",
                String::from_utf8_lossy(&oid.stderr).trim()
            );
        }
        let oid = String::from_utf8_lossy(&oid.stdout).trim().to_string();
        let mut index_info = format!("{mode} {oid} 0\t").into_bytes();
        index_info.extend_from_slice(path.as_bytes());
        index_info.push(0);
        let output = git_output_with_input(
            &self.workdir,
            &["update-index", "-z", "--index-info"],
            &index_info,
        )?;
        if !output.status.success() {
            anyhow::bail!(
                "could not update index for {path}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    fn conflict_commits(&self) -> Result<Option<super::ConflictCommits>> {
        let ours = git_output_checked(&self.workdir, &["rev-parse", "--verify", "HEAD"])?;
        let ours = String::from_utf8_lossy(&ours.stdout).trim().to_string();
        for incoming_ref in ["REBASE_HEAD", "MERGE_HEAD", "CHERRY_PICK_HEAD"] {
            let incoming = git_output(&self.workdir, &["rev-parse", "--verify", incoming_ref])?;
            if incoming.status.success() {
                if let Some(id) = String::from_utf8_lossy(&incoming.stdout).lines().next() {
                    let base = git_output(&self.workdir, &["merge-base", &ours, id])?;
                    let base = base
                        .status
                        .success()
                        .then(|| String::from_utf8_lossy(&base.stdout).trim().to_string());
                    return Ok(Some(super::ConflictCommits {
                        ours,
                        theirs: id.to_string(),
                        base,
                    }));
                }
            }
        }
        Ok(None)
    }

    fn conflict_versions(&self, path: &GitPath) -> Result<Option<ConflictVersions>> {
        let unmerged =
            git_output_with_path_checked(&self.workdir, &["ls-files", "-u", "-z", "--"], path)?;
        if unmerged.stdout.is_empty() {
            return Ok(None);
        }
        if self.conflict_is_too_large(path) {
            return Ok(None);
        }
        let stage = |number: u8| -> Result<Option<Vec<u8>>> {
            let spec = revision_path(&format!(":{number}:"), path)?;
            let output = git_output_with_os_arg(&self.workdir, &["show"], spec)?;
            Ok(output.status.success().then_some(output.stdout))
        };
        Ok(Some(ConflictVersions {
            base: stage(1)?,
            ours: stage(2)?,
            theirs: stage(3)?,
            working: path
                .to_path_buf()
                .ok()
                .and_then(|path| fs::read(self.workdir.join(path)).ok()),
        }))
    }

    fn stage(&self, path: &GitPath) -> Result<()> {
        run_git_with_path(&self.workdir, &["add", "--"], path)
    }

    fn choose_conflict_side(&self, path: &GitPath, side: ConflictSide) -> Result<()> {
        if side == ConflictSide::Base {
            let spec = revision_path(":1:", path)?;
            let base = git_output_with_os_arg(&self.workdir, &["show"], spec)?;
            if base.status.success() {
                fs::write(self.workdir.join(path.to_path_buf()?), base.stdout)
                    .with_context(|| format!("restore base version of {path}"))?;
            } else {
                match fs::remove_file(self.workdir.join(path.to_path_buf()?)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error).with_context(|| format!("remove {path}")),
                }
            }
            return Ok(());
        }

        let choice = match side {
            ConflictSide::Base => unreachable!("handled above"),
            ConflictSide::Ours => "--ours",
            ConflictSide::Theirs => "--theirs",
        };
        run_git_with_path(&self.workdir, &["checkout", choice, "--"], path)
    }

    fn write_conflict_result(&self, path: &GitPath, result: &[u8]) -> Result<()> {
        fs::write(self.workdir.join(path.to_path_buf()?), result)
            .with_context(|| format!("write conflict result for {path}"))
    }

    fn resolve_conflict(&self, path: &GitPath, side: ConflictSide) -> Result<()> {
        self.choose_conflict_side(path, side)?;
        self.stage(path)
    }

    fn edit_conflict_file(&self, path: &GitPath) -> Result<()> {
        let editor = std::env::var("VISUAL")
            .ok()
            .filter(|editor| !editor.trim().is_empty())
            .or_else(|| {
                std::env::var("EDITOR")
                    .ok()
                    .filter(|editor| !editor.trim().is_empty())
            })
            .unwrap_or_else(|| "vi".to_string());
        let status = Command::new("sh")
            .args(["-c", "exec $EDITOR \"$@\"", "figit"])
            .arg(path.to_os_string()?)
            .env("EDITOR", editor)
            .current_dir(&self.workdir)
            .status()
            .context("open conflict file in external editor")?;
        if !status.success() {
            anyhow::bail!("external editor exited unsuccessfully: {status}");
        }
        Ok(())
    }

    fn unstage(&self, path: &GitPath) -> Result<()> {
        run_git_with_path(&self.workdir, &["restore", "--staged", "--"], path)
    }

    fn discard(&self, path: &GitPath) -> Result<()> {
        let restored = run_git_with_path(&self.workdir, &["restore", "--worktree", "--"], path);
        if restored.is_ok() {
            return Ok(());
        }
        // `git restore` does not operate on untracked files; discard those by
        // removing the selected untracked path after confirmation in the UI.
        run_git_with_path(&self.workdir, &["clean", "-f", "--"], path)
    }

    fn diff_stats(&self, path: &GitPath, staged: bool) -> Result<(usize, usize)> {
        let is_untracked = !staged && self.is_untracked(path)?;
        if is_untracked {
            return self.untracked_diff_stats(path);
        }
        let target = if staged {
            DiffTarget::Index
        } else {
            DiffTarget::Worktree
        };
        if self.diff_is_too_large(path, target) {
            return Ok((0, 0));
        }
        let mut args = vec!["diff"];
        if staged {
            args.push("--cached");
        }
        args.push("--numstat");
        args.push("--");
        let output = git_output_with_path(&self.workdir, &args, path)
            .with_context(|| "run git diff --numstat")?;
        if !output.status.success() {
            anyhow::bail!(
                "git diff --numstat failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let first = stdout.lines().next().unwrap_or("");
        let mut fields = first.split('\t');
        let added = fields.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let removed = fields.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        Ok((added, removed))
    }

    fn status_diff_stats(&self, entries: &[StatusEntry]) -> Result<Vec<PathDiffStats>> {
        let limit = self.diff_size_limit();
        let sizes = self.blob_sizes_for(entries);
        let index_large = |path: &GitPath| {
            sizes.index.get(path).is_some_and(|size| *size > limit)
                || sizes.head.get(path).is_some_and(|size| *size > limit)
        };
        let worktree_large =
            |path: &GitPath| sizes.worktree_large.contains(path) || index_large(path);
        let tracked_staged = entries
            .iter()
            .filter(|entry| {
                !entry.is_conflict()
                    && entry.index != ' '
                    && entry.index != '?'
                    && !index_large(&entry.path)
            })
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        let tracked_unstaged = entries
            .iter()
            .filter(|entry| {
                !entry.is_conflict()
                    && entry.index != '?'
                    && entry.worktree != ' '
                    && !worktree_large(&entry.path)
            })
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        let staged_stats = self.numstat_for_paths(&tracked_staged, true)?;
        let unstaged_stats = self.numstat_for_paths(&tracked_unstaged, false)?;
        let mut stats = HashMap::with_capacity(entries.len() * 2);

        for (path, counts) in staged_stats {
            stats.insert((path, true), counts);
        }
        for (path, counts) in unstaged_stats {
            stats.insert((path, false), counts);
        }
        for entry in entries.iter().filter(|entry| entry.index == '?') {
            stats.insert(
                (entry.path.clone(), false),
                self.untracked_diff_stats(&entry.path)?,
            );
        }

        let mut result = Vec::with_capacity(entries.len() * 2);
        for entry in entries {
            for staged in [false, true] {
                let (added, removed) = stats
                    .get(&(entry.path.clone(), staged))
                    .copied()
                    .unwrap_or((0, 0));
                result.push(PathDiffStats {
                    path: entry.path.clone(),
                    staged,
                    added,
                    removed,
                });
            }
        }
        Ok(result)
    }

    fn stash_list(&self) -> Result<Vec<super::StashEntry>> {
        let output = git_output_checked(
            &self.workdir,
            &["stash", "list", "--format=%gd%x00%H%x00%s"],
        )?;
        let mut entries = Vec::new();
        for record in output.stdout.split(|byte| *byte == b'\n') {
            if record.is_empty() {
                continue;
            }
            let fields = record.splitn(3, |byte| *byte == 0).collect::<Vec<_>>();
            if fields.len() == 3 {
                entries.push(super::StashEntry {
                    reference: String::from_utf8_lossy(fields[0]).into_owned(),
                    id: String::from_utf8_lossy(fields[1]).into_owned(),
                    message: String::from_utf8_lossy(fields[2]).into_owned(),
                });
            }
        }
        Ok(entries)
    }

    fn stash_push(&self, message: &str, include_untracked: bool) -> Result<()> {
        let mut command = Command::new("git");
        command.args(["stash", "push"]);
        if include_untracked {
            command.arg("--include-untracked");
        }
        let output = command
            .args(["--message", message])
            .current_dir(&self.workdir)
            .output()
            .context("run git stash push")?;
        if !output.status.success() {
            anyhow::bail!(
                "git stash push failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    fn stash_apply(&self, reference: &str) -> Result<()> {
        run_git(&self.workdir, ["stash", "apply", reference])
    }

    fn stash_pop(&self, reference: &str) -> Result<()> {
        run_git(&self.workdir, ["stash", "pop", reference])
    }

    fn stash_drop(&self, reference: &str) -> Result<()> {
        run_git(&self.workdir, ["stash", "drop", reference])
    }

    fn commit_files(&self, commit: &str) -> Result<Vec<StatusEntry>> {
        let parents = self.commit_parents(commit)?;
        let mut args = vec![
            "diff-tree",
            "--no-commit-id",
            "--name-status",
            "--no-renames",
            "-r",
            "-z",
        ];
        if parents.len() > 1 {
            // Show a merge commit as the change it introduced relative to its
            // first parent, matching the commit's normal graph presentation.
            args.push(parents[0].as_str());
            args.push(commit);
        } else {
            if parents.is_empty() {
                args.push("--root");
            }
            args.push(commit);
        }
        let output = Command::new("git")
            .args(&args)
            .current_dir(&self.workdir)
            .output()
            .with_context(|| "run git diff-tree")?;
        if !output.status.success() {
            anyhow::bail!(
                "git diff-tree failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let mut entries = Vec::new();
        let fields: Vec<&[u8]> = output
            .stdout
            .split(|b| *b == 0)
            .filter(|f| !f.is_empty())
            .collect();
        let mut i = 0;
        while i + 1 < fields.len() {
            let code = fields[i].first().copied().unwrap_or(b'M') as char;
            let path = GitPath::from_bytes(fields[i + 1].to_vec());
            entries.push(StatusEntry {
                path,
                index: code,
                worktree: ' ',
            });
            i += 2;
        }
        Ok(entries)
    }

    fn commit_diff(&self, commit: &str, path: &GitPath) -> Result<String> {
        let parents = self.commit_parents(commit)?;
        if self.commit_path_is_too_large(commit, path, &parents) {
            return Ok(too_large_diff_notice(path, self.diff_size_limit()));
        }
        let output = if parents.len() > 1 {
            git_output_with_path(
                &self.workdir,
                &["diff", "--no-ext-diff", parents[0].as_str(), commit, "--"],
                path,
            )
            .with_context(|| "run first-parent merge diff")?
        } else {
            git_output_with_path(
                &self.workdir,
                &["show", "--format=", "--no-ext-diff", commit, "--"],
                path,
            )
            .with_context(|| "run git show")?
        };
        if !output.status.success() {
            anyhow::bail!(
                "git commit diff failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn commit_diff_stats(&self, commit: &str) -> Result<Vec<(GitPath, usize, usize)>> {
        let parents = self.commit_parents(commit)?;
        let output = if parents.len() > 1 {
            Command::new("git")
                .args([
                    "diff",
                    "--numstat",
                    "-z",
                    "--no-renames",
                    parents[0].as_str(),
                    commit,
                    "--",
                ])
                .current_dir(&self.workdir)
                .output()
                .with_context(|| "run first-parent merge numstat")?
        } else {
            Command::new("git")
                .args([
                    "show",
                    "--format=",
                    "--numstat",
                    "-z",
                    "--no-renames",
                    commit,
                    "--",
                ])
                .current_dir(&self.workdir)
                .output()
                .with_context(|| "run git show --numstat")?
        };
        if !output.status.success() {
            anyhow::bail!(
                "git show --numstat failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let mut stats = Vec::new();
        for record in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            let fields = record.splitn(3, |byte| *byte == b'\t').collect::<Vec<_>>();
            let [added, removed, path] = fields.as_slice() else {
                continue;
            };
            let added = std::str::from_utf8(added)
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            let removed = std::str::from_utf8(removed)
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            stats.push((GitPath::from_bytes(path.to_vec()), added, removed));
        }
        Ok(stats)
    }

    fn commit(
        &self,
        title: &str,
        description: &str,
        amend: bool,
        fixup: Option<&str>,
    ) -> Result<()> {
        let mut command = Command::new("git");
        command.arg("commit");
        if let Some(target) = fixup {
            command.arg(format!("--fixup={target}"));
        } else {
            if amend {
                command.arg("--amend");
            }
            command.args(["-m", title]);
            if !description.trim().is_empty() {
                command.args(["-m", description]);
            }
        }
        let output = command
            .current_dir(&self.workdir)
            .output()
            .with_context(|| "run git commit")?;
        if !output.status.success() {
            anyhow::bail!(
                "git commit failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    fn rebase_commit(&self, target: &str, action: &RebaseAction) -> Result<RebaseOutcome> {
        if rebase_in_progress(&self.workdir)? {
            anyhow::bail!("a Git rebase is already in progress");
        }

        let status = git_output_checked(&self.workdir, &["status", "--porcelain=v1", "-z"])?;
        if !status.stdout.is_empty() {
            anyhow::bail!("commit or stash working tree changes before rewriting history");
        }

        let head_ref = git_output(&self.workdir, &["symbolic-ref", "--quiet", "HEAD"])?;
        if !head_ref.status.success() {
            anyhow::bail!("interactive rebase requires a checked-out branch");
        }
        let is_ancestor = git_output(
            &self.workdir,
            &["merge-base", "--is-ancestor", target, "HEAD"],
        )?;
        if !is_ancestor.status.success() {
            anyhow::bail!("selected commit is not reachable from the checked-out branch");
        }

        let target_line =
            git_output_checked(&self.workdir, &["rev-list", "--parents", "-n", "1", target])?;
        let target_line = String::from_utf8_lossy(&target_line.stdout);
        let target_fields: Vec<&str> = target_line.split_whitespace().collect();
        if matches!(action, RebaseAction::Edit) && target_fields.len() == 1 {
            anyhow::bail!("editing a root commit is not supported yet");
        }
        let parent = target_fields.get(1).copied();

        let temp = tempfile::tempdir().context("create interactive rebase helper directory")?;
        let sequence_editor = temp.path().join("sequence-editor.sh");
        let matched_marker = temp.path().join("target-matched");
        fs::write(
            &sequence_editor,
            "#!/bin/sh\n\
             todo=\"$1\"\n\
             tmp=\"${todo}.figit\"\n\
             awk -v target=\"$FIGIT_REBASE_TARGET\" -v action=\"$FIGIT_REBASE_ACTION\" '\n\
               $1 == \"pick\" && $2 != \"\" && substr(target, 1, length($2)) == $2 { sub(/^[[:space:]]*[^[:space:]]+/, action); found++; print; next }\n\
               $1 == \"merge\" && $3 != \"\" && substr(target, 1, length($3)) == $3 {\n\
                 if (action == \"reword\") { sub(/-C/, \"-c\") }\n\
                 print; found++\n\
                 if (action == \"edit\") print \"break\"\n\
                 next\n\
               }\n\
               { print }\n\
               END { if (found != 1) exit 1 }\n\
             ' \"$todo\" > \"$tmp\" || exit $?\n\
             mv \"$tmp\" \"$todo\" || exit $?\n\
             : > \"$FIGIT_REBASE_MATCHED\"\n",
        )
        .context("write interactive rebase sequence editor")?;

        let sequence_editor_command = format!("sh {}", shell_quote(&sequence_editor));
        let mut command = Command::new("git");
        command.args([
            "rebase",
            "-i",
            "--rebase-merges",
            "--no-autosquash",
            "--keep-empty",
        ]);
        if let Some(parent) = parent {
            command.arg(parent);
        } else {
            command.arg("--root");
        }
        command
            .current_dir(&self.workdir)
            .env("GIT_SEQUENCE_EDITOR", sequence_editor_command)
            .env("FIGIT_REBASE_TARGET", target)
            .env(
                "FIGIT_REBASE_ACTION",
                match action {
                    RebaseAction::Reword { .. } => "reword",
                    RebaseAction::Edit => "edit",
                },
            )
            .env("FIGIT_REBASE_MATCHED", &matched_marker);

        if let RebaseAction::Reword { title, description } = action {
            let message_file = temp.path().join("message.txt");
            let message = if description.trim().is_empty() {
                format!("{}\n", title.trim_end())
            } else {
                format!("{}\n\n{}\n", title.trim_end(), description.trim_end())
            };
            fs::write(&message_file, message).context("write reworded commit message")?;
            let message_editor = temp.path().join("message-editor.sh");
            fs::write(
                &message_editor,
                "#!/bin/sh\ncp \"$FIGIT_REBASE_MESSAGE\" \"$1\"\n",
            )
            .context("write commit message editor")?;
            command
                .env("GIT_EDITOR", format!("sh {}", shell_quote(&message_editor)))
                .env("FIGIT_REBASE_MESSAGE", message_file);
        }

        let output = command.output().context("run interactive git rebase")?;
        if !output.status.success() {
            if matched_marker.exists() && rebase_in_progress(&self.workdir).unwrap_or(false) {
                let _ = run_git(&self.workdir, ["rebase", "--abort"]);
            }
            anyhow::bail!(
                "interactive rebase failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        if !matched_marker.exists() {
            anyhow::bail!("interactive rebase did not find the selected commit");
        }

        match action {
            RebaseAction::Reword { .. } => Ok(RebaseOutcome::Completed),
            RebaseAction::Edit => self.rebase_stop_info(target),
        }
    }

    fn prepare_rebase_plan(&self, target: &str) -> Result<RebasePlan> {
        self.validate_rebase_start(target)?;
        let target_line =
            git_output_checked(&self.workdir, &["rev-list", "--parents", "-n", "1", target])?;
        let target_line = String::from_utf8_lossy(&target_line.stdout);
        let fields: Vec<&str> = target_line.split_whitespace().collect();
        let upstream = fields.get(1).map(|parent| (*parent).to_string());

        let temp = tempfile::tempdir().context("create rebase todo preview directory")?;
        let capture_file = temp.path().join("todo.txt");
        let sequence_editor = temp.path().join("capture-todo.sh");
        fs::write(
            &sequence_editor,
            "#!/bin/sh\ncp \"$1\" \"$FIGIT_REBASE_TODO_CAPTURE\" || exit $?\nexit 1\n",
        )
        .context("write rebase todo preview editor")?;
        let mut command = Command::new("git");
        command.args([
            "rebase",
            "-i",
            "--rebase-merges",
            "--no-autosquash",
            "--keep-empty",
        ]);
        if let Some(upstream) = &upstream {
            command.arg(upstream);
        } else {
            command.arg("--root");
        }
        let output = command
            .current_dir(&self.workdir)
            .env(
                "GIT_SEQUENCE_EDITOR",
                format!("sh {}", shell_quote(&sequence_editor)),
            )
            .env("FIGIT_REBASE_TODO_CAPTURE", &capture_file)
            .output()
            .context("preview interactive rebase todo")?;
        if rebase_in_progress(&self.workdir).unwrap_or(false) {
            let _ = run_git(&self.workdir, ["rebase", "--abort"]);
        }
        if !capture_file.exists() {
            anyhow::bail!(
                "could not generate interactive rebase plan: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let todo = fs::read_to_string(&capture_file).context("read generated rebase todo")?;
        let blocks = parse_rebase_todo(&self.workdir, &todo)?;
        if !blocks.iter().any(|block| match block {
            RebasePlanBlock::Control(_) => false,
            RebasePlanBlock::Commits(entries) => !entries.is_empty(),
            RebasePlanBlock::Merge(_) => true,
        }) {
            anyhow::bail!("the selected commit has no commits to rebase");
        }
        Ok(RebasePlan {
            target: target.to_string(),
            upstream,
            blocks,
        })
    }

    fn execute_rebase_plan(&self, plan: &RebasePlan) -> Result<RebaseOutcome> {
        self.validate_rebase_start(&plan.target)?;
        validate_rebase_plan(plan)?;

        let temp = tempfile::tempdir().context("create interactive rebase plan directory")?;
        let todo_file = temp.path().join("planned-todo.txt");
        let sequence_editor = temp.path().join("sequence-editor.sh");
        let matched_marker = temp.path().join("sequence-applied");
        let mut todo = String::new();
        let mut editor_actions = Vec::new();
        for block in &plan.blocks {
            match block {
                RebasePlanBlock::Control(line) => {
                    todo.push_str(line);
                    todo.push('\n');
                }
                RebasePlanBlock::Merge(entry) => {
                    todo.push_str(&entry.tail);
                    todo.push('\n');
                }
                RebasePlanBlock::Commits(entries) => {
                    for entry in entries {
                        let command = match &entry.action {
                            RebaseTodoAction::Pick => "pick",
                            RebaseTodoAction::Reword => {
                                let message_file = format!("message-{}.txt", editor_actions.len());
                                let source = temp.path().join("editor-source");
                                fs::create_dir_all(&source)
                                    .context("create reword message directory")?;
                                fs::write(
                                    source.join(&message_file),
                                    format_commit_message(&entry.title, &entry.description),
                                )
                                .context("write planned reword message")?;
                                editor_actions.push(format!("M {message_file}"));
                                "reword"
                            }
                            RebaseTodoAction::Edit => "edit",
                            RebaseTodoAction::Squash => {
                                editor_actions.push("K".to_string());
                                "squash"
                            }
                            RebaseTodoAction::Fixup => "fixup",
                            RebaseTodoAction::Drop => "drop",
                            RebaseTodoAction::Merge => {
                                anyhow::bail!(
                                    "merge todo rows are structural and cannot be reassigned"
                                )
                            }
                        };
                        todo.push_str(command);
                        todo.push(' ');
                        todo.push_str(&entry.tail);
                        todo.push('\n');
                    }
                }
            }
        }
        fs::write(&todo_file, todo).context("write planned interactive rebase todo")?;
        fs::write(
            &sequence_editor,
            "#!/bin/sh\ncp \"$FIGIT_REBASE_TODO_FILE\" \"$1\" || exit $?\n: > \"$FIGIT_REBASE_TODO_APPLIED\"\n",
        )
        .context("write interactive rebase sequence editor")?;

        let rebase_editor_directory = rebase_state_directory(&self.workdir)?.join("figit-plan");
        let mut command = Command::new("git");
        command.args([
            "rebase",
            "-i",
            "--rebase-merges",
            "--no-autosquash",
            "--keep-empty",
        ]);
        if let Some(upstream) = &plan.upstream {
            command.arg(upstream);
        } else {
            command.arg("--root");
        }
        command
            .current_dir(&self.workdir)
            .env(
                "GIT_SEQUENCE_EDITOR",
                format!("sh {}", shell_quote(&sequence_editor)),
            )
            .env("GIT_EDITOR", "true")
            .env("FIGIT_REBASE_TODO_FILE", &todo_file)
            .env("FIGIT_REBASE_TODO_APPLIED", &matched_marker);

        if !editor_actions.is_empty() {
            let editor_source = temp.path().join("editor-source");
            fs::create_dir_all(&editor_source).context("create reword message directory")?;
            fs::write(editor_source.join("actions.txt"), editor_actions.join("\n"))
                .context("write planned editor actions")?;
            fs::write(
                editor_source.join("editor.sh"),
                "#!/bin/sh\ntarget_file=\"$1\"\ndir=$(dirname \"$0\")\nindex=$(cat \"$dir/index\" 2>/dev/null || echo 1)\naction=$(sed -n \"${index}p\" \"$dir/actions.txt\")\n[ -n \"$action\" ] || exit 0\necho $((index + 1)) > \"$dir/index\"\nset -- $action\n[ \"$1\" = M ] || exit 0\ncp \"$dir/$2\" \"$target_file\" || exit $?\n",
            )
            .context("write reword editor")?;
            fs::write(editor_source.join("index"), "1\n")
                .context("initialize rebase editor action index")?;
            let copy_editor = temp.path().join("copy-editor-files.sh");
            fs::write(
                &copy_editor,
                "#!/bin/sh\nmkdir -p \"$FIGIT_REBASE_EDITOR_DIR\" || exit $?\ncp -R \"$FIGIT_REBASE_EDITOR_SOURCE/.\" \"$FIGIT_REBASE_EDITOR_DIR/\" || exit $?\n",
            )
            .context("write reword editor installer")?;
            // The sequence editor installs editor data under Git's rebase state
            // directory so it remains available after an `edit` pause.
            fs::write(
                &sequence_editor,
                format!(
                    "#!/bin/sh\ncp \"$FIGIT_REBASE_TODO_FILE\" \"$1\" || exit $?\n: > \"$FIGIT_REBASE_TODO_APPLIED\"\nsh {} || exit $?\n",
                    shell_quote(&copy_editor)
                ),
            )
            .context("install persistent reword editor")?;
            command
                .env(
                    "GIT_EDITOR",
                    format!(
                        "sh {}",
                        shell_quote(&rebase_editor_directory.join("editor.sh"))
                    ),
                )
                .env("FIGIT_REBASE_EDITOR_SOURCE", &editor_source)
                .env("FIGIT_REBASE_EDITOR_DIR", &rebase_editor_directory);
        }

        let output = command.output().context("run planned interactive rebase")?;
        if !output.status.success() {
            if !matched_marker.exists() && rebase_in_progress(&self.workdir).unwrap_or(false) {
                let _ = run_git(&self.workdir, ["rebase", "--abort"]);
            }
            anyhow::bail!(
                "interactive rebase failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        if !matched_marker.exists() {
            anyhow::bail!("Git did not apply the planned rebase todo");
        }
        if rebase_in_progress(&self.workdir)? {
            self.rebase_stop_info(&plan.target)
        } else {
            Ok(RebaseOutcome::Completed)
        }
    }

    fn finish_rebase_edit(
        &self,
        target: &str,
        parents: &[String],
        title: &str,
        description: &str,
    ) -> Result<()> {
        if !rebase_in_progress(&self.workdir)? {
            anyhow::bail!("the interactive rebase is no longer active");
        }
        if parents.is_empty() {
            anyhow::bail!("editing a root commit is not supported yet");
        }
        let staged = git_output(&self.workdir, &["diff", "--cached", "--quiet"])?;
        if staged.status.success() {
            anyhow::bail!("stage at least one change before creating the replacement commit");
        }
        if staged.status.code() != Some(1) {
            anyhow::bail!("could not inspect staged changes");
        }
        let head = git_output_checked(&self.workdir, &["rev-parse", "HEAD"])?;
        if String::from_utf8_lossy(&head.stdout).trim() != parents[0] {
            anyhow::bail!("HEAD moved while the rebase edit was in progress");
        }

        if parents.len() == 1 {
            let (author_name, author_email, author_date) = commit_author(&self.workdir, target)?;
            let mut command = Command::new("git");
            command
                .args(["commit", "-m", title])
                .env("GIT_AUTHOR_NAME", author_name)
                .env("GIT_AUTHOR_EMAIL", author_email)
                .env("GIT_AUTHOR_DATE", author_date);
            if !description.trim().is_empty() {
                command.args(["-m", description]);
            }
            let output = command
                .current_dir(&self.workdir)
                .output()
                .context("create replacement commit for rebase edit")?;
            if !output.status.success() {
                anyhow::bail!(
                    "could not create replacement commit: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            return Ok(());
        }

        let tree = git_output_checked(&self.workdir, &["write-tree"])?;
        let tree = String::from_utf8_lossy(&tree.stdout).trim().to_string();
        let (author_name, author_email, author_date) = commit_author(&self.workdir, target)?;
        let message = if description.trim().is_empty() {
            format!("{}\n", title.trim_end())
        } else {
            format!("{}\n\n{}\n", title.trim_end(), description.trim_end())
        };
        let mut command = Command::new("git");
        command
            .arg("commit-tree")
            .arg(&tree)
            .args(parents.iter().flat_map(|parent| ["-p", parent.as_str()]))
            .current_dir(&self.workdir)
            .env("GIT_AUTHOR_NAME", author_name)
            .env("GIT_AUTHOR_EMAIL", author_email)
            .env("GIT_AUTHOR_DATE", author_date)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().context("create replacement merge commit")?;
        child
            .stdin
            .take()
            .context("open replacement commit message input")?
            .write_all(message.as_bytes())
            .context("write replacement merge commit message")?;
        let output = child
            .wait_with_output()
            .context("finish replacement merge commit")?;
        if !output.status.success() {
            anyhow::bail!(
                "could not create replacement merge commit: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let replacement = String::from_utf8_lossy(&output.stdout).trim().to_string();
        run_git(&self.workdir, ["update-ref", "HEAD", &replacement])
    }

    fn rebase_in_progress(&self) -> Result<bool> {
        rebase_in_progress(&self.workdir)
    }

    fn abort_rebase(&self) -> Result<()> {
        if !rebase_in_progress(&self.workdir)? {
            anyhow::bail!("no Git rebase is in progress");
        }
        run_git(&self.workdir, ["rebase", "--abort"])
    }

    fn continue_rebase(&self) -> Result<RebaseOutcome> {
        if !rebase_in_progress(&self.workdir)? {
            anyhow::bail!("no Git rebase is in progress");
        }
        let planned_editor = rebase_state_directory(&self.workdir)?.join("figit-plan/editor.sh");
        let editor = if planned_editor.exists() {
            format!("sh {}", shell_quote(&planned_editor))
        } else {
            "true".to_string()
        };
        let output = Command::new("git")
            .args(["rebase", "--continue"])
            .current_dir(&self.workdir)
            .env("GIT_EDITOR", editor)
            .output()
            .context("continue interactive git rebase")?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            let error = if error.trim().is_empty() {
                String::from_utf8_lossy(&output.stdout)
            } else {
                error
            };
            anyhow::bail!("git rebase --continue failed: {}", error.trim());
        }
        if rebase_in_progress(&self.workdir)? {
            self.rebase_stop_info("HEAD")
        } else {
            Ok(RebaseOutcome::Completed)
        }
    }

    fn file_line_number_width(&self, path: &GitPath, commit: Option<&str>) -> Result<usize> {
        let too_large = if let Some(commit) = commit {
            self.blob_size(&format!("{commit}:"), path)
                .is_some_and(|size| size > self.diff_size_limit())
        } else {
            self.diff_is_too_large(path, DiffTarget::Index)
                || self.diff_is_too_large(path, DiffTarget::Worktree)
        };
        if too_large {
            return Ok(3);
        }
        let bytes = if let Some(commit) = commit {
            let spec = revision_path(&format!("{commit}:"), path)?;
            let output = git_output_with_os_arg(&self.workdir, &["show"], spec)
                .with_context(|| "read committed file")?;
            output.stdout
        } else {
            match path
                .to_path_buf()
                .and_then(|path| fs::read(self.workdir.join(path)))
            {
                Ok(bytes) => bytes,
                Err(_) => {
                    let spec = revision_path("HEAD:", path)?;
                    git_output_with_os_arg(&self.workdir, &["show"], spec)
                        .with_context(|| "read HEAD version of file")?
                        .stdout
                }
            }
        };
        let lines = if bytes.is_empty() {
            1
        } else {
            bytes.iter().filter(|&&byte| byte == b'\n').count()
                + usize::from(bytes.last() != Some(&b'\n'))
        };
        Ok(lines.to_string().len())
    }

    fn branch(&self, name: &str, target: &str) -> Result<()> {
        run_git(&self.workdir, ["branch", name, target])
    }

    fn tag(&self, name: &str, target: &str) -> Result<()> {
        run_git(&self.workdir, ["tag", name, target])
    }

    fn reset(&self, target: &str, mode: &str) -> Result<()> {
        run_git(&self.workdir, ["reset", mode, target])
    }

    fn checkout(&self, target: &str) -> Result<()> {
        run_git(&self.workdir, ["checkout", target])
    }

    fn undo_options(&self) -> Result<Vec<UndoInfo>> {
        undo_options(&self.workdir)
    }

    fn undo_ref_change(&self, expected: &UndoInfo) -> Result<()> {
        if self.rebase_in_progress()? || has_in_progress_git_operation(&self.workdir)? {
            anyhow::bail!("finish or abort the current Git operation before undoing a ref change");
        }
        if self.status()?.iter().any(StatusEntry::is_conflict) {
            anyhow::bail!("resolve or abort conflicts before undoing a ref change");
        }

        let current = undo_options(&self.workdir)?;
        if !current.contains(expected) {
            anyhow::bail!("repository history changed after the undo prompt was opened; refresh and try again");
        }

        if let Some(source_branch) = expected.restore_branch.as_deref() {
            let previous_spec = format!("{source_branch}^{{commit}}");
            let source_id =
                git_output_checked(&self.workdir, &["rev-parse", "--verify", &previous_spec])?;
            let source_id = String::from_utf8_lossy(&source_id.stdout)
                .trim()
                .to_string();
            if source_id != expected.previous_id {
                anyhow::bail!("the previous checkout target has moved; refusing to check out a different state");
            }
            run_git(&self.workdir, ["checkout", "--quiet", source_branch])
                .context("restore the previous checkout")
        } else {
            run_git(
                &self.workdir,
                ["reset", "--soft", expected.previous_id.as_str()],
            )
            .context("move HEAD back to the previous reflog entry")
        }
    }

    fn cherry_pick(&self, target: &str) -> Result<()> {
        run_git(&self.workdir, ["cherry-pick", target])
    }

    fn revert(&self, target: &str) -> Result<()> {
        run_git(&self.workdir, ["revert", target])
    }

    fn rename_branch(&self, old: &str, new: &str, remote: bool) -> Result<()> {
        let remote_name = if remote {
            Some(self.branch_upstream_remote(old)?)
        } else {
            None
        };
        run_git(&self.workdir, ["branch", "-m", old, new])?;
        if let Some(remote_name) = remote_name {
            let new_ref = format!("refs/heads/{new}:refs/heads/{new}");
            let old_ref = format!(":refs/heads/{old}");
            if let Err(error) = run_git(
                &self.workdir,
                [
                    "push",
                    "--atomic",
                    remote_name.as_str(),
                    new_ref.as_str(),
                    old_ref.as_str(),
                ],
            ) {
                if let Err(rollback) = run_git(&self.workdir, ["branch", "-m", new, old]) {
                    anyhow::bail!(
                        "remote rename failed ({error:#}); local branch rollback also failed ({rollback:#})"
                    );
                }
                return Err(error).context("rename remote branch; local rename was rolled back");
            }
            let upstream = format!("{remote_name}/{new}");
            run_git(
                &self.workdir,
                ["branch", "--set-upstream-to", upstream.as_str(), new],
            )
            .context("remote branch was renamed, but setting its upstream failed")?;
        }
        Ok(())
    }

    fn delete_branch(&self, name: &str, remote: bool) -> Result<()> {
        let Some(remote_name) = remote
            .then(|| self.branch_upstream_remote(name))
            .transpose()?
        else {
            return run_git(&self.workdir, ["branch", "-D", name]);
        };
        let current = git_output(
            &self.workdir,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
        )?;
        if current.status.success() && String::from_utf8_lossy(&current.stdout).trim() == name {
            anyhow::bail!("cannot delete the checked-out branch '{name}' remotely first");
        }

        // Delete the remote ref first so a failed remote operation leaves the
        // local branch untouched. If local deletion fails, restore the remote.
        run_git(
            &self.workdir,
            ["push", remote_name.as_str(), "--delete", name],
        )?;
        if let Err(error) = run_git(&self.workdir, ["branch", "-D", name]) {
            let restore = format!("refs/heads/{name}:refs/heads/{name}");
            if let Err(rollback) = run_git(
                &self.workdir,
                ["push", remote_name.as_str(), restore.as_str()],
            ) {
                anyhow::bail!(
                    "local branch deletion failed ({error:#}); restoring the remote branch also failed ({rollback:#})"
                );
            }
            return Err(error).context("delete local branch; remote branch was restored");
        }
        Ok(())
    }

    fn delete_tag(&self, name: &str, remote: bool) -> Result<()> {
        if !remote {
            return run_git(&self.workdir, ["tag", "-d", name]);
        }
        let Some(remote_name) = super::GitBackend::default_remote(self, true)? else {
            anyhow::bail!("no remote is configured to delete the tag from");
        };
        let refspec = format!("refs/tags/{name}");
        // Delete remotely first so a failure leaves the local tag intact, then
        // restore the remote tag if the local deletion fails.
        run_git(
            &self.workdir,
            ["push", remote_name.as_str(), "--delete", refspec.as_str()],
        )?;
        if let Err(error) = run_git(&self.workdir, ["tag", "-d", name]) {
            let restore = format!("refs/tags/{name}:refs/tags/{name}");
            if let Err(rollback) = run_git(
                &self.workdir,
                ["push", remote_name.as_str(), restore.as_str()],
            ) {
                anyhow::bail!(
                    "local tag deletion failed ({error:#}); restoring the remote tag also failed ({rollback:#})"
                );
            }
            return Err(error).context("delete local tag; remote tag was restored");
        }
        Ok(())
    }

    fn pull(&self) -> Result<()> {
        run_git(&self.workdir, ["pull"])
    }

    fn remotes(&self) -> Result<Vec<String>> {
        let output = git_output_checked(&self.workdir, &["remote"])?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|remote| !remote.is_empty())
            .map(str::to_string)
            .collect())
    }

    fn default_remote(&self, for_push: bool) -> Result<Option<String>> {
        let head = git_output(
            &self.workdir,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
        )?;
        let branch = head
            .status
            .success()
            .then(|| String::from_utf8_lossy(&head.stdout).trim().to_string());
        self.resolve_remote(branch.as_deref(), for_push)
    }

    fn default_remote_for_branch(&self, branch: &str, for_push: bool) -> Result<Option<String>> {
        self.resolve_remote(Some(branch), for_push)
    }

    fn fetch(&self, remote: &str) -> Result<()> {
        run_git(&self.workdir, ["fetch", remote])
    }

    fn push(&self, remote: &str, branch: &str, mode: usize) -> Result<()> {
        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
        let mode = match mode {
            1 => Some("--force"),
            2 => Some("--force-with-lease"),
            _ => None,
        };
        if let Some(mode) = mode {
            run_git(&self.workdir, ["push", mode, remote, refspec.as_str()])
        } else {
            run_git(&self.workdir, ["push", remote, refspec.as_str()])
        }
    }

    fn push_tag(&self, remote: &str, tag: &str, mode: usize) -> Result<()> {
        let refspec = format!("refs/tags/{tag}:refs/tags/{tag}");
        // Tags are not normally rewound; force is only meaningful if the caller
        // explicitly asks for it (e.g. moving a release tag).
        if mode == 1 {
            run_git(&self.workdir, ["push", "--force", remote, refspec.as_str()])
        } else {
            run_git(&self.workdir, ["push", remote, refspec.as_str()])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parser_preserves_non_utf8_path_bytes() {
        let entries = parse_status(b"?? non-utf8-\xff.txt\0").expect("parse status");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path.as_bytes(), b"non-utf8-\xff.txt");
        assert!(entries[0].path.display().contains("\\xFF"));
    }
}

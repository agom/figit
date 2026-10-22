pub mod gix;

use std::path::Path;

use crate::core::model::{GitPath, GraphModel, StatusEntry};

/// Git operations used by the UI. Repository reads use `gix`; mutations are
/// implemented behind this trait so UI workflows remain backend-independent.
pub trait GitBackend {
    fn working_directory(&self) -> &Path;
    fn watch_paths(&self) -> anyhow::Result<Vec<std::path::PathBuf>>;
    fn build_graph(&self) -> anyhow::Result<GraphModel>;
    /// All reachable commit ids in the repository's stable graph order.
    fn ordered_history_ids(&self) -> anyhow::Result<Vec<String>>;
    /// Decode a page from a previously ordered id list without rerunning git log.
    fn graph_page(&self, ids: &[String], skip: usize, limit: usize) -> anyhow::Result<GraphWindow>;
    /// A page of history in graph order. `complete` marks the final page.
    fn graph_window(&self, skip: usize, limit: usize) -> anyhow::Result<GraphWindow>;
    fn status(&self) -> anyhow::Result<Vec<StatusEntry>>;
    fn diff(&self, path: &GitPath, target: DiffTarget) -> anyhow::Result<String>;
    fn diff_hunk_counts(&self, path: &GitPath) -> anyhow::Result<(usize, usize)>;
    /// Staged and worktree hunk counts for many paths at once. Batching keeps
    /// the refresh from spawning a handful of `git` processes per file.
    fn status_hunk_counts(
        &self,
        entries: &[StatusEntry],
    ) -> anyhow::Result<Vec<(GitPath, (usize, usize))>>;
    fn apply_hunk(
        &self,
        path: &GitPath,
        target: DiffTarget,
        hunk_index: usize,
    ) -> anyhow::Result<()>;
    fn apply_diff_line(
        &self,
        path: &GitPath,
        target: DiffTarget,
        line: &DiffLineSelection,
    ) -> anyhow::Result<()>;
    /// Current base (ours) and incoming commit (theirs) during a conflict.
    fn conflict_commits(&self) -> anyhow::Result<Option<ConflictCommits>>;
    fn conflict_versions(&self, path: &GitPath) -> anyhow::Result<Option<ConflictVersions>>;
    fn choose_conflict_side(&self, path: &GitPath, side: ConflictSide) -> anyhow::Result<()>;
    fn write_conflict_result(
        &self,
        path: &GitPath,
        result: &ConflictResolution,
    ) -> anyhow::Result<()>;
    fn stage(&self, path: &GitPath) -> anyhow::Result<()>;
    fn resolve_conflict(&self, path: &GitPath, side: ConflictSide) -> anyhow::Result<()>;
    fn edit_conflict_file(&self, path: &GitPath) -> anyhow::Result<()>;
    fn unstage(&self, path: &GitPath) -> anyhow::Result<()>;
    fn discard(&self, path: &GitPath) -> anyhow::Result<()>;
    fn diff_stats(&self, path: &GitPath, staged: bool) -> anyhow::Result<(usize, usize)>;
    fn status_diff_stats(&self, entries: &[StatusEntry]) -> anyhow::Result<Vec<PathDiffStats>>;
    fn stash_list(&self) -> anyhow::Result<Vec<StashEntry>>;
    fn stash_push(&self, message: &str, include_untracked: bool) -> anyhow::Result<()>;
    fn stash_apply(&self, reference: &str) -> anyhow::Result<()>;
    fn stash_pop(&self, reference: &str) -> anyhow::Result<()>;
    fn stash_drop(&self, reference: &str) -> anyhow::Result<()>;
    fn commit_files(&self, commit: &str) -> anyhow::Result<Vec<StatusEntry>>;
    fn commit_diff(&self, commit: &str, path: &GitPath) -> anyhow::Result<String>;
    fn commit_diff_stats(&self, commit: &str) -> anyhow::Result<Vec<(GitPath, usize, usize)>>;
    fn commit(
        &self,
        title: &str,
        description: &str,
        amend: bool,
        fixup: Option<&str>,
    ) -> anyhow::Result<()>;
    fn rebase_commit(&self, target: &str, action: &RebaseAction) -> anyhow::Result<RebaseOutcome>;
    fn rebase_onto(&self, target: &str) -> anyhow::Result<RebaseOutcome>;
    fn prepare_rebase_plan(&self, target: &str) -> anyhow::Result<RebasePlan>;
    fn execute_rebase_plan(&self, plan: &RebasePlan) -> anyhow::Result<RebaseOutcome>;
    fn rebase_in_progress(&self) -> anyhow::Result<bool>;
    fn abort_rebase(&self) -> anyhow::Result<()>;
    fn continue_rebase(&self) -> anyhow::Result<RebaseOutcome>;
    fn finish_rebase_edit(
        &self,
        target: &str,
        parents: &[String],
        title: &str,
        description: &str,
    ) -> anyhow::Result<()>;
    fn file_line_number_width(&self, path: &GitPath, commit: Option<&str>)
        -> anyhow::Result<usize>;
    fn branch(&self, name: &str, target: &str) -> anyhow::Result<()>;
    fn tag(&self, name: &str, target: &str) -> anyhow::Result<()>;
    fn reset(&self, target: &str, mode: &str) -> anyhow::Result<()>;
    fn checkout(&self, target: &str) -> anyhow::Result<()>;
    fn undo_options(&self) -> anyhow::Result<Vec<UndoInfo>>;
    fn undo_ref_change(&self, expected: &UndoInfo) -> anyhow::Result<()>;
    fn cherry_pick(&self, target: &str) -> anyhow::Result<()>;
    fn revert(&self, target: &str) -> anyhow::Result<()>;
    fn rename_branch(&self, old: &str, new: &str, remote: bool) -> anyhow::Result<()>;
    fn delete_branch(&self, name: &str, remote: bool) -> anyhow::Result<()>;
    fn delete_tag(&self, name: &str, remote: bool) -> anyhow::Result<()>;
    fn pull(&self) -> anyhow::Result<()>;
    fn remotes(&self) -> anyhow::Result<Vec<String>>;
    fn default_remote(&self, for_push: bool) -> anyhow::Result<Option<String>>;
    fn default_remote_for_branch(
        &self,
        branch: &str,
        for_push: bool,
    ) -> anyhow::Result<Option<String>>;
    fn fetch(&self, remote: &str) -> anyhow::Result<()>;
    fn push(&self, remote: &str, branch: &str, mode: usize) -> anyhow::Result<()>;
    fn push_tag(&self, remote: &str, tag: &str, mode: usize) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphWindow {
    pub commits: Vec<crate::core::model::Commit>,
    /// Present on the first page only.
    pub refs: Vec<crate::core::model::RefLabel>,
    pub branch_tracking: Vec<crate::core::model::BranchTracking>,
    pub head_id: Option<crate::core::model::Oid>,
    pub head_name: Option<String>,
    /// True when this page reached the end of the reachable history.
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebaseAction {
    Reword { title: String, description: String },
    Edit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictSide {
    Base,
    Ours,
    Theirs,
}

/// A deletion is distinct from an empty file and from no pending resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictResolution {
    Content(Vec<u8>),
    Delete,
}

impl ConflictResolution {
    pub fn content(&self) -> Option<&[u8]> {
        match self {
            Self::Content(bytes) => Some(bytes),
            Self::Delete => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffTarget {
    /// Changes from the index to the working tree; applying a hunk stages it.
    Worktree,
    /// Changes from HEAD to the index; applying a hunk in reverse unstages it.
    Index,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLineSelection {
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    /// Includes the leading `+` or `-` diff marker.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictCommits {
    pub ours: String,
    pub theirs: String,
    pub base: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashEntry {
    pub reference: String,
    pub id: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathDiffStats {
    pub path: GitPath,
    pub staged: bool,
    pub added: usize,
    pub removed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoInfo {
    pub current_id: String,
    pub previous_id: String,
    pub depth: usize,
    /// The reflog subject at the state that will be restored.
    pub message: String,
    /// The operation represented by the current HEAD reflog entry.
    pub latest_message: String,
    /// Set when traversing backward crosses a checkout and the old branch is known.
    pub restore_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictVersions {
    /// Missing stages represent files absent from that side (e.g. deleted by us).
    pub base: Option<Vec<u8>>,
    pub ours: Option<Vec<u8>>,
    pub theirs: Option<Vec<u8>>,
    pub working: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebaseOutcome {
    Completed,
    StoppedAtCommit {
        target: String,
        parents: Vec<String>,
        title: String,
        description: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebasePlan {
    pub target: String,
    pub upstream: Option<String>,
    pub blocks: Vec<RebasePlanBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebasePlanBlock {
    Control(String),
    Commits(Vec<RebasePlanEntry>),
    Merge(RebasePlanEntry),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebasePlanEntry {
    pub id: String,
    pub subject: String,
    pub tail: String,
    pub action: RebaseTodoAction,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebaseTodoAction {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Drop,
    Merge,
}

/// Open a repository, discovering it by walking up from `path` (like `git`).
pub fn open(path: &Path) -> anyhow::Result<Box<dyn GitBackend>> {
    Ok(Box::new(gix::GixBackend::open(path)?))
}

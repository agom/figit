use super::*;
use crate::backend::AutosquashPlan;

impl GixBackend {
    pub(super) fn prepare_autosquash_impl(
        &self,
        target: &str,
        create_fixup: bool,
    ) -> Result<AutosquashPlan> {
        anyhow::ensure!(
            !rebase_in_progress(&self.workdir)?,
            "finish or abort the current rebase first"
        );
        anyhow::ensure!(
            git_output(&self.workdir, &["symbolic-ref", "--quiet", "HEAD"])?
                .status
                .success(),
            "autosquash requires a checked-out branch"
        );
        anyhow::ensure!(
            git_output(
                &self.workdir,
                &["merge-base", "--is-ancestor", target, "HEAD"]
            )?
            .status
            .success(),
            "select an ancestor of the current branch"
        );
        anyhow::ensure!(
            git_output(&self.workdir, &["diff", "--quiet"])?
                .status
                .success(),
            "stage or stash unstaged changes before autosquashing"
        );
        for marker in ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"] {
            anyhow::ensure!(
                !self.repo.git_dir().join(marker).exists(),
                "finish the current Git operation before autosquashing"
            );
        }
        let branch = git_output_checked(&self.workdir, &["symbolic-ref", "--quiet", "HEAD"])?;
        let branch = String::from_utf8_lossy(&branch.stdout).trim().to_string();
        let parents = self.commit_parents(target)?;
        anyhow::ensure!(
            parents.len() < 2,
            "select a non-merge target for autosquash"
        );
        let head = git_output_checked(&self.workdir, &["rev-parse", "HEAD"])?;
        let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
        let tree = git_output_checked(&self.workdir, &["write-tree"])?;
        let index_tree = String::from_utf8_lossy(&tree.stdout).trim().to_string();
        let staged = git_output_checked(&self.workdir, &["diff", "--cached", "--name-only", "-z"])?;
        let staged_files = staged
            .stdout
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .count();
        anyhow::ensure!(
            if create_fixup {
                staged_files > 0
            } else {
                staged_files == 0
            },
            "{}",
            if create_fixup {
                "stage changes before creating a fixup"
            } else {
                "commit or stash staged changes before autosquashing existing fixups"
            }
        );
        let subject = git_output_checked(&self.workdir, &["show", "-s", "--format=%s", target])?;
        let upstream = parents.first().cloned();
        let range = upstream
            .as_ref()
            .map_or_else(|| "HEAD".into(), |base| format!("{base}..HEAD"));
        let count = git_output_checked(&self.workdir, &["rev-list", "--count", &range])?;
        let commits = String::from_utf8_lossy(&count.stdout)
            .trim()
            .parse::<usize>()?
            + usize::from(create_fixup);
        if !create_fixup {
            let subjects = git_output_checked(&self.workdir, &["log", "--format=%s", &range])?;
            anyhow::ensure!(
                String::from_utf8_lossy(&subjects.stdout)
                    .lines()
                    .any(|s| s.starts_with("fixup! ")
                        || s.starts_with("squash! ")
                        || s.starts_with("amend! ")),
                "no fixup or squash commits exist in the selected range"
            );
        }
        let published =
            !git_output_checked(&self.workdir, &["branch", "-r", "--contains", target])?
                .stdout
                .is_empty();
        Ok(AutosquashPlan {
            branch,
            target: target.into(),
            subject: String::from_utf8_lossy(&subject.stdout).trim().into(),
            head,
            index_tree,
            upstream,
            create_fixup,
            commits,
            staged_files,
            published,
        })
    }

    pub(super) fn execute_autosquash_impl(&self, plan: &AutosquashPlan) -> Result<RebaseOutcome> {
        let fresh = self.prepare_autosquash_impl(&plan.target, plan.create_fixup)?;
        anyhow::ensure!(
            fresh.branch == plan.branch
                && fresh.head == plan.head
                && fresh.index_tree == plan.index_tree,
            "repository changed since confirmation; review the operation again"
        );
        if plan.create_fixup {
            GitBackend::commit(self, "", "", false, Some(&plan.target))?;
        }
        let mut command = Command::new("git");
        command.args([
            "rebase",
            "-i",
            "--autosquash",
            "--rebase-merges",
            "--keep-empty",
            "--no-autostash",
        ]);
        if let Some(base) = &plan.upstream {
            command.arg(base);
        } else {
            command.arg("--root");
        }
        let output = command
            .current_dir(&self.workdir)
            .env("GIT_SEQUENCE_EDITOR", "true")
            .env("GIT_EDITOR", "true")
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "autosquash stopped: {}{}",
            String::from_utf8_lossy(&output.stderr).trim(),
            if rebase_in_progress(&self.workdir)? {
                " — resolve conflicts, then continue or abort"
            } else if plan.create_fixup {
                " — the fixup commit was preserved"
            } else {
                ""
            }
        );
        Ok(RebaseOutcome::Completed)
    }
}

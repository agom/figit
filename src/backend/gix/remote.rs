use super::*;
use std::os::unix::process::CommandExt;
use std::time::{Duration, Instant};

impl GixBackend {
    fn current_head_ref(&self) -> Result<Option<String>> {
        let output = git_output(&self.workdir, &["symbolic-ref", "--quiet", "HEAD"])?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string()))
    }

    pub(super) fn pull_impl(
        &self,
        branch: &str,
        strategy: crate::backend::PullStrategy,
    ) -> Result<()> {
        use crate::backend::PullStrategy;
        anyhow::ensure!(
            self.current_head_ref()?.as_deref() == Some(&format!("refs/heads/{branch}")),
            "The checked-out branch changed. Open Pull again."
        );
        let mut args = vec!["pull", "--no-edit"];
        match strategy {
            PullStrategy::Configured => {}
            PullStrategy::FastForward => args.push("--ff-only"),
            // Override pull.ff=only for an explicitly chosen reconciliation.
            PullStrategy::Rebase => args.extend(["--rebase", "--ff"]),
            PullStrategy::Merge => args.extend(["--no-rebase", "--ff"]),
        }
        git_output_checked(&self.workdir, &args)?;
        Ok(())
    }

    pub(super) fn prepare_branch_update_impl(
        &self,
        branch: &str,
    ) -> Result<crate::backend::BranchUpdate> {
        anyhow::ensure!(
            !self.rebase_in_progress()? && !has_in_progress_git_operation(&self.workdir)?,
            "Finish or abort the current Git operation before updating a branch."
        );
        let reference = format!("refs/heads/{branch}");
        git_output_checked(&self.workdir, &["check-ref-format", &reference])?;
        anyhow::ensure!(
            !git_output(&self.workdir, &["symbolic-ref", "--quiet", &reference])?
                .status
                .success(),
            "Updating a symbolic local branch is not supported."
        );
        let old_tip = git_output_checked(&self.workdir, &["rev-parse", "--verify", &reference])?;
        let remote = self
            .setting(&format!("branch.{branch}.remote"))?
            .filter(|s| !s.is_empty())
            .with_context(|| {
                format!("Branch {branch} has no upstream. Set its tracking branch first.")
            })?;
        let source = self
            .setting(&format!("branch.{branch}.merge"))?
            .filter(|s| s.starts_with("refs/heads/") && !s.contains('\n'))
            .with_context(|| format!("Branch {branch} has no single branch upstream."))?;
        let upstream = git_output_checked(
            &self.workdir,
            &["for-each-ref", "--format=%(upstream)", &reference],
        )?;
        let head_ref = self.current_head_ref()?;
        if head_ref.as_deref() != Some(&reference) {
            let worktrees =
                git_output_checked(&self.workdir, &["worktree", "list", "--porcelain", "-z"])?;
            let field = format!("branch {reference}");
            anyhow::ensure!(
                !worktrees
                    .stdout
                    .split(|b| *b == 0)
                    .any(|part| part == field.as_bytes()),
                "Branch {branch} is checked out in another worktree. Update it from that worktree."
            );
        }
        let upstream = String::from_utf8(upstream.stdout)?;
        let upstream = upstream.trim();
        let upstream = upstream
            .strip_prefix("refs/remotes/")
            .or_else(|| upstream.strip_prefix("refs/heads/"))
            .unwrap_or(upstream);
        Ok(crate::backend::BranchUpdate {
            branch: branch.into(),
            old_tip: String::from_utf8(old_tip.stdout)?.trim().into(),
            upstream: if upstream.is_empty() {
                format!("{remote}/{}", source.trim_start_matches("refs/heads/"))
            } else {
                upstream.into()
            },
            remote,
            source,
            head_ref,
        })
    }

    pub(super) fn update_branch_impl(
        &self,
        plan: &crate::backend::BranchUpdate,
        reset: bool,
    ) -> Result<()> {
        let validate = || -> Result<()> {
            anyhow::ensure!(
                self.prepare_branch_update_impl(&plan.branch)? == *plan,
                "Branch or upstream changed since confirmation. Open the command again."
            );
            Ok(())
        };
        validate()?;
        // Fetch the configured source explicitly: renamed upstream branches and
        // custom fetch refspecs need not match the local branch's name.
        git_output_checked(
            &self.workdir,
            &[
                "fetch",
                "--no-tags",
                "--no-recurse-submodules",
                "--",
                &plan.remote,
                &plan.source,
            ],
        )?;
        let target = git_output_checked(
            &self.workdir,
            &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
        )?;
        let target = String::from_utf8(target.stdout)?.trim().to_string();
        validate()?;
        if !reset {
            let ancestor = git_output(
                &self.workdir,
                &["merge-base", "--is-ancestor", &plan.old_tip, &target],
            )?;
            anyhow::ensure!(
                ancestor.status.success(),
                "Branch {} cannot fast-forward to {}. Check it out to merge/rebase, or use Reset branch to upstream to discard local-only commits.",
                plan.branch, plan.upstream
            );
        }
        let reference = format!("refs/heads/{}", plan.branch);
        if plan.head_ref.as_deref() == Some(&reference) {
            if reset {
                git_output_checked(&self.workdir, &["reset", "--hard", &target])?;
            } else {
                git_output_checked(&self.workdir, &["merge", "--ff-only", "--no-edit", &target])?;
            }
        } else {
            // Compare-and-swap protects commits made while the network fetch ran.
            git_output_checked(
                &self.workdir,
                &[
                    "update-ref",
                    "-m",
                    "figit: update branch from upstream",
                    &reference,
                    &target,
                    &plan.old_tip,
                ],
            )?;
        }
        Ok(())
    }

    fn setting(&self, key: &str) -> Result<Option<String>> {
        let output = git_output(&self.workdir, &["config", "--get", key])?;
        if output.status.code() == Some(1) {
            return Ok(None);
        }
        anyhow::ensure!(output.status.success(), "could not read Git setting {key}");
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ))
    }

    pub(super) fn auto_fetch_interval_impl(&self) -> Result<Option<Duration>> {
        let enabled = std::env::var("FIGIT_AUTO_FETCH")
            .ok()
            .or(self.setting("figit.autoFetch")?);
        if enabled.as_deref().is_some_and(|v| {
            matches!(
                v.to_ascii_lowercase().as_str(),
                "false" | "no" | "off" | "0"
            )
        }) {
            return Ok(None);
        }
        let seconds = self
            .setting("figit.fetchInterval")?
            .map(|value| value.parse::<u64>())
            .transpose()
            .context("figit.fetchInterval must be seconds (0 disables background fetching)")?
            .unwrap_or(120);
        Ok((seconds > 0).then(|| Duration::from_secs(seconds.max(10))))
    }

    pub(super) fn fetch_background_impl(
        &self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<usize> {
        let mut fetched = 0;
        for remote in self.remotes()? {
            anyhow::ensure!(
                !cancel.load(std::sync::atomic::Ordering::Relaxed),
                "background fetch cancelled"
            );
            let skip = self.setting(&format!("remote.{remote}.skipFetchAll"))?;
            if skip.as_deref().is_some_and(|v| {
                matches!(v.to_ascii_lowercase().as_str(), "true" | "yes" | "on" | "1")
            }) {
                continue;
            }
            let ssh = std::env::var("GIT_SSH_COMMAND")
                .ok()
                .or(self.setting("core.sshCommand")?)
                .unwrap_or_else(|| "ssh".into());
            let errors = tempfile::tempfile().context("create background fetch error log")?;
            let mut child = Command::new("git")
                .args([
                    "-c",
                    "http.lowSpeedLimit=1",
                    "-c",
                    "http.lowSpeedTime=15",
                    "fetch",
                    "--prune",
                    "--quiet",
                    "--no-write-fetch-head",
                    "--no-recurse-submodules",
                    &remote,
                ])
                .current_dir(&self.workdir)
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GCM_INTERACTIVE", "Never")
                .env("GIT_ASKPASS", "false")
                .env("SSH_ASKPASS", "false")
                .env(
                    "GIT_SSH_COMMAND",
                    format!("{ssh} -oBatchMode=yes -oConnectTimeout=10"),
                )
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(errors.try_clone()?)
                .process_group(0)
                .spawn()
                .context("start background fetch")?;
            let start = Instant::now();
            let status = loop {
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                if start.elapsed() > Duration::from_secs(45)
                    || cancel.load(std::sync::atomic::Ordering::Relaxed)
                {
                    // Each fetch owns its process group, including SSH and helpers.
                    unsafe {
                        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!("background fetch of {remote} timed out");
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            if !status.success() {
                use std::io::{Read, Seek, SeekFrom};
                let mut errors = errors;
                errors.seek(SeekFrom::Start(0))?;
                let mut message = String::new();
                errors.take(8192).read_to_string(&mut message)?;
                anyhow::bail!("{remote}: {}", message.trim());
            }
            fetched += 1;
        }
        Ok(fetched)
    }

    pub(super) fn remote_branch_tip_impl(
        &self,
        remote: &str,
        branch: &str,
    ) -> Result<Option<String>> {
        let source = format!("refs/heads/{branch}");
        let refspecs = git_output(
            &self.workdir,
            &["config", "--get-all", &format!("remote.{remote}.fetch")],
        )?;
        for spec in String::from_utf8_lossy(&refspecs.stdout).lines() {
            let Some((from, to)) = spec.trim_start_matches('+').split_once(':') else {
                continue;
            };
            let destination = if from == source {
                Some(to.to_string())
            } else if let (Some((prefix, suffix)), Some((left, right))) =
                (from.split_once('*'), to.split_once('*'))
            {
                source
                    .strip_prefix(prefix)
                    .and_then(|s| s.strip_suffix(suffix))
                    .map(|s| format!("{left}{s}{right}"))
            } else {
                None
            };
            if let Some(reference) = destination {
                let output = git_output(&self.workdir, &["rev-parse", "--verify", &reference])?;
                return Ok(output
                    .status
                    .success()
                    .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string()));
            }
        }
        Ok(None)
    }

    /// Establish tracking on the first successful push, including an
    /// up-to-date push that repairs a previously published untracked branch.
    /// An explicit push to another remote must preserve existing tracking.
    pub(super) fn push_branch_impl(
        &self,
        remote: &str,
        branch: &str,
        option: Option<&str>,
    ) -> Result<()> {
        let configured = |value: Option<String>| value.is_some_and(|value| !value.is_empty());
        let has_upstream = configured(self.setting(&format!("branch.{branch}.remote"))?)
            && configured(self.setting(&format!("branch.{branch}.merge"))?);
        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
        let mut args = vec!["push"];
        if let Some(option) = option {
            args.push(option);
        }
        if !has_upstream {
            args.push("--set-upstream");
        }
        args.extend([remote, refspec.as_str()]);
        git_output_checked(&self.workdir, &args)?;
        Ok(())
    }

    pub(super) fn push_with_lease_impl(
        &self,
        remote: &str,
        branch: &str,
        expected: Option<&str>,
    ) -> Result<()> {
        let lease = format!(
            "--force-with-lease=refs/heads/{branch}:{}",
            expected.unwrap_or("")
        );
        self.push_branch_impl(remote, branch, Some(&lease))
    }
}

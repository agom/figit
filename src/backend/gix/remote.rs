use super::*;
use std::os::unix::process::CommandExt;
use std::time::{Duration, Instant};

impl GixBackend {
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

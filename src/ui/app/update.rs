//! Update checks, installation state and check-cache policy.

use axoupdater::AxoUpdater;

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UpdateAvailability {
    Available,
    Current,
    UnsupportedInstall,
    SkippedRecentlyChecked,
}

pub(super) enum UpdateEvent {
    Check {
        manual: bool,
        result: std::result::Result<UpdateAvailability, String>,
    },
    Install(std::result::Result<bool, String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UpdateStatus {
    Idle,
    Checking,
    Available,
    Installing,
    RestartRequired,
    Current,
    UnsupportedInstall,
    Failed(String),
}

pub(super) fn update_check_cache_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .map(|cache| cache.join("figit/update-check"))
}

pub(super) fn update_check_is_recent() -> bool {
    let Some(path) = update_check_cache_path() else {
        return false;
    };
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(previous) = contents.trim().parse::<u64>() else {
        return false;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(previous, |duration| duration.as_secs());
    now.saturating_sub(previous) < Duration::from_secs(24 * 60 * 60).as_secs()
}

pub(super) fn record_update_check() {
    let Some(path) = update_check_cache_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return;
    };
    if std::fs::create_dir_all(parent).is_ok() {
        let _ = std::fs::write(path, now.as_secs().to_string());
    }
}

impl App {
    pub(super) fn start_update_check(&mut self, manual: bool) {
        if matches!(
            self.update_status,
            UpdateStatus::Checking | UpdateStatus::Installing
        ) {
            return;
        }
        self.update_status = UpdateStatus::Checking;
        self.update_status_since = Some(Instant::now());
        let sender = self.update_tx.clone();
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<UpdateAvailability> {
                if !manual && update_check_is_recent() {
                    return Ok(UpdateAvailability::SkippedRecentlyChecked);
                }
                let mut updater = AxoUpdater::new_for(env!("CARGO_PKG_NAME"));
                // Only cargo-dist installations have a receipt that identifies
                // their release source and install path. Leave source builds
                // and package-manager installs to their own update mechanism.
                if updater.load_receipt().is_err() {
                    return Ok(UpdateAvailability::UnsupportedInstall);
                }
                record_update_check();
                if updater.is_update_needed_sync()? {
                    Ok(UpdateAvailability::Available)
                } else {
                    Ok(UpdateAvailability::Current)
                }
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(UpdateEvent::Check { manual, result });
        });
    }

    pub(super) fn install_update(&mut self) {
        if self.update_status != UpdateStatus::Available {
            return;
        }
        self.update_status = UpdateStatus::Installing;
        self.update_status_since = Some(Instant::now());
        let sender = self.update_tx.clone();
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<bool> {
                let mut updater = AxoUpdater::new_for(env!("CARGO_PKG_NAME"));
                updater.load_receipt()?;
                // Keep the installer subprocess from writing into the TUI's
                // alternate screen; progress is reported in the status bar.
                updater.disable_installer_output();
                Ok(updater.run_sync()?.is_some())
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(UpdateEvent::Install(result));
        });
    }

    pub(super) fn poll_update(&mut self) {
        while let Ok(event) = self.update_rx.try_recv() {
            self.redraw = true;
            match event {
                UpdateEvent::Check { manual, result } => {
                    self.update_status = match result {
                        Ok(UpdateAvailability::Available) => UpdateStatus::Available,
                        Ok(UpdateAvailability::Current) if manual => UpdateStatus::Current,
                        Ok(UpdateAvailability::UnsupportedInstall) if manual => {
                            UpdateStatus::UnsupportedInstall
                        }
                        Ok(UpdateAvailability::SkippedRecentlyChecked) => UpdateStatus::Idle,
                        Ok(_) => UpdateStatus::Idle,
                        Err(error) if manual => UpdateStatus::Failed(error),
                        Err(_) => UpdateStatus::Idle,
                    };
                    self.update_status_since = Some(Instant::now());
                }
                UpdateEvent::Install(Ok(true)) => {
                    self.update_status = UpdateStatus::RestartRequired;
                    self.update_status_since = Some(Instant::now());
                }
                UpdateEvent::Install(Ok(false)) => {
                    self.update_status = UpdateStatus::Current;
                    self.update_status_since = Some(Instant::now());
                }
                UpdateEvent::Install(Err(error)) => {
                    self.update_status = UpdateStatus::Failed(error);
                    self.update_status_since = Some(Instant::now());
                }
            }
        }

        let transient = matches!(
            self.update_status,
            UpdateStatus::Current | UpdateStatus::UnsupportedInstall | UpdateStatus::Failed(_)
        );
        if transient
            && self
                .update_status_since
                .is_some_and(|since| since.elapsed() > Duration::from_secs(8))
        {
            self.redraw = true;
            self.update_status = UpdateStatus::Idle;
            self.update_status_since = None;
        }
    }
}

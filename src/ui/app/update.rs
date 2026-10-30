//! Update checks, installation state and check-cache policy.

use axoupdater::AxoUpdater;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UpdateAvailability {
    Available(String),
    Current,
    UnsupportedInstall,
}

pub(super) const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

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
    Available(String),
    Installing,
    RestartRequired,
    Current,
    UnsupportedInstall,
    Failed(String),
}

impl App {
    pub(super) fn start_update_check(&mut self, manual: bool) {
        if matches!(
            self.update_status,
            UpdateStatus::Checking | UpdateStatus::Installing
        ) {
            return;
        }
        self.update_check_deadline = Some(Instant::now() + UPDATE_CHECK_INTERVAL);
        self.update_status = UpdateStatus::Checking;
        self.update_status_since = Some(Instant::now());
        let sender = self.update_tx.clone();
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<UpdateAvailability> {
                let mut updater = AxoUpdater::new_for(env!("CARGO_PKG_NAME"));
                // Only cargo-dist installations have a receipt that identifies
                // their release source and install path. Leave source builds
                // and package-manager installs to their own update mechanism.
                if updater.load_receipt().is_err() {
                    return Ok(UpdateAvailability::UnsupportedInstall);
                }
                if !updater.check_receipt_is_for_this_executable()? {
                    return Ok(UpdateAvailability::UnsupportedInstall);
                }
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                let (needed, version) = runtime.block_on(async {
                    let version = updater.query_new_version().await?.map(ToString::to_string);
                    let needed = updater.is_update_needed().await?;
                    anyhow::Ok((needed, version))
                })?;
                if needed {
                    Ok(UpdateAvailability::Available(
                        version.unwrap_or_else(|| "newer version".to_string()),
                    ))
                } else {
                    Ok(UpdateAvailability::Current)
                }
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(UpdateEvent::Check { manual, result });
        });
    }

    pub(super) fn install_update(&mut self) {
        if !matches!(self.update_status, UpdateStatus::Available(_)) {
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
                        Ok(UpdateAvailability::Available(version)) => {
                            UpdateStatus::Available(version)
                        }
                        Ok(UpdateAvailability::Current) if manual => UpdateStatus::Current,
                        Ok(UpdateAvailability::UnsupportedInstall) if manual => {
                            UpdateStatus::UnsupportedInstall
                        }
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

//! Quiet, periodic remote refresh without changing panel focus.

use super::*;

pub(super) struct AutoFetchCompletion {
    pub interval: Option<Duration>,
    pub result: std::result::Result<usize, String>,
}

pub(super) struct AutoFetchState {
    pub deadline: Option<Instant>,
    pub interval: Duration,
    pub(super) worker: Option<Receiver<AutoFetchCompletion>>,
    pub last_success: Option<Instant>,
    pub last_error: Option<String>,
    label: String,
    pub disabled: bool,
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Default for AutoFetchState {
    fn default() -> Self {
        Self {
            deadline: None,
            interval: Duration::from_secs(120),
            worker: None,
            last_success: None,
            last_error: None,
            label: String::new(),
            disabled: false,
            cancel: Default::default(),
        }
    }
}
impl AutoFetchState {
    pub(super) fn footer(&self) -> String {
        if self.worker.is_some() {
            "Fetching…".into()
        } else if self.last_error.is_some() {
            "Auto-fetch failed (: details)".into()
        } else if let Some(since) = self.last_success {
            let minutes = since.elapsed().as_secs() / 60;
            if minutes == 0 {
                "Fetched just now".into()
            } else {
                format!("Fetched {minutes}m ago")
            }
        } else {
            String::new()
        }
    }
    pub(super) fn status_detail(&self) -> String {
        if let Some(error) = &self.last_error {
            format!("Background fetch: {error}\nManual Fetch can prompt for credentials.")
        } else if self.disabled {
            "Background fetching is disabled (figit.autoFetch / figit.fetchInterval).".into()
        } else {
            format!("{}\nBackground fetch interval: {} seconds. Configure with git config figit.fetchInterval <seconds> or git config figit.autoFetch false.", self.footer(), self.interval.as_secs())
        }
    }
}
impl App {
    pub(super) fn poll_auto_fetch(&mut self) {
        if let Some(receiver) = self.auto_fetch.worker.as_ref() {
            match receiver.try_recv() {
                Ok(AutoFetchCompletion { interval, result }) => {
                    self.auto_fetch.worker = None;
                    self.auto_fetch.disabled = interval.is_none();
                    self.auto_fetch.deadline = interval.map(|interval| Instant::now() + interval);
                    if let Some(interval) = interval {
                        self.auto_fetch.interval = interval;
                    }
                    match result {
                        Ok(count) => {
                            self.auto_fetch.last_error = None;
                            if count > 0 {
                                self.auto_fetch.last_success = Some(Instant::now());
                                self.refresh.deadline = Some(Instant::now());
                                self.refresh.preserve_scroll = true;
                            }
                        }
                        Err(error)
                            if !self
                                .auto_fetch
                                .cancel
                                .load(std::sync::atomic::Ordering::Relaxed) =>
                        {
                            self.auto_fetch.last_error = Some(error);
                            self.refresh.deadline = Some(Instant::now());
                            self.refresh.preserve_scroll = true;
                        }
                        Err(_) => {}
                    }
                    self.redraw = true;
                }
                Err(TryRecvError::Disconnected) => {
                    self.auto_fetch.worker = None;
                    self.auto_fetch.deadline = Some(Instant::now() + self.auto_fetch.interval);
                    self.auto_fetch.last_error = Some("fetch worker stopped".into());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        let label = self.auto_fetch.footer();
        if label != self.auto_fetch.label {
            self.auto_fetch.label = label;
            self.redraw = true;
        }
        if self.loading
            || self.git_busy()
            || self.refresh.in_progress
            || !self.modal_closed()
            || self.rebase_active
            || !self.active_conflict_paths.is_empty()
            || self.auto_fetch.worker.is_some()
            || !self
                .auto_fetch
                .deadline
                .is_some_and(|time| Instant::now() >= time)
        {
            return;
        }
        let Some(path) = self.repository_path.clone() else {
            return;
        };
        let (sender, receiver) = mpsc::channel();
        self.auto_fetch.worker = Some(receiver);
        self.auto_fetch.deadline = None;
        self.auto_fetch.cancel = Default::default();
        let cancel = self.auto_fetch.cancel.clone();
        let gate = self.git_gate.clone();
        thread::spawn(move || {
            let _guard = gate.lock().unwrap_or_else(|e| e.into_inner());
            let result = (|| -> anyhow::Result<_> {
                let backend = crate::backend::open(&path)?;
                let interval = backend.auto_fetch_interval()?;
                let result = if interval.is_some() {
                    backend
                        .fetch_background(&cancel)
                        .map_err(|e| format!("{e:#}"))
                } else {
                    Ok(0)
                };
                Ok(AutoFetchCompletion { interval, result })
            })();
            let completion = result.unwrap_or_else(|error| AutoFetchCompletion {
                interval: Some(Duration::from_secs(120)),
                result: Err(format!("{error:#}")),
            });
            let _ = sender.send(completion);
        });
    }
}

impl App {
    pub(super) fn stop_auto_fetch(&mut self) {
        self.auto_fetch
            .cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(receiver) = self.auto_fetch.worker.take() {
            let _ = receiver.recv_timeout(Duration::from_millis(200));
        }
    }
}

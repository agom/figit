use super::*;

pub(super) const POLL_INTERVAL: Duration = Duration::from_secs(5);

pub(super) type LiveWatcher = Box<dyn Watcher + Send>;
type WatcherResult = std::result::Result<LiveWatcher, String>;
pub(super) type WatcherSetup = Receiver<WatcherResult>;

impl App {
    pub(super) fn start_repository_watcher(&mut self) {
        let Some(path) = self.repository_path.clone() else {
            return;
        };
        let events = self.watch_tx.clone();
        self.watch_setup = Some(spawn_setup(move || create_watcher(&path, events)));
    }

    pub(super) fn poll_repository_watcher(&mut self) {
        let result = match self.watch_setup.as_ref().map(Receiver::try_recv) {
            None | Some(Err(TryRecvError::Empty)) => return,
            Some(Ok(result)) => result,
            Some(Err(TryRecvError::Disconnected)) => {
                Err("file watcher setup ended unexpectedly".to_string())
            }
        };
        self.redraw = true;
        self.watch_setup = None;
        match result {
            Ok(watcher) => {
                self.repository_watcher = Some(watcher);
                self.watcher_error = None;
                self.watcher_poll_deadline = None;
                // The first snapshot may predate registration. Rescan once to
                // catch writes made before their directories were watched.
                self.refresh.deadline = Some(Instant::now());
                crate::profile::mark("watcher ready");
            }
            Err(error) => {
                self.watcher_error = Some(error);
                // Watch limits and unsupported filesystems must not disable
                // automatic refresh. Reuse the background snapshot worker;
                // polling never recursively registers filesystem watches.
                self.watcher_poll_deadline = Some(Instant::now() + POLL_INTERVAL);
                self.refresh.deadline = Some(Instant::now());
            }
        }
    }

    /// Some native filesystem watchers coalesce quick writes to the same file.
    /// A cheap metadata check of the visible diff catches those saves without
    /// scanning the repository or rebuilding history on every tick.
    pub(super) fn poll_visible_file_metadata(&mut self) {
        let now = Instant::now();
        if !self
            .visible_file_poll_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            return;
        }
        self.visible_file_poll_deadline = Some(now + Duration::from_millis(500));
        if self.historical_commit.is_some() || self.three_way_view.is_some() {
            return;
        }
        let current = self.diff_path.as_ref().map(|path| {
            let fingerprint = self.backend.as_ref().and_then(|backend| {
                let relative = path.to_path_buf().ok()?;
                let metadata =
                    std::fs::metadata(backend.working_directory().join(relative)).ok()?;
                Some((metadata.modified().ok()?, metadata.len()))
            });
            (path.clone(), fingerprint)
        });
        if current != self.visible_file_fingerprint {
            self.visible_file_fingerprint = current;
            self.refresh.deadline = Some(now);
        }
    }
}

pub(super) fn spawn_setup<F>(setup: F) -> WatcherSetup
where
    F: FnOnce() -> WatcherResult + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        crate::profile::mark("watcher setup started");
        let result = setup();
        // If the app quit during registration, sending fails and drops the
        // watcher here. Exiting never waits for this worker to finish.
        let _ = sender.send(result);
    });
    receiver
}

pub(super) fn create_watcher(path: &Path, events: SyncSender<()>) -> WatcherResult {
    let backend = crate::backend::open(path).map_err(|error| error.to_string())?;
    let paths = backend.watch_paths().map_err(|error| error.to_string())?;
    let paths = paths
        .into_iter()
        .map(|path| path.canonicalize().unwrap_or(path))
        .collect::<Vec<_>>();
    let mut watcher = new_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        // Atomic Git updates first create a .lock file and then rename it
        // over the final ref or index. Treat create/remove notifications
        // as refresh signals too; the normal debounce lets the write land
        // before we read repository state.
        if !watch_event_changes_state(&event) {
            return;
        }
        match events.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) | Err(TrySendError::Disconnected(())) => {}
        }
    })
    .map_err(|error| error.to_string())?;
    let roots = watch_roots(paths);
    for path in roots {
        watcher
            .watch(&path, RecursiveMode::Recursive)
            .map_err(|error| format!("{}: {error}", path.display()))?;
    }
    Ok(watcher)
}

fn new_watcher<F>(handler: F) -> notify::Result<LiveWatcher>
where
    F: FnMut(notify::Result<notify::Event>) + Send + 'static,
{
    #[cfg(target_os = "macos")]
    {
        // FSEvents does not consistently report changes made under .git on
        // temporary and network-backed volumes. PollWatcher provides the same
        // debounced event stream there, while Linux and Windows keep native
        // notifications.
        PollWatcher::new(
            handler,
            NotifyConfig::default()
                .with_poll_interval(Duration::from_secs(2))
                .with_follow_symlinks(false),
        )
        .map(|watcher| Box::new(watcher) as LiveWatcher)
    }
    #[cfg(not(target_os = "macos"))]
    {
        RecommendedWatcher::new(handler, NotifyConfig::default().with_follow_symlinks(false))
            .map(|watcher| Box::new(watcher) as LiveWatcher)
    }
}

/// A recursive parent already covers its descendants. Linked-worktree Git
/// directories outside the working tree remain separate roots.
pub(super) fn watch_roots(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then(left.cmp(right))
    });
    let mut roots = Vec::<PathBuf>::new();
    for path in paths {
        // Watch Git metadata as an independent root even when it sits inside
        // the worktree. Some native watcher backends do not reliably report
        // changes from hidden directories through a recursive parent watch.
        #[cfg(not(target_os = "macos"))]
        if path.file_name().is_some_and(|name| name == ".git") {
            if !roots.contains(&path) {
                roots.push(path);
            }
            continue;
        }
        if !roots.iter().any(|root| path.starts_with(root)) {
            roots.push(path);
        }
    }
    roots
}

/// Access notifications are noisy; all other paths, including lock files,
/// can be state changes. Git's atomic lock-and-rename sequence is debounced by
/// the refresh loop before it reads the repository.
pub(super) fn watch_event_changes_state(event: &notify::Event) -> bool {
    !matches!(event.kind, notify::EventKind::Access(_))
}

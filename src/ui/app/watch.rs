use super::*;

type WatcherResult = std::result::Result<RecommendedWatcher, String>;
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
        self.watch_setup = None;
        match result {
            Ok(watcher) => {
                self.repository_watcher = Some(watcher);
                self.watcher_error = None;
                // The first snapshot may predate registration. Rescan once to
                // catch writes made before their directories were watched.
                self.refresh_deadline = Some(Instant::now());
                crate::profile::mark("watcher ready");
            }
            Err(error) => self.watcher_error = Some(error),
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
    let git_dir = paths
        .iter()
        .find(|path| path.file_name().is_some_and(|name| name == ".git"))
        .cloned();
    let mut watcher = RecommendedWatcher::new(
        move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            if matches!(event.kind, notify::EventKind::Access(_)) {
                return;
            }
            if !event.paths.is_empty()
                && event
                    .paths
                    .iter()
                    .all(|path| is_ignored_watch_path(path, git_dir.as_deref()))
            {
                return;
            }
            match events.try_send(()) {
                Ok(()) | Err(TrySendError::Full(())) | Err(TrySendError::Disconnected(())) => {}
            }
        },
        NotifyConfig::default().with_follow_symlinks(false),
    )
    .map_err(|error| error.to_string())?;
    for path in watch_roots(paths) {
        watcher
            .watch(&path, RecursiveMode::Recursive)
            .map_err(|error| format!("{}: {error}", path.display()))?;
    }
    Ok(watcher)
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
        if !roots.iter().any(|root| path.starts_with(root)) {
            roots.push(path);
        }
    }
    roots
}

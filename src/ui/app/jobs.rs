use super::*;

// Only owned requests/results cross threads. Each worker opens its own backend;
// gix repositories and the UI itself never cross the thread boundary.
type Completion = Box<dyn FnOnce(&mut App) + Send>;

pub(super) struct LocalJob {
    pub label: &'static str,
    pub started: Instant,
    receiver: Receiver<Completion>,
    return_focus: Focus,
}

pub(super) struct JobResult<T> {
    pub result: std::result::Result<T, String>,
    snapshot: std::result::Result<RepositorySnapshot, String>,
}

impl<T> JobResult<T> {
    /// Snapshot failures must not report a successful mutation as a failed
    /// mutation: retrying a commit/rebase could otherwise repeat the action.
    pub fn refresh(self, app: &mut App) {
        match self.snapshot {
            Ok(snapshot) => app.reload_from_snapshot(snapshot),
            Err(error) => {
                app.commit_dialog.set_open(false);
                app.commit_dialog.set_confirming(false);
                app.graph_action = None;
                app.rebase_plan = None;
                app.abort_rebase_popup = false;
                app.focus = Focus::Graph;
                app.refresh.deadline = Some(Instant::now());
                app.show_git_error(format!(
                    "Git operation finished, but refreshing failed: {error}"
                ));
            }
        }
    }
}

impl App {
    pub(super) fn git_busy(&self) -> bool {
        self.remote_operation.is_some() || self.local_job.is_some()
    }

    pub(super) fn start_local_job<T, F, C>(&mut self, label: &'static str, job: F, complete: C)
    where
        T: Send + 'static,
        F: FnOnce(&dyn GitBackend) -> anyhow::Result<T> + Send + 'static,
        C: FnOnce(&mut App, JobResult<T>) + Send + 'static,
    {
        if self.git_busy() {
            return;
        }
        let Some(path) = self.repository_path.clone() else {
            return;
        };
        let (sender, receiver) = mpsc::channel::<Completion>();
        self.note_internal_change();
        self.local_job = Some(LocalJob {
            label,
            started: Instant::now(),
            receiver,
            return_focus: self.focus,
        });
        if self.focus == Focus::Commit {
            self.focus = Focus::Graph;
        }
        self.action_error = None;
        self.redraw = true;
        thread::spawn(move || {
            let result = crate::backend::open(&path)
                .and_then(|backend| job(backend.as_ref()))
                .map_err(|error| format!("{error:#}"));
            let snapshot = load_repository_snapshot(&path).map_err(|error| format!("{error:#}"));
            let _ = sender.send(Box::new(move |app| {
                complete(app, JobResult { result, snapshot })
            }));
        });
    }

    pub(super) fn poll_local_job(&mut self) {
        let completion = match self.local_job.as_ref().map(|job| job.receiver.try_recv()) {
            None | Some(Err(TryRecvError::Empty)) => return,
            Some(Ok(completion)) => completion,
            Some(Err(TryRecvError::Disconnected)) => {
                self.local_job = None;
                self.redraw = true;
                self.show_git_error(
                    "Git worker stopped unexpectedly; check repository state before retrying"
                        .into(),
                );
                return;
            }
        };
        if let Some(job) = self.local_job.take() {
            self.focus = job.return_focus;
        }
        self.redraw = true;
        completion(self);
    }
}

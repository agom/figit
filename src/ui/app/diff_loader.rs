//! One background reader with a latest-request mailbox for file navigation.

use std::sync::{Arc, Mutex};

use super::*;

pub(super) struct DiffRequest {
    pub generation: u64,
    pub repository: PathBuf,
    pub entry: StatusEntry,
    pub target: DiffTarget,
    pub commit: Option<String>,
    pub full_file_context: bool,
}

pub(super) struct DiffCompletion {
    pub request: DiffRequest,
    pub result: std::result::Result<(DiffPresentation, usize), String>,
}

pub(super) struct DiffLoader {
    pending: Arc<Mutex<Option<DiffRequest>>>,
    completed: Arc<Mutex<Option<DiffCompletion>>>,
    wake: SyncSender<()>,
}

impl DiffLoader {
    pub fn new() -> Self {
        Self::with_reader(|request| {
            (|| -> anyhow::Result<_> {
                let backend = crate::backend::open(&request.repository)?;
                let diff = match request.commit.as_deref() {
                    Some(commit) => backend.commit_diff_presentation(
                        commit,
                        &request.entry.path,
                        request.full_file_context,
                    )?,
                    None => backend.diff_entry_presentation(
                        &request.entry,
                        request.target,
                        request.full_file_context,
                    )?,
                };
                let width = backend
                    .file_line_number_width(&request.entry.path, request.commit.as_deref())
                    .unwrap_or(3);
                Ok((diff, width))
            })()
            .map_err(|error| format!("{error:#}"))
        })
    }

    pub(super) fn with_reader<F>(read: F) -> Self
    where
        F: Fn(&DiffRequest) -> std::result::Result<(DiffPresentation, usize), String>
            + Send
            + 'static,
    {
        let pending = Arc::new(Mutex::new(None::<DiffRequest>));
        let completed = Arc::new(Mutex::new(None));
        let (wake, receiver) = mpsc::sync_channel(1);
        let requests = pending.clone();
        let results = completed.clone();
        thread::spawn(move || {
            while receiver.recv().is_ok() {
                let request = requests.lock().unwrap_or_else(|e| e.into_inner()).take();
                let Some(request) = request else {
                    continue;
                };
                let result = read(&request);
                *results.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(DiffCompletion { request, result });
            }
        });
        Self {
            pending,
            completed,
            wake,
        }
    }

    pub fn request(&self, request: DiffRequest) {
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(request);
        let _ = self.wake.try_send(());
    }

    pub fn take_completed(&self) -> Option<DiffCompletion> {
        self.completed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

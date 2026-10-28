//! Remote selection and asynchronous fetch, pull and push operations.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PushTarget {
    Branch(String),
    Tag(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RemoteOperation {
    Fetch(String),
    Pull,
    Push {
        mode: usize,
        remote: String,
        target: PushTarget,
    },
}

impl RemoteOperation {
    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::Fetch(_) => "Fetch",
            Self::Pull => "Pull",
            Self::Push { .. } => "Push",
        }
    }
}

pub(super) struct RemoteCompletion {
    pub(super) operation: RemoteOperation,
    pub(super) result: std::result::Result<RepositorySnapshot, String>,
    pub(super) selection: GraphSelectionSnapshot,
}

impl App {
    pub(super) fn open_remote_action(&mut self, for_push: bool) {
        self.pending_fixup = false;
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        let push_target = if for_push {
            if let Some(branch) = self.selected_local_branch() {
                Some(PushTarget::Branch(branch))
            } else if let Some(tag) = self.selected_tag() {
                Some(PushTarget::Tag(tag))
            } else {
                self.show_git_error(
                    "Select a commit with a local branch or tag before pushing".into(),
                );
                return;
            }
        } else {
            None
        };
        let remotes = match backend.remotes() {
            Ok(remotes) if !remotes.is_empty() => remotes,
            Ok(_) => {
                self.show_git_error("No Git remotes are configured for this repository".into());
                return;
            }
            Err(error) => {
                self.show_git_error(error.to_string());
                return;
            }
        };
        let default_remote = match push_target.as_ref() {
            Some(PushTarget::Branch(branch)) => backend
                .default_remote_for_branch(branch, true)
                .ok()
                .flatten(),
            Some(PushTarget::Tag(_)) => backend.default_remote(true).ok().flatten(),
            None => backend.default_remote(false).ok().flatten(),
        };
        self.remote_selection = default_remote
            .as_ref()
            .and_then(|remote| remotes.iter().position(|name| name == remote))
            .unwrap_or(0);
        if remotes.len() == 1 {
            let remote = remotes[0].clone();
            if for_push {
                self.push_target = push_target;
                self.open_push_mode(remote);
            } else {
                self.start_remote_operation(RemoteOperation::Fetch(remote));
            }
            return;
        }
        self.push_target = push_target;
        self.remote_choices = remotes;
        self.graph_action = Some(if for_push {
            GraphAction::PushRemote
        } else {
            GraphAction::Fetch
        });
        self.action_error = None;
        self.focus = Focus::Commit;
    }

    pub(super) fn open_push_mode(&mut self, remote: String) {
        self.push_remote = Some(remote);
        if self.push_target.is_none() {
            self.push_target = self
                .selected_local_branch()
                .map(PushTarget::Branch)
                .or_else(|| self.selected_tag().map(PushTarget::Tag));
        }
        self.open_graph_action(GraphAction::Push);
    }

    pub(super) fn move_remote_selection(&mut self, delta: i32) {
        if self.remote_choices.is_empty() {
            return;
        }
        self.remote_selection = if delta < 0 {
            (self.remote_selection + self.remote_choices.len() - 1) % self.remote_choices.len()
        } else {
            (self.remote_selection + 1) % self.remote_choices.len()
        };
    }

    pub(super) fn start_remote_operation(&mut self, operation: RemoteOperation) {
        if self.git_busy() {
            return;
        }
        let Some(repository_path) = self.repository_path.clone() else {
            self.action_error =
                Some("Cannot resolve the repository path for the remote operation".into());
            return;
        };
        let sender = self.remote_tx.clone();
        let selection = GraphSelectionSnapshot {
            commit_id: self.selected_commit_id(),
            row: self.selected_graph,
            had_local_row: !self.local_status.is_empty(),
            selected_local_row: !self.local_status.is_empty() && self.selected_graph == 0,
        };
        self.remote_operation = Some(operation.clone());
        self.remote_started = Some(Instant::now());
        self.remote_notice = None;
        self.action_error = None;
        self.graph_action = None;
        self.focus = Focus::Graph;

        let repository_path_for_load = repository_path.clone();
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<_> {
                let backend = crate::backend::open(&repository_path)?;
                match &operation {
                    RemoteOperation::Fetch(remote) => backend.fetch(remote)?,
                    RemoteOperation::Pull => backend.pull()?,
                    RemoteOperation::Push {
                        mode,
                        remote,
                        target,
                    } => match target {
                        PushTarget::Branch(branch) => backend.push(remote, branch, *mode)?,
                        PushTarget::Tag(tag) => backend.push_tag(remote, tag, *mode)?,
                    },
                }
                drop(backend);
                load_repository_snapshot(&repository_path_for_load)
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(RemoteCompletion {
                operation,
                result,
                selection,
            });
        });
    }

    pub(super) fn poll_remote_operation(&mut self) {
        loop {
            match self.remote_rx.try_recv() {
                Ok(completion) => {
                    self.redraw = true;
                    self.remote_operation = None;
                    self.remote_started = None;
                    if matches!(&completion.operation, RemoteOperation::Push { .. }) {
                        self.push_remote = None;
                        self.push_target = None;
                    }
                    match completion.result {
                        Ok(snapshot) => {
                            self.reload_from_snapshot(snapshot);
                            self.restore_graph_selection(completion.selection);
                            self.remote_notice = Some((
                                format!("{} complete", completion.operation.label()),
                                Instant::now(),
                            ));
                        }
                        Err(error) => {
                            self.remote_notice = None;
                            self.show_git_error(format!(
                                "{} failed: {error}",
                                completion.operation.label()
                            ));
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.remote_operation = None;
                    self.remote_started = None;
                    self.show_git_error("Remote operation worker stopped unexpectedly".into());
                    break;
                }
            }
        }
        if self
            .remote_notice
            .as_ref()
            .is_some_and(|(_, since)| since.elapsed() > Duration::from_secs(3))
        {
            self.remote_notice = None;
            self.redraw = true;
        }
    }
}

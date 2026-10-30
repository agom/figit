use super::*;

/// Debounces repository changes and rejects snapshots started before a mutation.
pub(super) struct RefreshState {
    pub sender: Sender<RepositoryRefreshCompletion>,
    pub receiver: Receiver<RepositoryRefreshCompletion>,
    pub deadline: Option<Instant>,
    pub in_progress: bool,
    /// Invalidates results started before an internal repository mutation.
    pub generation: u64,
}

impl RefreshState {
    pub fn new(
        sender: Sender<RepositoryRefreshCompletion>,
        receiver: Receiver<RepositoryRefreshCompletion>,
    ) -> Self {
        Self {
            sender,
            receiver,
            deadline: None,
            in_progress: false,
            generation: 0,
        }
    }
}

impl App {
    pub(super) fn poll_repository_refresh(&mut self) {
        let now = Instant::now();
        if self
            .watcher_poll_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.watcher_poll_deadline = Some(now + watch::POLL_INTERVAL);
            // Keep at most one pending refresh; existing guards prevent
            // overlapping workers and defer refreshes during editing flows.
            self.refresh.deadline = Some(
                self.refresh
                    .deadline
                    .map_or(now, |pending| pending.min(now)),
            );
        }
        let mut watched = false;
        while self.watch_rx.try_recv().is_ok() {
            watched = true;
        }
        if watched {
            // Events produced by our own git writes are coalesced into one
            // refresh once the quiet window ends; external changes that land
            // inside the window are deferred rather than dropped.
            let delay = match self.ignore_watch_until {
                Some(until) if Instant::now() < until => {
                    until.saturating_duration_since(Instant::now()) + Duration::from_millis(50)
                }
                _ => Duration::from_millis(300),
            };
            let deadline = Instant::now() + delay;
            // Coalesce into the earliest pending refresh. In particular, a
            // watcher-ready catch-up must not be delayed by a stream of events.
            self.refresh.deadline = Some(
                self.refresh
                    .deadline
                    .map_or(deadline, |pending| pending.min(deadline)),
            );
        }

        while let Ok(completion) = self.refresh.receiver.try_recv() {
            self.redraw = true;
            let initial_load = self.loading;
            self.refresh.in_progress = false;
            if initial_load {
                self.loading = false;
                self.loading_started = None;
            }
            // A refresh that started before the app's own latest mutation would
            // report stale status (e.g. a file the app just staged still shows
            // as conflicted). Drop it instead of clobbering the fresh state.
            if completion.generation != self.refresh.generation {
                continue;
            }
            match completion.result {
                Ok(snapshot) => {
                    self.watch_error = None;
                    let choose_initial_focus = initial_load;
                    let initial_status_has_changes =
                        choose_initial_focus && !snapshot.status.is_empty();
                    self.reload_external_snapshot(snapshot);
                    if choose_initial_focus {
                        self.focus = if initial_status_has_changes {
                            if self.three_way_view.is_some() {
                                Focus::Diff
                            } else {
                                Focus::Status
                            }
                        } else {
                            Focus::Graph
                        };
                        if !initial_status_has_changes {
                            self.select_head_commit();
                        }
                        crate::profile::mark("initial snapshot applied");
                    }
                }
                Err(error) if initial_load => {
                    self.loading = false;
                    self.loading_started = None;
                    self.show_git_error(error);
                }
                Err(error) => self.watch_error = Some(error),
            }
        }

        let ready = self
            .refresh
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline);
        if !ready || self.refresh.in_progress || self.git_busy() {
            return;
        }
        if self.graph_action.is_some()
            || self.commit_dialog.visible()
            || self.abort_rebase_popup
            || self.rebase_plan.is_some()
            || self.error_popup.is_some()
            || self.stash_popup.is_some()
            || self.command_picker.is_some()
        {
            return;
        }
        self.refresh.deadline = None;
        let Some(path) = self.repository_path.clone() else {
            return;
        };
        let sender = self.refresh.sender.clone();
        self.refresh.in_progress = true;
        let generation = self.refresh.generation;
        thread::spawn(move || {
            let result = load_repository_snapshot(&path).map_err(|error| format!("{error:#}"));
            let _ = sender.send(RepositoryRefreshCompletion { generation, result });
        });
    }

    /// Append the next history page when the selection nears the last row.
    pub(super) fn poll_graph_page(&mut self) {
        while let Ok(page) = self.graph_page_rx.try_recv() {
            self.redraw = true;
            if page.generation != self.graph_generation {
                continue;
            }
            self.graph_page_in_progress = false;
            if page.skip != self.graph.commits.len() {
                continue; // graph was reloaded; drop the stale page
            }
            let page = match page.result {
                Ok(page) => page,
                Err(error) => {
                    self.show_git_error(error);
                    return;
                }
            };
            if page.commits.is_empty() {
                self.graph_complete = true;
                continue;
            }
            // Take the engine out so the `known` set can borrow the commit ids.
            let mut engine = std::mem::take(&mut self.layout_engine);
            let rows = if page.complete {
                let known: HashSet<&str> = self
                    .graph
                    .commits
                    .iter()
                    .map(|commit| commit.id.as_str())
                    .chain(page.commits.iter().map(|commit| commit.id.as_str()))
                    .collect();
                engine.extend(&page.commits, Some(&known))
            } else {
                engine.extend(&page.commits, None)
            };
            self.layout_engine = engine;
            self.max_lanes = self.max_lanes.max(layout::max_lanes(&rows));
            self.rows.extend(rows);
            self.graph.commits.extend(page.commits);
            self.graph_complete = page.complete;
        }

        // Load the next page once the selection actually reaches the oldest
        // loaded row, so a short first page does not eagerly pull everything.
        let loaded_rows = self.rows.len() + usize::from(!self.local_status.is_empty());
        let at_end = loaded_rows > 0 && self.selected_graph + 1 >= loaded_rows;
        if at_end
            && !self.graph_complete
            && !self.graph_page_in_progress
            && !self.loading
            && self.remote_operation.is_none()
        {
            self.start_graph_page();
        }
    }
    pub(super) fn start_graph_page(&mut self) {
        let Some(path) = self.repository_path.clone() else {
            return;
        };
        let skip = self.graph.commits.len();
        let generation = self.graph_generation;
        let sender = self.graph_page_tx.clone();
        self.graph_page_in_progress = true;
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<GraphWindow> {
                let backend = crate::backend::open(&path)?;
                backend.graph_window(skip, graph_page_size())
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(GraphPageCompletion {
                skip,
                generation,
                result,
            });
        });
    }
    pub(super) fn poll_graph_details(&mut self) {
        while let Ok(completion) = self.graph_details_rx.try_recv() {
            self.redraw = true;
            self.graph_details_in_progress = false;
            if self.historical_commit.as_deref() != Some(completion.commit_id.as_str()) {
                continue;
            }
            match completion.result {
                Ok(details) => {
                    self.status = details.files;
                    self.refresh_status_indices();
                    self.selected_status = 0;
                    self.selected_statuses.clear();
                    if !self.status.is_empty() {
                        self.selected_statuses.insert(0);
                    }
                    self.status_section = StatusSection::Unstaged;
                    self.diff_stats.clear();
                    self.diff_hunk_counts.clear();
                    self.diff_stats.extend(
                        details
                            .stats
                            .into_iter()
                            .map(|(path, added, removed)| ((path, false), (added, removed))),
                    );
                    if let Some((path, diff, number_width)) = details.diff {
                        self.diff_path = Some(path);
                        self.diff_number_width = number_width;
                        self.diff_full = false;
                        self.diff_truncated = None;
                        self.apply_diff_text(diff);
                    } else {
                        self.diff_path = None;
                        self.diff_number_width = 3;
                        self.diff_lines.clear();
                    }
                    self.selected_diff_hunk = 0;
                    self.selected_diff_line = None;
                    self.diff_scroll = 0;
                    self.diff_cache = None;
                    self.details_loading = false;
                    self.details_error = None;
                }
                Err(error) => {
                    self.details_loading = false;
                    self.details_error = Some(error);
                }
            }
        }

        let ready = self
            .graph_details_deadline
            .is_some_and(|deadline| Instant::now() >= deadline);
        if !ready || self.graph_details_in_progress || self.git_busy() {
            return;
        }
        if self.graph_action.is_some()
            || self.commit_dialog.visible()
            || self.abort_rebase_popup
            || self.rebase_plan.is_some()
            || self.error_popup.is_some()
            || self.stash_popup.is_some()
            || self.command_picker.is_some()
        {
            return;
        }
        self.graph_details_deadline = None;
        let Some(commit_id) = self.historical_commit.clone() else {
            return;
        };
        let Some(path) = self.repository_path.clone() else {
            return;
        };
        let sender = self.graph_details_tx.clone();
        self.graph_details_in_progress = true;
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<_> {
                let backend = crate::backend::open(&path)?;
                let files = backend.commit_files(&commit_id)?;
                let stats = backend.commit_diff_stats(&commit_id)?;
                let diff = if let Some(file) = files.first() {
                    Some((
                        file.path.clone(),
                        backend.commit_diff(&commit_id, &file.path)?,
                        backend.file_line_number_width(&file.path, Some(&commit_id))?,
                    ))
                } else {
                    None
                };
                Ok(HistoricalCommitDetails { files, stats, diff })
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(GraphDetailsCompletion { commit_id, result });
        });
    }

    /// Select the checked-out commit so the graph opens where work is happening.
    pub(super) fn select_head_commit(&mut self) {
        let Some(head) = self.graph.head_id.as_deref() else {
            return;
        };
        let local_offset = usize::from(!self.local_status.is_empty());
        let Some(row) = self
            .rows
            .iter()
            .position(|row| self.graph.commits[row.commit_index].id == head)
        else {
            return;
        };
        self.select_graph_index(row + local_offset);
    }
    pub(super) fn reload_external_snapshot(&mut self, snapshot: RepositorySnapshot) {
        let RepositorySnapshot {
            graph,
            status: local_status,
            rows,
            max_lanes,
            layout: layout_engine,
            complete,
        } = snapshot;
        if graph == self.graph && local_status == self.local_status {
            // Porcelain status records change kinds, not file contents.
            self.refresh_visible_content();
            return;
        }
        let graph_display_refs = graph.display_refs_by_commit();
        let tracking_by_branch = graph
            .branch_tracking
            .iter()
            .cloned()
            .map(|tracking| (tracking.branch.clone(), tracking))
            .collect();
        let branch_ids = current_branch_ids(&graph);
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        self.graph_generation = self.graph_generation.wrapping_add(1);
        self.layout_engine = layout_engine;
        self.graph_complete = complete;
        self.graph_page_in_progress = false;

        let selected_commit = self.selected_commit_id();
        let selected_path = self
            .status
            .get(self.selected_status)
            .map(|entry| entry.path.clone());
        let old_section = self.status_section;
        let old_selected_graph = self.selected_graph;
        let historical = self.historical_commit.as_ref().and_then(|id| {
            if graph.commits.iter().any(|commit| &commit.id == id) {
                Some(id.clone())
            } else {
                graph.head_id.clone()
            }
        });
        let historical_changed = historical != self.historical_commit;
        let focus = self.focus;

        self.conflict_commits = if local_status.iter().any(StatusEntry::is_conflict) {
            backend.conflict_commits().ok().flatten()
        } else {
            None
        };
        self.active_conflict_paths = conflict_paths(&local_status);
        self.graph = graph;
        self.graph_display_refs = graph_display_refs;
        self.tracking_by_branch = tracking_by_branch;
        self.current_branch_ids = branch_ids;
        self.rows = rows;
        self.max_lanes = max_lanes;
        self.local_status = local_status;
        self.status = if let Some(commit) = historical.as_deref() {
            backend
                .commit_files(commit)
                .unwrap_or_else(|_| self.local_status.clone())
        } else {
            self.local_status.clone()
        };
        self.historical_commit = historical;
        if historical_changed {
            self.details_scroll = 0;
            self.graph_details_deadline = None;
            self.details_loading = false;
            self.details_error = None;
        }
        self.rebase_active = backend.rebase_in_progress().unwrap_or(self.rebase_active);

        let local_offset = usize::from(!self.local_status.is_empty());
        let selected_row = selected_commit.as_ref().and_then(|id| {
            self.rows
                .iter()
                .position(|row| self.graph.commits[row.commit_index].id == *id)
                .or_else(|| {
                    // An external rewrite (a rebase, amend, ...) can remove the
                    // selected commit. Follow HEAD instead of landing on an
                    // arbitrary row.
                    self.graph.head_id.as_deref().and_then(|head| {
                        self.rows
                            .iter()
                            .position(|row| self.graph.commits[row.commit_index].id == head)
                    })
                })
        });
        self.selected_graph = selected_row
            .map(|row| row + local_offset)
            .unwrap_or_else(|| {
                old_selected_graph.min(self.rows.len().saturating_sub(1) + local_offset)
            });
        self.scroll_to_selected_graph = true;
        self.keep_selected_graph_node_visible();
        self.refresh_status_indices();
        let old_section_indices = self.section_indices(old_section);
        self.status_section = if old_section_indices.is_empty() {
            match old_section {
                StatusSection::Unstaged if !self.staged_indices.is_empty() => StatusSection::Staged,
                StatusSection::Staged if !self.unstaged_indices.is_empty() => {
                    StatusSection::Unstaged
                }
                _ => old_section,
            }
        } else {
            old_section
        };
        let indices = self.section_indices(self.status_section).to_vec();
        self.selected_status = selected_path
            .as_ref()
            .and_then(|path| {
                indices
                    .iter()
                    .copied()
                    .find(|&index| self.status[index].path == *path)
            })
            .or_else(|| indices.first().copied())
            .unwrap_or(0);
        self.selected_statuses.clear();
        if !indices.is_empty() {
            self.selected_statuses.insert(self.selected_status);
        }
        self.focus = focus;
        self.refresh_diff();
        self.refresh_diff_stats();
    }
    pub(super) fn refresh_diff_stats(&mut self) {
        self.diff_stats.clear();
        self.diff_hunk_counts.clear();
        let Some(path) = self.repository_path.clone() else {
            return;
        };
        self.diff_stats_generation = self.diff_stats_generation.wrapping_add(1);
        let generation = self.diff_stats_generation;
        let entries = self.status.clone();
        let historical_commit = self.historical_commit.clone();
        let sender = self.diff_stats_tx.clone();
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<DiffStatsSnapshot> {
                let backend = crate::backend::open(&path)?;
                if let Some(commit) = historical_commit {
                    let stats = backend
                        .commit_diff_stats(&commit)?
                        .into_iter()
                        .map(|(path, added, removed)| PathDiffStats {
                            path,
                            staged: false,
                            added,
                            removed,
                        })
                        .collect();
                    return Ok(DiffStatsSnapshot {
                        stats,
                        hunks: Vec::new(),
                    });
                }
                let stats = backend.status_diff_stats(&entries)?;
                let hunks = backend.status_hunk_counts(&entries)?;
                Ok(DiffStatsSnapshot { stats, hunks })
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(DiffStatsCompletion { generation, result });
        });
    }
    pub(super) fn poll_diff_stats(&mut self) {
        while let Ok(completion) = self.diff_stats_rx.try_recv() {
            self.redraw = true;
            if completion.generation != self.diff_stats_generation {
                continue;
            }
            let Ok(snapshot) = completion.result else {
                continue;
            };
            self.diff_stats.clear();
            self.diff_hunk_counts.clear();
            for stat in snapshot.stats {
                self.diff_stats
                    .insert((stat.path, stat.staged), (stat.added, stat.removed));
            }
            self.diff_hunk_counts.extend(snapshot.hunks);
        }
    }
}

pub(super) struct RepositoryRefreshCompletion {
    pub(super) generation: u64,
    pub(super) result: std::result::Result<RepositorySnapshot, String>,
}

pub(super) struct GraphPageCompletion {
    /// Commit count and graph generation when the page was requested. The
    /// generation distinguishes reloads whose first page has the same length.
    pub(super) skip: usize,
    pub(super) generation: u64,
    pub(super) result: std::result::Result<GraphWindow, String>,
}

pub(super) struct RepositorySnapshot {
    pub(super) graph: GraphModel,
    pub(super) status: Vec<StatusEntry>,
    pub(super) rows: Vec<GraphRow>,
    pub(super) max_lanes: usize,
    pub(super) layout: layout::Layout,
    pub(super) complete: bool,
}

pub(super) struct GraphDetailsCompletion {
    pub(super) commit_id: String,
    pub(super) result: std::result::Result<HistoricalCommitDetails, String>,
}

pub(super) struct DiffStatsCompletion {
    pub(super) generation: u64,
    pub(super) result: std::result::Result<DiffStatsSnapshot, String>,
}

pub(super) struct DiffStatsSnapshot {
    pub(super) stats: Vec<PathDiffStats>,
    pub(super) hunks: Vec<(GitPath, (usize, usize))>,
}

pub(super) struct HistoricalCommitDetails {
    pub(super) files: Vec<StatusEntry>,
    pub(super) stats: Vec<(GitPath, usize, usize)>,
    pub(super) diff: Option<(GitPath, String, usize)>,
}

/// Commits loaded per history page. The first page renders immediately and
/// older history is appended as the selection approaches the end.
/// Override with `FIGIT_GRAPH_PAGE` (useful on very large repositories).
pub(super) const GRAPH_PAGE: usize = 2000;

pub(super) fn graph_page_size() -> usize {
    std::env::var("FIGIT_GRAPH_PAGE")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .filter(|size| *size > 0)
        .unwrap_or(GRAPH_PAGE)
}

pub(super) fn load_repository_snapshot(path: &Path) -> Result<RepositorySnapshot> {
    let backend = crate::backend::open(path)?;
    crate::profile::mark("snapshot: backend");
    let window = backend.graph_window(0, graph_page_size())?;
    crate::profile::mark("snapshot: graph page");
    let status = backend.status()?;
    crate::profile::mark("snapshot: status");
    let mut layout = layout::Layout::new();
    let rows = if window.complete {
        let known: HashSet<&str> = window
            .commits
            .iter()
            .map(|commit| commit.id.as_str())
            .collect();
        layout.extend(&window.commits, Some(&known))
    } else {
        layout.extend(&window.commits, None)
    };
    let max_lanes = layout::max_lanes(&rows);
    crate::profile::mark("snapshot: layout");
    Ok(RepositorySnapshot {
        graph: GraphModel {
            commits: window.commits,
            refs: window.refs,
            branch_tracking: window.branch_tracking,
            head_id: window.head_id,
            head_name: window.head_name,
        },
        status,
        rows,
        max_lanes,
        layout,
        complete: window.complete,
    })
}

impl App {
    /// Note that Figit just changed repository state itself, so watcher
    /// events from those writes are not treated as external changes.
    pub(super) fn note_internal_change(&mut self) {
        self.ignore_watch_until = Some(Instant::now() + Duration::from_millis(600));
        self.refresh.generation = self.refresh.generation.wrapping_add(1);
    }

    pub(super) fn refresh_status(&mut self) {
        self.note_internal_change();
        if let Some(backend) = self.backend.as_ref() {
            self.rebase_active = backend.rebase_in_progress().unwrap_or(self.rebase_active);
            if let Ok(status) = backend.status() {
                let conflict_commits = if status.iter().any(StatusEntry::is_conflict) {
                    backend.conflict_commits().ok().flatten()
                } else {
                    None
                };
                // While a commit is selected, the right pane lists that commit's
                // files; keep them rather than replacing them with the worktree.
                let pane_status = match self.historical_commit.as_deref() {
                    Some(commit) => backend
                        .commit_files(commit)
                        .unwrap_or_else(|_| status.clone()),
                    None => status.clone(),
                };
                self.active_conflict_paths = conflict_paths(&status);
                self.conflict_commits = conflict_commits;
                self.local_status = status;
                self.status = pane_status;
                self.refresh_status_indices();
                if let Some(conflict) = self.status.iter().position(StatusEntry::is_conflict) {
                    self.status_section = StatusSection::Unstaged;
                    self.selected_status = conflict;
                    self.selected_statuses.clear();
                    self.selected_statuses.insert(conflict);
                } else {
                    self.selected_statuses
                        .retain(|&index| index < self.status.len());
                    self.selected_status = self
                        .selected_status
                        .min(self.status.len().saturating_sub(1));
                }
                if self.status.is_empty() {
                    self.selected_statuses.clear();
                } else if self.selected_statuses.is_empty() {
                    self.selected_statuses.insert(self.selected_status);
                }
                self.refresh_diff();
                self.refresh_diff_stats();
            }
        }
    }

    pub(super) fn refresh_status_indices(&mut self) {
        self.unstaged_indices = self
            .status
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                (e.is_conflict() || e.worktree != ' ' || e.index == '?').then_some(i)
            })
            .collect();
        self.staged_indices = self
            .status
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                (!e.is_conflict() && e.index != ' ' && e.index != '?').then_some(i)
            })
            .collect();
        self.unstaged_indices
            .sort_by(|&left, &right| self.status[left].path.cmp(&self.status[right].path));
        self.staged_indices
            .sort_by(|&left, &right| self.status[left].path.cmp(&self.status[right].path));
    }

    pub(super) fn reload_from_snapshot(&mut self, snapshot: RepositorySnapshot) {
        self.note_internal_change();
        let RepositorySnapshot {
            graph,
            status,
            rows,
            max_lanes,
            layout: layout_engine,
            complete,
        } = snapshot;
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        self.graph_generation = self.graph_generation.wrapping_add(1);
        self.layout_engine = layout_engine;
        self.graph_complete = complete;
        self.graph_page_in_progress = false;
        self.rebase_active = backend.rebase_in_progress().unwrap_or(false);
        self.conflict_commits = if status.iter().any(StatusEntry::is_conflict) {
            backend.conflict_commits().ok().flatten()
        } else {
            None
        };
        self.active_conflict_paths = conflict_paths(&status);
        self.graph = graph;
        self.rebuild_graph_indexes();
        self.rows = rows;
        self.max_lanes = max_lanes;
        self.status = status.clone();
        self.local_status = status;
        self.historical_commit = None;
        self.graph_details_deadline = None;
        self.details_loading = false;
        self.details_error = None;
        let has_local = !self.local_status.is_empty();
        self.selected_graph = self
            .reset_boundary
            .as_ref()
            .and_then(|target| {
                self.rows
                    .iter()
                    .position(|row| self.graph.commits[row.commit_index].id == *target)
            })
            .map(|row| row + usize::from(has_local))
            .unwrap_or(0);
        self.keep_selected_graph_node_visible();
        self.refresh_status_indices();
        self.status_section = if self.unstaged_indices.is_empty() && !self.staged_indices.is_empty()
        {
            StatusSection::Staged
        } else {
            StatusSection::Unstaged
        };
        self.selected_status = self
            .section_indices(self.status_section)
            .first()
            .copied()
            .unwrap_or(0);
        self.selected_statuses.clear();
        if !self.status.is_empty() {
            self.selected_statuses.insert(self.selected_status);
        }
        self.commit_dialog.title.clear();
        self.commit_dialog.title_cursor = 0;
        self.commit_dialog.description.clear();
        self.commit_dialog.description_cursor = 0;
        self.commit_dialog.mode = CommitMode::Normal;
        self.commit_dialog.set_open(false);
        self.commit_dialog.set_confirming(false);
        self.abort_rebase_popup = false;
        self.error_popup = None;
        self.error_scroll = 0;
        self.three_way_view = None;
        self.focus = Focus::Status;
        self.refresh_diff();
        self.refresh_diff_stats();
    }
}

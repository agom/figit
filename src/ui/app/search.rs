use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

impl App {
    pub(super) fn open_search_popup(&mut self) {
        self.pending_fixup = false;
        self.search_popup = Some(SearchPopupState {
            query: String::new(),
            cursor: 0,
            matches: Vec::new(),
            selected: 0,
            searched: false,
            all_history: false,
            worker: None,
            navigating: false,
            scanned: 0,
            generation: self.graph_generation,
            error: None,
        });
    }
    pub(super) fn handle_search_key(&mut self, key: KeyEvent) {
        let Some(state) = self.search_popup.as_mut() else {
            return;
        };
        let jump_to_result = state.searched
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
            && !state.matches.is_empty();
        let reset_search = state.searched && key.code == KeyCode::Char('/');
        match key.code {
            KeyCode::Esc => {
                self.search_popup = None;
                self.diff_cache = None;
            }
            KeyCode::Char('a') if state.searched => {
                if state.worker.is_none() && (!state.all_history || state.error.is_some()) {
                    self.start_history_lookup();
                }
            }
            KeyCode::Enter if state.searched => self.select_search_result(),
            KeyCode::Enter => self.start_commit_search(),
            KeyCode::Char('/') if state.searched => {
                state.worker = None;
                state.navigating = false;
                state.all_history = false;
                state.query.clear();
                state.cursor = 0;
                state.matches.clear();
                state.selected = 0;
                state.searched = false;
                state.error = None;
            }
            KeyCode::Up if state.searched && !state.matches.is_empty() => {
                state.selected = if state.selected == 0 {
                    state.matches.len() - 1
                } else {
                    state.selected - 1
                };
            }
            KeyCode::Down if state.searched && !state.matches.is_empty() => {
                state.selected = (state.selected + 1) % state.matches.len();
            }
            KeyCode::Left if !state.searched => {
                state.cursor = state.cursor.saturating_sub(1);
            }
            KeyCode::Right if !state.searched => {
                state.cursor = (state.cursor + 1).min(state.query.chars().count());
            }
            KeyCode::Home if !state.searched => state.cursor = 0,
            KeyCode::End if !state.searched => state.cursor = state.query.chars().count(),
            KeyCode::Backspace if !state.searched => {
                backspace_at_cursor(&mut state.query, &mut state.cursor);
            }
            KeyCode::Delete if !state.searched => delete_at_cursor(&mut state.query, state.cursor),
            KeyCode::Char('u')
                if key.modifiers.contains(KeyModifiers::CONTROL) && !state.searched =>
            {
                state.query.clear();
                state.cursor = 0;
            }
            KeyCode::Char(character)
                if !state.searched
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                insert_at_cursor(&mut state.query, &mut state.cursor, character);
            }
            _ => {}
        }
        if jump_to_result {
            self.select_search_result();
        }
        if reset_search {
            self.diff_cache = None;
        }
    }
    pub(super) fn start_commit_search(&mut self) {
        let Some(state) = self.search_popup.as_mut() else {
            return;
        };
        let query = state.query.trim().to_string();
        if query.is_empty() {
            return;
        }
        state.matches.clear();
        state.scanned = 0;
        state.selected = 0;
        state.searched = true;
        state.error = None;
        self.extend_history_search();
    }

    fn start_history_lookup(&mut self) {
        let Some(path) = self.repository_path.clone() else {
            return;
        };
        let Some(state) = self.search_popup.as_mut() else {
            return;
        };
        let query = state.query.trim().to_lowercase();
        let tips = matching_branch_tips(&self.graph, &query);
        let include_head = self.graph.head_id.is_some();
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result =
                crate::backend::gix::search_history(&path, &query, &tips, include_head, &cancelled);
            let _ = tx.send(result.map_err(|e| e.to_string()));
        });
        state.worker = Some(HistoryLookup { stop, rx });
        state.all_history = true;
        state.error = None;
    }

    pub(super) fn extend_history_search(&mut self) {
        let Some(state) = self.search_popup.as_mut().filter(|s| s.searched) else {
            return;
        };
        if state.generation != self.graph_generation {
            state.generation = self.graph_generation;
            if state.all_history {
                state.navigating = false;
                self.start_history_lookup();
                return;
            }
            state.matches.clear();
            state.scanned = 0;
            state.selected = 0;
        }
        if state.all_history {
            if let Some(job) = &state.worker {
                let result = match job.rx.try_recv() {
                    Ok(result) => result,
                    Err(TryRecvError::Empty) => return,
                    Err(TryRecvError::Disconnected) => Err("History search stopped".into()),
                };
                state.worker = None;
                self.redraw = true;
                match result {
                    Ok(matches) => {
                        let previous = state.matches.get(state.selected);
                        state.selected = previous
                            .and_then(|id| matches.iter().position(|m| m == id))
                            .unwrap_or(0);
                        state.matches = matches;
                        self.select_search_result();
                    }
                    Err(error) => state.error = Some(error),
                }
            } else if state.navigating
                && (state.scanned != self.graph.commits.len() || self.graph_complete)
            {
                self.select_search_result();
            }
            return;
        }
        if state.scanned == self.graph.commits.len() {
            return;
        }
        let query = state.query.trim().to_lowercase();
        let tips = matching_branch_tips(&self.graph, &query);
        let first = state.matches.is_empty();
        state.matches.extend(
            self.graph.commits[state.scanned..]
                .iter()
                .filter(|commit| {
                    tips.contains(&commit.id)
                        || commit.summary.to_lowercase().contains(&query)
                        || commit.message.to_lowercase().contains(&query)
                })
                .map(|commit| commit.id.clone()),
        );
        state.scanned = self.graph.commits.len();
        if first {
            self.select_search_result();
        }
    }
    pub(super) fn select_search_result(&mut self) {
        let Some(id) = self
            .search_popup
            .as_ref()
            .and_then(|state| state.matches.get(state.selected).cloned())
        else {
            return;
        };
        self.action_error = None;
        let local_offset = usize::from(!self.local_status.is_empty());
        let row = self
            .rows
            .iter()
            .position(|row| self.graph.commits[row.commit_index].id == id);
        let state = self.search_popup.as_mut().unwrap();
        state.navigating = row.is_none() && !self.graph_complete;
        state.scanned = self.graph.commits.len();
        if let Some(row) = row {
            self.focus = Focus::Graph;
            self.select_graph_index(row + local_offset);
        } else if self.graph_complete {
            state.error = Some("Result is no longer in the graph; search again".into());
        }
    }
}

fn matching_branch_tips(graph: &GraphModel, query: &str) -> HashSet<String> {
    graph
        .refs
        .iter()
        .filter(|r| {
            matches!(r.kind, RefKind::LocalBranch | RefKind::RemoteBranch)
                && r.name.to_lowercase().contains(query)
        })
        .map(|r| r.id.clone())
        .collect()
}

struct HistoryLookup {
    stop: Arc<AtomicBool>,
    rx: Receiver<Result<Vec<String>, String>>,
}

impl Drop for HistoryLookup {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub(super) struct SearchPopupState {
    pub(super) query: String,
    pub(super) cursor: usize,
    pub(super) matches: Vec<String>,
    pub(super) selected: usize,
    pub(super) searched: bool,
    pub(super) all_history: bool,
    worker: Option<HistoryLookup>,
    pub(super) navigating: bool,
    scanned: usize,
    generation: u64,
    pub(super) error: Option<String>,
}

impl App {
    pub(super) fn selected_search_query(&self) -> Option<&str> {
        let state = self.search_popup.as_ref().filter(|state| state.searched)?;
        let selected = state.matches.get(state.selected)?;
        (self.historical_commit.as_deref() == Some(selected.as_str()))
            .then_some(state.query.as_str())
    }
}

impl SearchPopupState {
    pub(super) fn looking_up(&self) -> bool {
        self.worker.is_some()
    }
}

use super::*;

impl App {
    pub(super) fn open_search_popup(&mut self) {
        self.pending_fixup = false;
        self.search_popup = Some(SearchPopupState {
            query: String::new(),
            cursor: 0,
            matches: Vec::new(),
            selected: 0,
            searched: false,
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
            KeyCode::Enter if state.searched => self.select_search_result(),
            KeyCode::Enter => self.start_commit_search(),
            KeyCode::Char('/') if state.searched => {
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
        let folded = query.to_lowercase();
        let position = self
            .graph
            .commits
            .iter()
            .enumerate()
            .filter(|(_, commit)| {
                commit.summary.to_lowercase().contains(&folded)
                    || commit.message.to_lowercase().contains(&folded)
            })
            .map(|(index, commit)| (index, commit.id.clone()))
            .collect::<Vec<_>>();
        state.matches = position.into_iter().map(|(_, id)| id).collect();
        state.selected = 0;
        state.searched = true;
        state.error = None;
        self.select_search_result();
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
        if let Some(row) = self
            .rows
            .iter()
            .position(|row| self.graph.commits[row.commit_index].id == id)
        {
            self.focus = Focus::Graph;
            self.select_graph_index(row + local_offset);
        }
    }
}

pub(super) struct SearchPopupState {
    pub(super) query: String,
    pub(super) cursor: usize,
    pub(super) matches: Vec<String>,
    pub(super) selected: usize,
    pub(super) searched: bool,
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

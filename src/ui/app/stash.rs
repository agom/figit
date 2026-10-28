//! Stash dialog state and workflows.

use super::*;

pub(super) struct StashUiState {
    pub(super) entries: Vec<StashEntry>,
    pub(super) selected: usize,
    pub(super) saving: bool,
    pub(super) message: String,
    pub(super) message_cursor: usize,
    pub(super) include_untracked: bool,
    pub(super) confirming_drop: bool,
}

impl App {
    pub(super) fn open_stash_manager(&mut self) {
        self.pending_fixup = false;
        if self.rebase_active || self.three_way_view.is_some() {
            self.show_git_error(
                "Finish or abort the active conflict/rebase before managing stashes.".into(),
            );
            return;
        }
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        match backend.stash_list() {
            Ok(entries) => {
                self.stash_popup = Some(StashUiState {
                    entries,
                    selected: 0,
                    saving: false,
                    message: "WIP".into(),
                    message_cursor: 3,
                    include_untracked: false,
                    confirming_drop: false,
                });
            }
            Err(error) => self.show_git_error(error.to_string()),
        }
    }

    pub(super) fn refresh_stash_entries(&mut self) {
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        match backend.stash_list() {
            Ok(entries) => {
                if let Some(state) = self.stash_popup.as_mut() {
                    state.entries = entries;
                    state.selected = state.selected.min(state.entries.len().saturating_sub(1));
                }
            }
            Err(error) => self.show_git_error(error.to_string()),
        }
    }

    pub(super) fn save_stash(&mut self) {
        let Some(state) = self.stash_popup.as_ref() else {
            return;
        };
        let message = state.message.trim().to_string();
        let message = if message.is_empty() {
            "WIP".to_string()
        } else {
            message
        };
        let include_untracked = state.include_untracked;
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        match backend.stash_push(&message, include_untracked) {
            Ok(()) => {
                self.stash_popup = None;
                self.refresh_status();
                self.refresh_diff_stats();
            }
            Err(error) => self.show_git_error(error.to_string()),
        }
    }

    pub(super) fn apply_selected_stash(&mut self, pop: bool) {
        let Some(state) = self.stash_popup.as_ref() else {
            return;
        };
        let Some(entry) = state.entries.get(state.selected) else {
            return;
        };
        let reference = entry.reference.clone();
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        let result = if pop {
            backend.stash_pop(&reference)
        } else {
            backend.stash_apply(&reference)
        };
        self.stash_popup = None;
        self.refresh_status();
        self.refresh_diff_stats();
        if let Err(error) = result {
            self.show_git_error(error.to_string());
        }
    }

    pub(super) fn drop_selected_stash(&mut self) {
        let Some(state) = self.stash_popup.as_ref() else {
            return;
        };
        let Some(entry) = state.entries.get(state.selected) else {
            return;
        };
        let reference = entry.reference.clone();
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        match backend.stash_drop(&reference) {
            Ok(()) => self.refresh_stash_entries(),
            Err(error) => self.show_git_error(error.to_string()),
        }
    }

    pub(super) fn handle_stash_key(&mut self, key: KeyEvent) -> bool {
        let Some(state) = self.stash_popup.as_mut() else {
            return false;
        };
        if state.saving {
            match key.code {
                KeyCode::Esc => state.saving = false,
                KeyCode::Enter => self.save_stash(),
                KeyCode::Tab => state.include_untracked = !state.include_untracked,
                KeyCode::Left => state.message_cursor = state.message_cursor.saturating_sub(1),
                KeyCode::Right => {
                    state.message_cursor =
                        (state.message_cursor + 1).min(state.message.chars().count())
                }
                KeyCode::Home => state.message_cursor = 0,
                KeyCode::End => state.message_cursor = state.message.chars().count(),
                KeyCode::Backspace => {
                    backspace_at_cursor(&mut state.message, &mut state.message_cursor)
                }
                KeyCode::Delete => delete_at_cursor(&mut state.message, state.message_cursor),
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    insert_at_cursor(&mut state.message, &mut state.message_cursor, character);
                }
                _ => {}
            }
            return false;
        }
        if state.confirming_drop {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    if let Some(state) = self.stash_popup.as_mut() {
                        state.confirming_drop = false;
                    }
                    self.drop_selected_stash();
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    state.confirming_drop = false;
                }
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.stash_popup = None,
            KeyCode::Up | KeyCode::Char('k') => {
                state.selected = state.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.selected = (state.selected + 1).min(state.entries.len().saturating_sub(1));
            }
            KeyCode::Char('u') => state.include_untracked = !state.include_untracked,
            KeyCode::Char('s') => {
                state.saving = true;
                state.message.clear();
                state.message_cursor = 0;
            }
            KeyCode::Char('a') => self.apply_selected_stash(false),
            KeyCode::Char('p') => self.apply_selected_stash(true),
            KeyCode::Char('d') if !state.entries.is_empty() => state.confirming_drop = true,
            KeyCode::Enter => self.apply_selected_stash(false),
            _ => {}
        }
        false
    }
}

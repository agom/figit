//! Shared application state and constructors; feature handlers live in submodules.

mod commands;
mod commit;
mod conflict;
mod conflict_model;
mod diff;
mod diff_view;
mod graph;
mod input;
mod jobs;
mod presentation;
mod rebase;
mod refresh;
mod remote;
mod render;
mod search;
mod stash;
mod terminal;
mod text;
mod update;
mod watch;
mod workflows;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use notify::{Config as NotifyConfig, RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::Frame;

use crate::backend::{
    ConflictCommits, ConflictResolution, ConflictSide, DiffLineSelection, DiffTarget, GitBackend,
    GraphWindow, PathDiffStats, RebaseAction, RebaseOutcome, RebasePlan, RebasePlanBlock,
    RebasePlanEntry, RebaseTodoAction, StashEntry, UndoInfo,
};
use crate::core::model::{
    BranchTracking, Commit, DisplayRef, GitPath, GraphModel, RefKind, StatusEntry,
};
use crate::graph::layout::{self, GraphRow};

use commands::{CommandId, CommandPickerState};
use commit::{split_commit_message, CommitMode};
use conflict::{conflict_paths, ThreeWayView};
use conflict_model::{
    align_conflict_side, conflict_hunks, display_lines, replace_conflict_hunk, three_way_column,
    AlignedSide, ConflictHunk, ThreeWayCell, ThreeWayRow,
};
use graph::{current_branch_ids, GraphSelectionSnapshot};
use presentation::{
    border_scrollbar_area, conflict_side_border_style, darker_color, dim_graph_line, fmt_date,
    minimal_scrollbar, scrollbar_position, status_glyph, AHEAD_GLYPH, BEHIND_GLYPH,
    HEAD_BRANCH_MARKER, PALETTE, PILL_LEFT, PILL_RIGHT, REMOTE_GLYPH, TAG_GLYPH,
};
use rebase::{
    plan_entry_at, plan_entry_mut, rebase_plan_entry_count, rebase_todo_action_name,
    RebaseEditState, RebasePlanState,
};
use refresh::{
    load_repository_snapshot, DiffStatsCompletion, GraphDetailsCompletion, GraphPageCompletion,
    RepositorySnapshot,
};
use remote::{PushTarget, RemoteCompletion, RemoteOperation};
use search::SearchPopupState;
use stash::StashUiState;
use update::{UpdateEvent, UpdateStatus};
use workflows::{DeleteTarget, GraphAction};

use diff::{
    diff_hunk_headers, diff_layout, emphasize_selected_diff_hunk, emphasize_selected_diff_line,
    parse_diff, render_conflict_diff, render_diff_row, DiffKind, DiffLine, DiffMode, DiffRow,
};
use terminal::TerminalAnsiPalette;
use text::{
    backspace_at_cursor, delete_at_cursor, highlight_search_matches, insert_at_cursor,
    move_cursor_to_line_edge, move_cursor_vertical, pad_colored_lines, single_line_cursor,
    styled_key_hints, styled_key_hints_fitting, truncate, truncate_start, wrapped_text_cursor,
    wrapped_text_lines,
};

pub use terminal::{parse_osc11, parse_osc4};

pub struct App {
    refresh: refresh::RefreshState,
    commit_dialog: commit::CommitDialog,
    local_job: Option<jobs::LocalJob>,
    redraw: bool,
    graph: GraphModel,
    graph_display_refs: HashMap<String, Vec<DisplayRef>>,
    tracking_by_branch: HashMap<String, BranchTracking>,
    current_branch_ids: HashSet<String>,
    status: Vec<StatusEntry>,
    backend: Option<Box<dyn GitBackend>>,
    diff_lines: Vec<DiffLine>,
    diff_error: Option<String>,
    diff_full: bool,
    diff_truncated: Option<(usize, usize)>,
    selected_diff_hunk: usize,
    hunk_mode: bool,
    line_mode: bool,
    diff_mode_before_line: Option<DiffMode>,
    selected_diff_line: Option<usize>,
    diff_target: DiffTarget,
    diff_path: Option<GitPath>,
    /// Cached diff layout (cheap) for the current mode; rows are materialized
    /// on demand so syntax highlighting only runs for the visible slice.
    diff_cache: Option<(DiffMode, Vec<DiffRow>)>,
    /// Indices of hunk-header lines within `diff_lines`, recomputed only when
    /// the diff changes so the render and hunk-navigation paths don't rescan.
    hunk_header_indices: Vec<usize>,
    diff_stats: HashMap<(GitPath, bool), (usize, usize)>,
    diff_hunk_counts: HashMap<GitPath, (usize, usize)>,
    conflict_commits: Option<ConflictCommits>,
    active_conflict_paths: HashSet<GitPath>,
    rows: Vec<GraphRow>,
    max_lanes: usize,
    scroll_y: usize,
    scroll_to_selected_graph: bool,
    scroll_x: usize,
    selected_graph: usize,
    local_status: Vec<StatusEntry>,
    unstaged_indices: Vec<usize>,
    staged_indices: Vec<usize>,
    historical_commit: Option<String>,
    /// The terminal's actual background color (queried once via OSC 11), used
    /// to draw pill text/caps explicitly; `None` falls back to reverse video.
    bg: Option<Color>,
    terminal_palette: TerminalAnsiPalette,
    no_color: bool,
    panel_hints_visible: bool,
    focus: Focus,
    selected_status: usize,
    selected_statuses: HashSet<usize>,
    status_section: StatusSection,
    staging_area: Rect,
    graph_area: Rect,
    details_area: Rect,
    diff_area: Rect,
    diff_scroll: u16,
    /// Horizontal pan (in display columns) of the focused diff's content.
    diff_scroll_x: u16,
    details_scroll: u16,
    diff_mode: DiffMode,
    graph_visible: bool,
    maximized: bool,
    viewport_width: u16,
    diff_number_width: usize,
    commit_field: CommitField,
    abort_rebase_popup: bool,
    error_popup: Option<String>,
    error_title: String,
    error_scroll: u16,
    graph_action: Option<GraphAction>,
    graph_action_return_focus: Option<Focus>,
    graph_action_input: String,
    graph_action_cursor: usize,
    discard_paths: Vec<GitPath>,
    reset_mode: usize,
    reset_focus: usize,
    remote_action: bool,
    push_mode: usize,
    push_focus: usize,
    action_error: Option<String>,
    undo_options: Vec<UndoInfo>,
    undo_selection: usize,
    repository_path: Option<PathBuf>,
    watch_tx: SyncSender<()>,
    watch_rx: Receiver<()>,
    /// Our own mutations write to `.git` (loose objects, refs) and would
    /// otherwise trigger a redundant auto-refresh right after the UI already
    /// reloaded. Ignore watcher events inside this window.
    ignore_watch_until: Option<Instant>,
    watch_error: Option<String>,
    watcher_error: Option<String>,
    watcher_poll_deadline: Option<Instant>,
    watch_setup: Option<watch::WatcherSetup>,
    repository_watcher: Option<RecommendedWatcher>,
    graph_details_tx: Sender<GraphDetailsCompletion>,
    graph_details_rx: Receiver<GraphDetailsCompletion>,
    graph_details_deadline: Option<Instant>,
    graph_details_in_progress: bool,
    diff_stats_tx: Sender<DiffStatsCompletion>,
    diff_stats_rx: Receiver<DiffStatsCompletion>,
    diff_stats_generation: u64,
    details_loading: bool,
    details_error: Option<String>,
    search_popup: Option<SearchPopupState>,
    command_picker: Option<CommandPickerState>,
    remote_tx: Sender<RemoteCompletion>,
    remote_rx: Receiver<RemoteCompletion>,
    remote_operation: Option<RemoteOperation>,
    remote_started: Option<Instant>,
    remote_notice: Option<(String, Instant)>,
    update_tx: Sender<UpdateEvent>,
    update_rx: Receiver<UpdateEvent>,
    update_status: UpdateStatus,
    update_status_since: Option<Instant>,
    remote_choices: Vec<String>,
    remote_selection: usize,
    push_remote: Option<String>,
    push_target: Option<PushTarget>,
    push_after_create: bool,
    delete_targets: Vec<DeleteTarget>,
    delete_target_index: usize,
    loading: bool,
    loading_started: Option<Instant>,
    graph_page_tx: Sender<GraphPageCompletion>,
    graph_page_rx: Receiver<GraphPageCompletion>,
    graph_page_in_progress: bool,
    graph_generation: u64,
    layout_engine: layout::Layout,
    graph_complete: bool,
    reset_boundary: Option<String>,
    pending_fixup: bool,
    fixup_origin: usize,
    rebase_paused: bool,
    rebase_active: bool,
    rebase_edit: Option<RebaseEditState>,
    rebase_plan: Option<RebasePlanState>,
    pending_conflict_editor: Option<GitPath>,
    three_way_view: Option<ThreeWayView>,
    stash_popup: Option<StashUiState>,
    help_popup: bool,
    help_scroll: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Status,
    Graph,
    Details,
    Commit,
    Diff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommitField {
    Title,
    Description,
    Commit,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusSection {
    Unstaged,
    Staged,
}

impl App {
    pub fn new(
        graph: GraphModel,
        status: Vec<StatusEntry>,
        backend: Option<Box<dyn GitBackend>>,
    ) -> Self {
        let rows = layout::layout(&graph);
        let max_lanes = layout::max_lanes(&rows);
        let has_local_changes = !status.is_empty();
        let repository_path = backend
            .as_ref()
            .map(|backend| backend.working_directory().to_path_buf());
        let (remote_tx, remote_rx) = mpsc::channel();
        let (update_tx, update_rx) = mpsc::channel();
        let (watch_tx, watch_rx) = mpsc::sync_channel(1);
        let (refresh_tx, refresh_rx) = mpsc::channel();
        let (graph_page_tx, graph_page_rx) = mpsc::channel();
        let (graph_details_tx, graph_details_rx) = mpsc::channel();
        let (diff_stats_tx, diff_stats_rx) = mpsc::channel();
        let conflict_commits = if status.iter().any(StatusEntry::is_conflict) {
            backend
                .as_ref()
                .and_then(|backend| backend.conflict_commits().ok().flatten())
        } else {
            None
        };
        let active_conflict_paths = conflict_paths(&status);
        let rebase_active = backend
            .as_ref()
            .and_then(|backend| backend.rebase_in_progress().ok())
            .unwrap_or(false);
        let mut app = Self {
            refresh: refresh::RefreshState::new(refresh_tx, refresh_rx),
            commit_dialog: commit::CommitDialog::default(),
            local_job: None,
            redraw: true,
            graph,
            graph_display_refs: HashMap::new(),
            tracking_by_branch: HashMap::new(),
            current_branch_ids: HashSet::new(),
            status: status.clone(),
            backend,
            diff_lines: Vec::new(),
            diff_error: None,
            diff_full: false,
            diff_truncated: None,
            selected_diff_hunk: 0,
            hunk_mode: false,
            line_mode: false,
            diff_mode_before_line: None,
            selected_diff_line: None,
            diff_target: DiffTarget::Worktree,
            diff_path: None,
            diff_cache: None,
            hunk_header_indices: Vec::new(),
            diff_stats: HashMap::new(),
            diff_hunk_counts: HashMap::new(),
            conflict_commits,
            active_conflict_paths,
            rows,
            max_lanes,
            scroll_y: 0,
            scroll_to_selected_graph: false,
            scroll_x: 0,
            selected_graph: 0,
            local_status: status.clone(),
            unstaged_indices: Vec::new(),
            staged_indices: Vec::new(),
            historical_commit: None,
            bg: None,
            terminal_palette: [None; 16],
            no_color: false,
            panel_hints_visible: true,
            focus: if has_local_changes {
                Focus::Status
            } else {
                Focus::Graph
            },
            selected_status: 0,
            selected_statuses: HashSet::new(),
            status_section: StatusSection::Unstaged,
            staging_area: Rect::default(),
            graph_area: Rect::default(),
            details_area: Rect::default(),
            diff_area: Rect::default(),
            diff_scroll: 0,
            diff_scroll_x: 0,
            details_scroll: 0,
            diff_mode: DiffMode::Unified,
            graph_visible: true,
            maximized: false,
            viewport_width: 120,
            diff_number_width: 3,
            commit_field: CommitField::Title,
            abort_rebase_popup: false,
            error_popup: None,
            error_title: "Git error".into(),
            error_scroll: 0,
            graph_action: None,
            graph_action_return_focus: None,
            graph_action_input: String::new(),
            graph_action_cursor: 0,
            discard_paths: Vec::new(),
            reset_mode: 1,
            reset_focus: 1,
            remote_action: false,
            push_mode: 0,
            push_focus: 0,
            action_error: None,
            undo_options: Vec::new(),
            undo_selection: 0,
            repository_path,
            watch_tx,
            watch_rx,
            ignore_watch_until: None,
            watch_error: None,
            watcher_error: None,
            watcher_poll_deadline: None,
            watch_setup: None,
            repository_watcher: None,
            graph_details_tx,
            graph_details_rx,
            graph_details_deadline: None,
            graph_details_in_progress: false,
            diff_stats_tx,
            diff_stats_rx,
            diff_stats_generation: 0,
            details_loading: false,
            details_error: None,
            search_popup: None,
            command_picker: None,
            remote_tx,
            remote_rx,
            remote_operation: None,
            remote_started: None,
            remote_notice: None,
            update_tx,
            update_rx,
            update_status: UpdateStatus::Idle,
            update_status_since: None,
            remote_choices: Vec::new(),
            remote_selection: 0,
            push_remote: None,
            push_target: None,
            push_after_create: false,
            delete_targets: Vec::new(),
            delete_target_index: 0,
            loading: false,
            loading_started: None,
            graph_page_tx,
            graph_page_rx,
            graph_page_in_progress: false,
            graph_generation: 0,
            layout_engine: layout::Layout::new(),
            graph_complete: true,
            reset_boundary: None,
            fixup_origin: 0,
            pending_fixup: false,
            rebase_paused: rebase_active,
            rebase_active,
            rebase_edit: None,
            rebase_plan: None,
            pending_conflict_editor: None,
            three_way_view: None,
            stash_popup: None,
            help_popup: false,
            help_scroll: 0,
        };
        app.rebuild_graph_indexes();
        app.refresh_status_indices();
        app.status_section = if app.unstaged_indices.is_empty() && !app.staged_indices.is_empty() {
            StatusSection::Staged
        } else {
            StatusSection::Unstaged
        };
        app.selected_status = app
            .section_indices(app.status_section)
            .first()
            .copied()
            .unwrap_or(0);
        if !app.status.is_empty() {
            app.selected_statuses.insert(app.selected_status);
        }
        app.refresh_diff();
        app.refresh_diff_stats();
        app
    }

    pub fn loading(backend: Box<dyn GitBackend>) -> Self {
        let mut app = Self::new(GraphModel::default(), Vec::new(), Some(backend));
        app.loading = true;
        app.loading_started = Some(Instant::now());
        app
    }
}

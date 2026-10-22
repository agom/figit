use super::*;
use crate::core::model::{BranchTracking, Commit, RefKind, RefLabel, StatusEntry};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

#[test]
fn help_overlay_opens_and_closes() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.handle_key(KeyEvent::from(KeyCode::Char('?')));
    assert!(app.help_popup, "? should open the help overlay");
    // Navigation scrolls instead of moving the graph selection.
    app.handle_key(KeyEvent::from(KeyCode::Down));
    assert_eq!(app.help_scroll, 1);
    assert!(!app.handle_key(KeyEvent::from(KeyCode::Esc)));
    assert!(!app.help_popup, "Esc should close the help overlay");
}

#[test]
fn update_notifications_offer_install_and_restart() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.update_tx
        .send(UpdateEvent::Check {
            manual: false,
            result: Ok(UpdateAvailability::Available),
        })
        .unwrap();
    app.poll_update();
    assert_eq!(app.update_status, UpdateStatus::Available);
    assert!(app.status_line().to_string().contains("U install"));

    app.update_tx.send(UpdateEvent::Install(Ok(true))).unwrap();
    app.poll_update();
    assert_eq!(app.update_status, UpdateStatus::RestartRequired);
    assert!(app.status_line().to_string().contains("quit and relaunch"));
}

#[test]
fn stale_history_page_does_not_modify_a_reloaded_graph() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    let original_commits = app.graph.commits.clone();
    app.graph_generation = 2;
    app.graph_page_in_progress = true;
    app.graph_page_tx
        .send(GraphPageCompletion {
            skip: original_commits.len(),
            generation: 1,
            result: Ok(GraphWindow {
                commits: vec![c("stale", &[], "stale page", 0)],
                refs: Vec::new(),
                branch_tracking: Vec::new(),
                head_id: None,
                head_name: None,
                complete: false,
            }),
        })
        .unwrap();

    app.poll_graph_page();

    assert_eq!(app.graph.commits, original_commits);
    assert!(app.graph_page_in_progress);
}

#[test]
fn large_diffs_are_truncated_until_explicitly_loaded() {
    let mut app = App::new(GraphModel::default(), Vec::new(), None);
    let mut diff = String::from("@@ -1,1 +1,1 @@\n");
    for index in 0..(MAX_RENDERED_DIFF_LINES + 500) {
        diff.push_str(&format!("+line {index}\n"));
    }
    let total = diff.lines().count();

    app.apply_diff_text(diff.clone());
    assert_eq!(
        app.diff_truncated,
        Some((MAX_RENDERED_DIFF_LINES, total)),
        "an oversized diff should be capped with a notice"
    );
    assert!(
        app.diff_lines.len() <= MAX_RENDERED_DIFF_LINES,
        "truncated rendering must not parse the whole diff"
    );

    app.diff_full = true;
    app.apply_diff_text(diff);
    assert!(
        app.diff_truncated.is_none(),
        "loading the full diff clears the notice"
    );
    assert!(app.diff_lines.len() > MAX_RENDERED_DIFF_LINES);
}

#[test]
fn watcher_ignores_git_index_writes_but_not_worktree_changes() {
    let git_dir = Path::new("/repo/.git");
    // `git status` rewrites the index; reacting would loop forever.
    assert!(is_ignored_watch_path(&git_dir.join("index"), Some(git_dir)));
    assert!(is_ignored_watch_path(
        &git_dir.join("index.lock"),
        Some(git_dir)
    ));
    assert!(is_ignored_watch_path(
        &git_dir.join("hooks/update.lock"),
        Some(git_dir)
    ));
    // Real repository changes still trigger a refresh.
    assert!(!is_ignored_watch_path(&git_dir.join("HEAD"), Some(git_dir)));
    assert!(!is_ignored_watch_path(
        &git_dir.join("refs/heads/main"),
        Some(git_dir)
    ));
    assert!(!is_ignored_watch_path(
        Path::new("/repo/src/main.rs"),
        Some(git_dir)
    ));
    // A worktree file that merely shares the name must not be ignored.
    assert!(!is_ignored_watch_path(
        Path::new("/repo/index"),
        Some(git_dir)
    ));
}

#[test]
fn contextual_hints_fit_and_keep_navigation_discoverable() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    for focus in [Focus::Graph, Focus::Status, Focus::Diff, Focus::Details] {
        app.focus = focus;
        for width in 60..140 {
            let hints = app.context_hints(width);
            assert!(hints.width() <= width);
            assert!(hints.to_string().contains("? help"));
            assert!(hints.to_string().contains("z maximize"));
        }
    }
}

#[test]
fn remote_operation_shows_progress_and_keeps_graph_navigation_available() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.remote_operation = Some(RemoteOperation::Fetch("origin".into()));
    app.remote_started = Some(Instant::now());
    app.focus = Focus::Graph;

    assert!(app.status_line().to_string().contains("Fetch in progress"));
    assert!(!app.handle_key(KeyEvent::from(KeyCode::Down)));
    assert_eq!(app.selected_graph, 1);
    assert!(!app.handle_key(KeyEvent::from(KeyCode::Char('x'))));
    assert!(
        app.graph_action.is_none(),
        "mutating graph actions are ignored during remote work"
    );
}

#[test]
fn status_rows_keep_line_totals_visible_in_narrow_columns() {
    let paths = ["a.rs", "a-much-longer-file-name.rs"];
    let status = paths
        .iter()
        .map(|path| StatusEntry {
            path: (*path).to_string().into(),
            index: ' ',
            worktree: 'M',
        })
        .collect();
    let mut app = App::new(GraphModel::default(), status, None);
    app.diff_stats
        .insert((paths[0].to_string().into(), false), (2, 1));
    app.diff_stats
        .insert((paths[1].to_string().into(), false), (1, 1));
    app.diff_hunk_counts
        .insert(paths[0].to_string().into(), (1, 2));
    app.diff_hunk_counts
        .insert(paths[1].to_string().into(), (10, 12));

    for width in 7..=48 {
        let lines = paths
            .iter()
            .enumerate()
            .map(|(index, _)| app.file_status_line(index, false, width))
            .collect::<Vec<_>>();
        for line in &lines {
            assert!(line.width() <= width, "row exceeds width {width}: {line:?}");
            assert!(
                line.to_string().contains("-1"),
                "removal count clipped at width {width}: {line:?}"
            );
        }
        let removal_column = |line: &Line<'static>| {
            let text = line.to_string();
            text[..text.rfind("-1").unwrap()].chars().count()
        };
        assert_eq!(
            removal_column(&lines[0]),
            removal_column(&lines[1]),
            "removal stats drift at width {width}"
        );
    }
}

#[test]
fn devicon_colors_parse_hex() {
    assert_eq!(
        super::presentation::devicon_color("#89e051"),
        Color::Rgb(0x89, 0xe0, 0x51)
    );
    assert_eq!(
        super::presentation::devicon_color("89e051"),
        Color::Rgb(0x89, 0xe0, 0x51)
    );
    assert_eq!(super::presentation::devicon_color("bogus"), Color::Gray);
}

#[test]
fn file_type_glyphs_cover_common_categories() {
    use super::presentation::file_type_glyph;
    assert_eq!(file_type_glyph("src/main.rs"), '\u{f1c9}');
    assert_eq!(file_type_glyph("docs/CHANGELOG.md"), '\u{f15c}');
    assert_eq!(file_type_glyph("assets/logo.png"), '\u{f1c5}');
    assert_eq!(file_type_glyph("Cargo.lock"), '\u{f023}');
    assert_eq!(file_type_glyph(".gitignore"), '\u{f1d3}');
    assert_eq!(file_type_glyph("archive.tar.gz"), '\u{f1c6}');
    assert_eq!(file_type_glyph("NOTES"), '\u{f15b}');
}

#[test]
fn status_rows_show_file_type_icons() {
    let status = vec![StatusEntry {
        path: "src/main.rs".to_string().into(),
        index: ' ',
        worktree: 'M',
    }];
    let app = App::new(GraphModel::default(), status, None);
    let line = app.file_status_line(0, false, 80).to_string();
    assert!(
        line.contains('\u{f1c9}'),
        "expected the code icon in {line:?}"
    );

    let status = vec![StatusEntry {
        path: "NOTES".to_string().into(),
        index: ' ',
        worktree: 'M',
    }];
    let app = App::new(GraphModel::default(), status, None);
    let line = app.file_status_line(0, false, 80).to_string();
    assert!(
        line.contains('\u{f15b}'),
        "expected the default file icon in {line:?}"
    );
}

#[test]
fn dimmed_graph_keeps_lane_color_and_darkens_it_instead_of_graying_it() {
    let mut line = Line::from(vec![
        Span::styled("●", Style::default().fg(Color::LightCyan)),
        Span::raw(" "),
        Span::styled(
            "commit summary",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " branch ",
            Style::default().fg(Color::Black).bg(Color::LightGreen),
        ),
        Span::styled(
            " inactive pill ",
            Style::default()
                .fg(Color::Rgb(80, 80, 80))
                .bg(Color::LightBlue),
        ),
        Span::styled(
            "cap",
            Style::default()
                .fg(Color::LightBlue)
                .bg(Color::Rgb(80, 80, 80)),
        ),
    ]);

    dim_graph_line(&mut line, 0, 1, Some(Color::Rgb(80, 80, 80)), &[None; 16]);

    assert_eq!(line.spans[0].style.fg, Some(Color::Cyan));
    assert_eq!(line.spans[2].style.fg, Some(Color::DarkGray));
    assert_eq!(line.spans[3].style.bg, Some(Color::Green));
    assert_eq!(line.spans[4].style.fg, Some(Color::Rgb(40, 40, 40)));
    assert_eq!(line.spans[4].style.bg, Some(Color::Blue));
    assert_eq!(line.spans[5].style.fg, line.spans[4].style.bg);
    assert!(!line.spans[4].style.add_modifier.contains(Modifier::DIM));
    assert!(!line.spans[5].style.add_modifier.contains(Modifier::DIM));
}

#[test]
fn dim_palette_uses_terminal_theme_colors() {
    let expected = [
        Color::Blue,
        Color::Green,
        Color::Yellow,
        Color::Magenta,
        Color::Cyan,
        Color::Red,
        Color::Gray,
    ];
    let mut theme: TerminalAnsiPalette = [None; 16];
    theme[12] = Some(Color::Rgb(140, 90, 220));
    for (active, dimmed) in PALETTE.into_iter().zip(expected) {
        assert_eq!(darker_color(active, &[None; 16]), dimmed);
    }
    assert_eq!(
        darker_color(Color::LightBlue, &theme),
        Color::Rgb(70, 45, 110)
    );
}

#[test]
fn conflict_side_border_dims_only_the_non_selected_commit_color() {
    let mut theme: TerminalAnsiPalette = [None; 16];
    theme[12] = Some(Color::Rgb(140, 90, 220));

    let selected = conflict_side_border_style(Color::LightBlue, true, &theme);
    let unselected = conflict_side_border_style(Color::LightBlue, false, &theme);

    assert_eq!(selected.fg, Some(Color::LightBlue));
    assert!(selected.add_modifier.contains(Modifier::BOLD));
    assert_eq!(unselected.fg, Some(Color::Rgb(70, 45, 110)));
    assert!(!unselected.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn current_branch_includes_commits_reachable_through_merge_parents() {
    let mut graph = synthetic_graph();
    graph.commits.push(c("unmerged", &[], "unmerged", 0));

    let current = current_branch_ids(&graph);

    assert!(current.contains("m"));
    assert!(current.contains("l"));
    assert!(current.contains("r"));
    assert!(current.contains("b"));
    assert!(!current.contains("unmerged"));
}

#[test]
fn horizontal_graph_scroll_keeps_selected_commit_node_visible() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    let (row_index, node_col) = app
        .rows
        .iter()
        .enumerate()
        .find_map(|(index, row)| row.node_col.filter(|&col| col > 0).map(|col| (index, col)))
        .expect("fixture has a commit on a non-first lane");

    app.select_graph_index(row_index);
    for _ in 0..node_col + 2 {
        app.handle_key(KeyEvent::from(KeyCode::Right));
    }
    assert_eq!(app.scroll_x, node_col);

    app.select_graph_index(0);
    assert!(app.scroll_x <= app.selected_graph_node_col().unwrap_or(0));
}

#[test]
fn graph_action_popup_is_modal_to_global_shortcuts() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.graph_action = Some(GraphAction::Branch);
    app.focus = Focus::Commit;

    assert!(!app.handle_key(KeyEvent::from(KeyCode::Char('q'))));
    assert_eq!(app.graph_action_input, "q");
    let selected = app.selected_graph;
    assert!(!app.handle_key(KeyEvent::from(KeyCode::Down)));
    assert_eq!(app.selected_graph, selected);
    assert!(app.graph_action.is_some());
}

#[test]
fn popup_options_use_arrows_while_name_fields_accept_hjkl() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.graph_action = Some(GraphAction::Reset);
    app.focus = Focus::Commit;
    app.reset_focus = 1;
    app.handle_key(KeyEvent::from(KeyCode::Char('j')));
    assert_eq!(app.reset_focus, 1);
    app.handle_key(KeyEvent::from(KeyCode::Down));
    assert_eq!(app.reset_focus, 2);

    app.graph_action = Some(GraphAction::Branch);
    app.graph_action_input.clear();
    app.commit_field = CommitField::Title;
    app.handle_key(KeyEvent::from(KeyCode::Char('j')));
    assert_eq!(app.graph_action_input, "j");
}

#[test]
fn text_fields_move_the_insertion_cursor_with_arrows() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.graph_action = Some(GraphAction::Branch);
    app.focus = Focus::Commit;
    app.graph_action_input = "branch".into();
    app.graph_action_cursor = 6;

    app.handle_key(KeyEvent::from(KeyCode::Left));
    app.handle_key(KeyEvent::from(KeyCode::Char('X')));
    assert_eq!(app.graph_action_input, "brancXh");
    app.handle_key(KeyEvent::from(KeyCode::Backspace));
    assert_eq!(app.graph_action_input, "branch");

    app.graph_action = None;
    app.commit_popup = true;
    app.commit_field = CommitField::Title;
    app.commit_title = "abcd".into();
    app.commit_title_cursor = 2;
    app.handle_key(KeyEvent::from(KeyCode::Right));
    app.handle_key(KeyEvent::from(KeyCode::Char('X')));
    assert_eq!(app.commit_title, "abcXd");

    app.commit_field = CommitField::Description;
    app.commit_description = "one\ntwo".into();
    app.commit_description_cursor = app.commit_description.chars().count();
    app.handle_key(KeyEvent::from(KeyCode::Up));
    app.handle_key(KeyEvent::from(KeyCode::Char('X')));
    assert_eq!(app.commit_description, "oneX\ntwo");
}

#[test]
fn reword_and_all_name_popups_edit_at_the_arrow_cursor() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.handle_key(KeyEvent::from(KeyCode::Char('w')));
    assert!(matches!(app.commit_mode, CommitMode::Reword(_)));
    app.commit_title = "message".into();
    app.commit_title_cursor = 3;
    app.handle_key(KeyEvent::from(KeyCode::Left));
    app.handle_key(KeyEvent::from(KeyCode::Char('X')));
    assert_eq!(app.commit_title, "meXssage");
    assert_eq!(app.commit_title_cursor, 3);

    app.commit_field = CommitField::Description;
    app.commit_description = "body".into();
    app.commit_description_cursor = 2;
    app.handle_key(KeyEvent::from(KeyCode::Right));
    app.handle_key(KeyEvent::from(KeyCode::Char('X')));
    assert_eq!(app.commit_description, "bodXy");

    app.commit_popup = false;
    app.graph_action = Some(GraphAction::Branch);
    app.commit_field = CommitField::Title;
    for action in [GraphAction::Branch, GraphAction::Tag, GraphAction::Rename] {
        app.graph_action = Some(action);
        app.graph_action_input = "branch".into();
        app.graph_action_cursor = 3;
        app.handle_key(KeyEvent::from(KeyCode::Left));
        app.handle_key(KeyEvent::from(KeyCode::Char('X')));
        assert_eq!(app.graph_action_input, "brXanch", "for {action:?}");
        assert_eq!(app.graph_action_cursor, 3, "for {action:?}");
    }
}

#[test]
fn graph_action_popups_ignore_commit_editor_keys() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.graph_action = Some(GraphAction::Fetch);
    app.focus = Focus::Commit;
    app.commit_title = "keep".into();
    app.commit_description = "body".into();
    app.commit_field = CommitField::Title;

    app.handle_key(KeyEvent::from(KeyCode::Char('z')));
    app.handle_key(KeyEvent::from(KeyCode::Backspace));
    app.handle_key(KeyEvent::from(KeyCode::F(2)));
    assert_eq!(app.commit_title, "keep");
    assert_eq!(app.commit_description, "body");
}

#[test]
fn page_down_moves_the_focused_selection() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.focus = Focus::Graph;
    let before = app.selected_graph;
    app.handle_key(KeyEvent::from(KeyCode::PageDown));
    assert!(
        app.selected_graph > before,
        "PageDown should page the graph selection"
    );
    app.handle_key(KeyEvent::from(KeyCode::PageUp));
    assert_eq!(app.selected_graph, before);
}

#[test]
fn rebase_plan_actions_and_reordering_are_keyboard_editable() {
    let entry = |id: &str, action| RebasePlanEntry {
        id: id.into(),
        subject: format!("{id} subject"),
        tail: format!("{id} subject"),
        action,
        title: format!("{id} subject"),
        description: String::new(),
    };
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.rebase_plan = Some(RebasePlanState {
        plan: RebasePlan {
            target: "a".into(),
            upstream: Some("parent".into()),
            blocks: vec![
                RebasePlanBlock::Control("label onto".into()),
                RebasePlanBlock::Commits(vec![
                    entry("first", RebaseTodoAction::Pick),
                    entry("second", RebaseTodoAction::Pick),
                ]),
                RebasePlanBlock::Merge(entry("merge", RebaseTodoAction::Merge)),
            ],
        },
        selected: 0,
    });

    app.handle_rebase_plan_key(KeyEvent::from(KeyCode::Char('e')));
    assert_eq!(
        plan_entry_at(&app.rebase_plan.as_ref().unwrap().plan, 0)
            .unwrap()
            .action,
        RebaseTodoAction::Edit
    );
    app.handle_rebase_plan_key(KeyEvent::from(KeyCode::Down));
    app.handle_rebase_plan_key(KeyEvent::from(KeyCode::Char('s')));
    assert_eq!(
        plan_entry_at(&app.rebase_plan.as_ref().unwrap().plan, 1)
            .unwrap()
            .action,
        RebaseTodoAction::Squash
    );
    app.handle_rebase_plan_key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
    let plan = &app.rebase_plan.as_ref().unwrap().plan;
    let RebasePlanBlock::Commits(entries) = &plan.blocks[1] else {
        panic!("commit block")
    };
    assert_eq!(entries[0].id, "second");
    assert_eq!(entries[1].id, "first");

    app.handle_rebase_plan_key(KeyEvent::from(KeyCode::Down));
    app.handle_rebase_plan_key(KeyEvent::from(KeyCode::Down));
    app.handle_rebase_plan_key(KeyEvent::from(KeyCode::Char('e')));
    assert!(app
        .action_error
        .as_deref()
        .unwrap()
        .contains("Merge commits are fixed"));
}

#[test]
fn reword_opens_editor_for_selected_merge_commit() {
    let mut graph = synthetic_graph();
    graph.commits[0].message = "Original body\n".into();
    let mut app = App::new(graph, Vec::new(), None);

    app.handle_key(KeyEvent::from(KeyCode::Char('w')));

    assert!(app.commit_popup);
    assert_eq!(app.commit_title, "merge");
    assert_eq!(app.commit_description, "Original body");
    assert_eq!(app.commit_mode, CommitMode::Reword("m".into()));
}

#[test]
fn edit_shortcut_opens_confirmation_for_selected_merge_commit() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);

    app.handle_key(KeyEvent::from(KeyCode::Char('e')));
    assert!(app.commit_popup);
    assert!(app.commit_confirm);
    assert_eq!(app.commit_mode, CommitMode::Edit("m".into()));

    app.handle_key(KeyEvent::from(KeyCode::Esc));
    assert!(!app.commit_popup);
    assert_eq!(app.focus, Focus::Graph);
}

#[test]
fn reword_and_edit_explain_when_graph_is_not_focused() {
    let mut app = App::new(
        synthetic_graph(),
        vec![StatusEntry {
            path: "file".into(),
            index: ' ',
            worktree: 'M',
        }],
        None,
    );

    app.handle_key(KeyEvent::from(KeyCode::Char('w')));
    assert!(app
        .action_error
        .as_deref()
        .unwrap()
        .contains("Focus the Graph"));
    app.handle_key(KeyEvent::from(KeyCode::Tab));
    app.handle_key(KeyEvent::from(KeyCode::Char('e')));
    assert!(app
        .action_error
        .as_deref()
        .unwrap()
        .contains("Select a commit row"));
}

fn c(id: &str, parents: &[&str], summary: &str, ts: i64) -> Commit {
    Commit {
        id: id.to_string(),
        parents: parents.iter().map(|p| p.to_string()).collect(),
        summary: summary.to_string(),
        message: summary.to_string(),
        author_name: "T".into(),
        author_email: "t@e".into(),
        timestamp: ts,
        committer_timestamp: ts,
        timezone_offset: 0,
        committer_timezone_offset: 0,
    }
}

fn synthetic_graph() -> GraphModel {
    GraphModel {
        commits: vec![
            c("m", &["l", "r"], "merge", 4),
            c("l", &["b"], "left", 3),
            c("r", &["b"], "right", 2),
            c("b", &[], "base", 1),
        ],
        refs: vec![
            RefLabel {
                kind: RefKind::LocalBranch,
                name: "main".into(),
                id: "m".into(),
            },
            RefLabel {
                kind: RefKind::Tag,
                name: "v1".into(),
                id: "b".into(),
            },
        ],
        branch_tracking: Vec::new(),
        head_id: Some("m".into()),
        head_name: Some("main".into()),
    }
}

fn render(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).unwrap();
    let lines: Vec<Line> = app
        .rows
        .iter()
        .skip(app.scroll_y)
        .take(h as usize)
        .map(|row| app.render_row(row, w as usize))
        .collect();
    terminal
        .draw(|f| f.render_widget(Paragraph::new(lines), f.area()))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let width = buffer.area.width as usize;
    buffer
        .content()
        .chunks(width)
        .map(|row| {
            row.iter()
                .map(|c| c.symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// The right-hand SHA/author/date columns as rendered by `render_row`.
fn column_tail(_app: &App, commit: &Commit) -> String {
    let sha: String = commit.id.chars().take(7).collect();
    format!("{sha:>7}")
}

#[test]
fn renders_graph_rows_with_lane_glyphs_and_labels() {
    let mut app = App::new(synthetic_graph(), vec![], None);
    app.bg = Some(Color::Rgb(0, 0, 0));
    let text = render(&mut app, 50, 8);
    let (m, l, r, b) = (
        &app.graph.commits[0],
        &app.graph.commits[1],
        &app.graph.commits[2],
        &app.graph.commits[3],
    );
    assert!(
        text[0].starts_with(&format!(
            "▲─╮ {PILL_LEFT} {HEAD_BRANCH_MARKER} main {PILL_RIGHT} merge"
        )),
        "{}",
        text[0]
    );
    assert!(text[0].ends_with(&column_tail(&app, m)), "{}", text[0]);
    assert!(text[1].starts_with("● │ left"));
    assert!(text[1].ends_with(&column_tail(&app, l)), "{}", text[1]);
    assert!(text[2].starts_with("│ ● right"));
    assert!(text[2].ends_with(&column_tail(&app, r)), "{}", text[2]);
    assert!(text[3].starts_with(&format!("●─╯ {PILL_LEFT} {TAG_GLYPH} v1 {PILL_RIGHT} base")));
    assert!(text[3].ends_with(&column_tail(&app, b)), "{}", text[3]);
}

#[test]
fn compact_graph_keeps_one_row_per_commit() {
    let mut app = App::new(synthetic_graph(), vec![], None);
    let text = render(&mut app, 50, 4);
    assert_eq!(text.len(), 4);
    assert!(text[0].starts_with("▲─╮"));
    assert!(text[1].starts_with("● │"));
}

#[test]
fn branch_label_matches_lane_color() {
    let app = App::new(synthetic_graph(), vec![], None);
    assert_ne!(
        PALETTE[0],
        Color::LightCyan,
        "first branch should contrast with panel borders"
    );
    let backend = TestBackend::new(50, 8);
    let mut terminal = Terminal::new(backend).unwrap();
    let row = app.render_row(&app.rows[0], 50);
    let sha_color = row.spans.last().unwrap().style.fg;
    terminal
        .draw(|f| f.render_widget(Paragraph::new(vec![row]), f.area()))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();

    // Row 0 is the merge commit, node on lane color 0.
    let nc = app.rows[0].node_col.unwrap();
    let expected = PALETTE[app.rows[0].colors[nc] % PALETTE.len()];
    assert_eq!(
        sha_color,
        Some(Color::DarkGray),
        "ordinary commit SHAs stay gray"
    );
    let main_column = (0..buffer.area.width)
        .find(|&column| buffer.cell((column, 0)).unwrap().symbol() == "m")
        .expect("main label cell");
    let cell = buffer.cell((main_column, 0)).expect("main label cell");
    // Reverse-video pill: fg carries the branch color and is swapped to bg.
    assert_eq!(cell.style().fg, Some(expected));
    assert!(
        cell.style().add_modifier.contains(Modifier::REVERSED),
        "label must be a negative (reversed) pill"
    );
}

#[test]
fn local_branch_label_shows_ahead_and_behind_counts() {
    let mut app = App::new(synthetic_graph(), vec![], None);
    app.bg = Some(Color::Rgb(0, 0, 0));
    app.graph.branch_tracking.push(BranchTracking {
        branch: "main".into(),
        upstream: "origin/main".into(),
        ahead: 3,
        behind: 2,
        gone: false,
    });
    app.rebuild_graph_indexes();
    let row = app.render_row(&app.rows[0], 100).to_string();
    assert!(
        row.contains("main ↑3 ↓2"),
        "tracking counts missing from branch label: {row}"
    );
}

#[test]
fn local_branch_with_a_gone_upstream_is_marked() {
    let mut app = App::new(synthetic_graph(), vec![], None);
    app.bg = Some(Color::Rgb(0, 0, 0));
    app.graph.branch_tracking.push(BranchTracking {
        branch: "main".into(),
        upstream: "origin/main".into(),
        ahead: 0,
        behind: 0,
        gone: true,
    });
    app.rebuild_graph_indexes();
    let row = app.render_row(&app.rows[0], 100).to_string();
    assert!(
        row.contains("main ✗"),
        "gone upstream marker missing from branch label: {row}"
    );
}

#[test]
fn historical_graph_details_debounce_until_navigation_pauses() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.select_graph_index(1);

    assert_eq!(app.historical_commit.as_deref(), Some("l"));
    assert!(app.details_loading);
    assert!(app.graph_details_deadline.is_some());
    assert!(app.status.is_empty());
    assert!(app.diff_lines.is_empty());

    let first_deadline = app.graph_details_deadline.unwrap();
    app.select_graph_index(2);
    assert_eq!(app.historical_commit.as_deref(), Some("r"));
    assert!(
        app.graph_details_deadline.unwrap() > first_deadline,
        "moving again should reset the details debounce"
    );
}

#[test]
fn normal_diff_keeps_green_and_red() {
    assert_eq!(diff_color(DiffKind::Added), Color::LightGreen);
    assert_eq!(diff_color(DiffKind::Removed), Color::LightRed);
    assert_eq!(
        diff_background(DiffKind::Added),
        Some(Color::Rgb(38, 56, 43))
    );
    assert_eq!(
        diff_background(DiffKind::Removed),
        Some(Color::Rgb(70, 44, 49))
    );
}

#[test]
fn selected_hunk_highlight_preserves_added_and_removed_diff_backgrounds() {
    let parsed =
        parse_diff("@@ -1,2 +1,2 @@\n context\n-old\n+new\n@@ -8 +8 @@\n-last old\n+last new\n");
    let mut rendered = render_diff(&parsed, DiffMode::Unified, 60, "file.txt", 3, 0, true);
    mark_selected_diff_hunk(&mut rendered, 0);

    let context = rendered
        .iter()
        .find(|line| line.to_string().contains("context"))
        .unwrap();
    assert_eq!(context.spans[1].style.bg, Some(Color::Rgb(45, 50, 65)));
    let removed = rendered
        .iter()
        .find(|line| line.to_string().contains("-old"))
        .unwrap();
    assert_eq!(removed.spans[1].style.bg, Some(Color::Rgb(70, 44, 49)));
    let added = rendered
        .iter()
        .find(|line| line.to_string().contains("+new"))
        .unwrap();
    assert_eq!(added.spans[1].style.bg, Some(Color::Rgb(38, 56, 43)));
    let second_hunk = rendered
        .iter()
        .find(|line| line.to_string().contains("last old"))
        .unwrap();
    assert!(!second_hunk
        .spans
        .iter()
        .any(|span| { span.style.bg == Some(Color::Rgb(45, 50, 65)) }));
}

#[test]
fn inline_emphasis_skips_unrelated_line_pairs() {
    let (old, new) = inline_change_ranges(
        "Focus::Graph => {",
        "// The commit-details pane is always highlighted for the",
    );
    assert!(
        old.is_empty() && new.is_empty(),
        "unrelated lines should not get token-level emphasis"
    );
}

#[test]
fn inline_emphasis_marks_whole_tokens() {
    let (old, new) = inline_change_ranges("Focus::Status", "Focus::Details");
    assert_eq!(old, vec![(7, 13)], "`Status` is emphasized as one token");
    assert_eq!(new, vec![(7, 14)], "`Details` is emphasized as one token");

    let (old, new) = inline_change_ranges("a == b", "a != b");
    assert_eq!(old, vec![(2, 4)], "`==` is emphasized as one token");
    assert_eq!(new, vec![(2, 4)], "`!=` is emphasized as one token");
}

#[test]
fn replaced_lines_pair_by_position_when_line_numbers_shift() {
    let parsed = parse_diff("@@ -10 +20 @@\n-let x = Graph;\n+let x = Diff;\n");
    let rendered = render_diff(&parsed, DiffMode::Unified, 60, "file.txt", 3, 0, true);
    let emphasized = |color: Color| {
        rendered
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| span.style.bg == Some(color))
            .map(|span| span.content.to_string())
            .collect::<String>()
    };
    assert!(
        emphasized(Color::Rgb(107, 60, 66)).contains("Graph"),
        "removed `Graph` should be emphasized despite the shifted line number"
    );
    assert!(
        emphasized(Color::Rgb(54, 91, 60)).contains("Diff"),
        "added `Diff` should be emphasized despite the shifted line number"
    );
}

#[test]
fn unified_diff_keeps_removed_lines_before_added_lines() {
    let parsed = parse_diff("@@ -1,3 +1,3 @@\n-a\n-b\n-c\n+x\n+y\n+z\n");
    let rows = diff_layout(&parsed, DiffMode::Unified);
    let order: Vec<&str> = rows
        .iter()
        .map(|row| match row {
            DiffRow::Unified { index, .. } => parsed[*index].text.as_str(),
            DiffRow::Side { .. } => unreachable!("unified layout returned a side row"),
        })
        .collect();
    assert_eq!(
        order,
        ["@@ -1,3 +1,3 @@", "-a", "-b", "-c", "+x", "+y", "+z"]
    );
}

#[test]
fn skip_columns_drops_display_columns() {
    assert_eq!(skip_columns("abcdef", 0), "abcdef");
    assert_eq!(skip_columns("abcdef", 2), "cdef");
    assert_eq!(skip_columns("abcdef", 6), "");
    assert_eq!(skip_columns("abcdef", 9), "");
    assert_eq!(skip_columns("héllo", 2), "llo");
}

#[test]
fn diff_panning_is_clamped_to_content_width() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.diff_area = Rect::new(0, 0, 40, 6);
    app.diff_number_width = 3;

    app.set_diff_lines(parse_diff("@@ -1 +1 @@\n-short\n+short\n"));
    app.pan_diff_horizontal(4);
    assert_eq!(app.diff_scroll_x, 0, "short lines never need panning");

    let long = "x".repeat(200);
    app.set_diff_lines(parse_diff(&format!("@@ -1 +1 @@\n-{long}\n+{long}\n")));
    app.pan_diff_horizontal(4);
    assert_eq!(app.diff_scroll_x, 4);
    for _ in 0..200 {
        app.pan_diff_horizontal(4);
    }
    let clamped = app.diff_scroll_x;
    assert!(clamped > 4, "a long line should keep panning");
    app.pan_diff_horizontal(4);
    assert_eq!(
        app.diff_scroll_x, clamped,
        "panning clamps at the widest line"
    );
    app.pan_diff_horizontal(-1000);
    assert_eq!(app.diff_scroll_x, 0, "panning back reaches the left edge");
}

#[test]
fn large_diffs_still_syntax_highlight_their_visible_rows() {
    // Well past the old 2,000-line cutoff that disabled highlighting entirely.
    let mut diff = String::from("@@ -1,5000 +1,5000 @@\n");
    for i in 0..5_000 {
        diff.push_str(&format!(
            " fn function_{i}(value: usize) -> usize {{ value }}\n"
        ));
    }
    let lines = parse_diff(&diff);
    assert!(lines.len() > 5_000);

    let row = DiffRow::Unified {
        index: 2,
        partner: None,
    };
    let highlighted = render_diff_row(&lines, row, 80, "big.rs", 4, 0, true);
    let plain = render_diff_row(&lines, row, 80, "big.rs", 4, 0, false);
    assert!(
        highlighted
            .spans
            .iter()
            .any(|span| matches!(span.style.fg, Some(Color::Rgb(..)))),
        "a visible row of a large diff should be syntax highlighted"
    );
    assert!(
        !plain
            .spans
            .iter()
            .any(|span| matches!(span.style.fg, Some(Color::Rgb(..)))),
        "the plain fallback should use diff colors, not syntax colors"
    );
}

#[test]
fn hunk_wrap_keeps_diff_scroll_inside_the_rendered_viewport() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.diff_area = Rect::new(0, 0, 60, 6);
    app.set_diff_lines(parse_diff(
        "@@ -1 +1 @@\n-first\n+first changed\n\
             context 1\n context 2\n context 3\n context 4\n context 5\n\
             @@ -10 +10 @@\n-last\n+last changed\n",
    ));

    app.move_diff_hunk(1);
    assert_eq!(app.selected_diff_hunk, 1);
    app.move_diff_hunk(1);
    assert_eq!(
        app.selected_diff_hunk, 0,
        "moving down from the last hunk wraps to the first"
    );
    assert_eq!(
        app.diff_scroll, 0,
        "wrapped hunk should render at the top without an overscroll frame"
    );
}

#[test]
fn hunk_navigation_scrolls_to_the_adjacent_hunk() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.diff_area = Rect::new(0, 0, 60, 4);
    app.set_diff_lines(parse_diff(
        "@@ -1 +1 @@\n-first\n+first changed\n\
             context 1\n context 2\n\
             @@ -10 +10 @@\n-second\n+second changed\n\
             @@ -20 +20 @@\n-third\n+third changed\n",
    ));

    // The three hunk headers land on rendered rows 0, 5 and 8.
    assert_eq!(app.diff_scroll, 0);
    app.jump_diff_hunk(1);
    assert_eq!(app.diff_scroll, 5, "`]` jumps to the next hunk header");
    app.jump_diff_hunk(1);
    assert_eq!(app.diff_scroll, 8, "`]` jumps to the last hunk header");
    app.jump_diff_hunk(1);
    assert_eq!(
        app.diff_scroll, 0,
        "`]` from the last hunk wraps to the first"
    );
    app.jump_diff_hunk(-1);
    assert_eq!(
        app.diff_scroll, 8,
        "`[` from the first hunk wraps to the last"
    );
}

#[test]
fn only_conflicting_commit_shas_use_ours_blue_and_theirs_yellow() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.conflict_commits = Some(ConflictCommits {
        ours: "m".into(),
        theirs: "r".into(),
        base: Some("b".into()),
    });
    for row in &app.rows {
        let id = &app.graph.commits[row.commit_index].id;
        let color = app.render_row(row, 50).spans.last().unwrap().style.fg;
        let expected = if id == "m" {
            Color::LightBlue
        } else if id == "r" {
            Color::LightYellow
        } else if id == "b" {
            Color::LightMagenta
        } else {
            Color::DarkGray
        };
        assert_eq!(color, Some(expected), "SHA color for {id}");
    }
}

#[test]
fn conflict_sides_get_distinct_colors_even_on_one_lane() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    assert_eq!(app.graph_lane_color("m"), app.graph_lane_color("l"));
    app.conflict_commits = Some(ConflictCommits {
        ours: "m".into(),
        theirs: "l".into(),
        base: Some("b".into()),
    });
    let (ours, theirs) = app.conflict_colors().unwrap();
    assert_eq!(ours, Color::LightBlue);
    assert_eq!(theirs, Color::LightYellow);
    assert_eq!(app.conflict_commit_color("m"), Some(ours));
    assert_eq!(app.conflict_commit_color("l"), Some(theirs));
    assert_eq!(app.conflict_commit_color("b"), Some(Color::LightMagenta));
    assert_eq!(app.conflict_commit_color("r"), None);
}

#[test]
fn ours_sha_stays_blue_when_it_is_also_the_merge_base() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.conflict_commits = Some(ConflictCommits {
        ours: "m".into(),
        theirs: "r".into(),
        base: Some("m".into()),
    });

    assert_eq!(app.conflict_commit_color("m"), Some(Color::LightBlue));
    assert_eq!(app.conflict_commit_color("r"), Some(Color::LightYellow));
}

#[test]
fn conflict_diff_tracks_ours_and_theirs_commit_colors() {
    let diff = "Conflict stages for file.txt (ours = current base; theirs = incoming commit)\n\
Diff: base → ours (current base)\n@@ -1 +1 @@\n-base\n+ours\n\
Diff: base → theirs (incoming commit)\n@@ -1 +1 @@\n-base\n+theirs\n\
Diff: ours → theirs\n@@ -1 +1 @@\n-ours\n+theirs\n";
    let lines = parse_diff(diff);
    let rendered = render_conflict_diff(
        &lines,
        "file.txt",
        3,
        Color::LightBlue,
        Color::LightYellow,
        true,
    );
    let color_for = |text: &str, occurrence: usize| {
        rendered
            .iter()
            .filter(|line| line.to_string().contains(text))
            .nth(occurrence)
            .unwrap()
            .spans[1]
            .style
    };
    assert_eq!(color_for("+ours", 0).fg, Some(Color::LightBlue));
    assert_eq!(color_for("+theirs", 0).fg, Some(Color::LightYellow));
    assert_eq!(color_for("-ours", 0).fg, Some(Color::LightBlue));
    assert_eq!(color_for("+theirs", 1).fg, Some(Color::LightYellow));
    assert_eq!(color_for("+theirs", 0).bg, Some(Color::Reset));
    assert_eq!(diff_color(DiffKind::Added), Color::LightGreen);
    assert_eq!(diff_color(DiffKind::Removed), Color::LightRed);
}

#[test]
fn conflict_result_preview_is_a_base_relative_diff() {
    let diff = conflict_result_diff(
        "base line\nunchanged\n",
        "chosen line\nunchanged\n",
        "Result",
    );
    assert_eq!(diff[0].kind, DiffKind::Meta);
    assert_eq!(diff[0].text, "Base → Result");
    assert!(diff
        .iter()
        .any(|line| { line.kind == DiffKind::Removed && line.text == "-base line" }));
    assert!(diff
        .iter()
        .any(|line| { line.kind == DiffKind::Added && line.text == "+chosen line" }));
    assert!(diff
        .iter()
        .any(|line| { line.kind == DiffKind::Context && line.text == " unchanged" }));
}

#[test]
fn conflict_side_diffs_recolor_standard_diff_changes_with_the_side_color() {
    let diff = conflict_result_diff("base\n", "ours\n", "Ours");
    let rendered = render_conflict_side_diff(
        &diff,
        "file.txt",
        3,
        40,
        DiffMode::Unified,
        Color::LightBlue,
        &[None; 16],
    );
    let removed = rendered
        .iter()
        .find(|line| line.to_string().contains("-base"))
        .unwrap();
    let added = rendered
        .iter()
        .find(|line| line.to_string().contains("+ours"))
        .unwrap();
    assert_eq!(removed.spans[1].style.fg, Some(Color::LightRed));
    assert_eq!(removed.spans[1].style.bg, Some(Color::Rgb(70, 44, 49)));
    assert_eq!(added.spans[1].style.fg, Some(Color::LightBlue));
    assert_eq!(added.spans[1].style.bg, Some(Color::Blue));
}

#[test]
fn selected_conflict_diff_uses_blue_and_yellow_commit_colors() {
    let mut app = App::new(
        synthetic_graph(),
        vec![StatusEntry {
            path: "file.txt".into(),
            index: 'U',
            worktree: 'U',
        }],
        None,
    );
    app.conflict_commits = Some(ConflictCommits {
        ours: "m".into(),
        theirs: "r".into(),
        base: Some("b".into()),
    });
    app.set_diff_lines(parse_diff(
        "Diff: ours → theirs\n@@ -1 +1 @@\n-ours\n+theirs\n",
    ));
    let ours = Color::LightBlue;
    let theirs = Color::LightYellow;
    let lines = render_conflict_diff(
        &app.diff_lines,
        "",
        app.diff_number_width,
        ours,
        theirs,
        true,
    );
    let removed = lines
        .iter()
        .find(|line| line.to_string().contains("-ours"))
        .unwrap();
    let added = lines
        .iter()
        .find(|line| line.to_string().contains("+theirs"))
        .unwrap();
    assert_eq!(removed.spans[1].style.fg, Some(ours));
    assert_eq!(added.spans[1].style.fg, Some(theirs));
}

fn render_full_app(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let columns = buffer.area.width as usize;
    buffer
        .content()
        .chunks(columns)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn graph_hidden_layout_keeps_a_full_width_diff_and_stable_boundary() {
    let mut app = App::new(
        synthetic_graph(),
        vec![
            StatusEntry {
                path: "a.txt".into(),
                index: ' ',
                worktree: 'M',
            },
            StatusEntry {
                path: "b.txt".into(),
                index: 'M',
                worktree: ' ',
            },
        ],
        None,
    );
    app.graph_visible = false;
    for (width, height) in [(80, 24), (120, 40), (24, 8)] {
        render_full_app(&mut app, width, height);
        assert_eq!(app.diff_area.width, width);
        assert_eq!(app.diff_area.y, app.staging_area.bottom());
        let [unstaged, staged] = app.staging_panes(app.staging_area);
        assert_eq!(unstaged.y, staged.y);
        assert_eq!(unstaged.right(), staged.x);
        let boundary = app.diff_area.y;
        app.staged_indices.clear();
        render_full_app(&mut app, width, height);
        assert_eq!(app.diff_area.y, boundary);
        app.refresh_status_indices();
    }
    app.historical_commit = Some("m".into());
    render_full_app(&mut app, 120, 40);
    assert_eq!(app.staging_area.y, app.details_area.y);
    assert_eq!(app.staging_area.right(), app.details_area.x);
    assert_eq!(app.details_area.bottom(), app.diff_area.y);
}

#[test]
fn hidden_graph_tab_order_follows_the_lists_then_diff_and_skips_empty_lists() {
    let mut app = App::new(
        synthetic_graph(),
        vec![
            StatusEntry {
                path: "a.txt".into(),
                index: ' ',
                worktree: 'M',
            },
            StatusEntry {
                path: "b.txt".into(),
                index: 'M',
                worktree: ' ',
            },
        ],
        None,
    );
    app.graph_visible = false;
    app.focus = Focus::Status;
    app.status_section = StatusSection::Unstaged;
    app.cycle_panel_focus(true);
    assert_eq!(
        (app.focus, app.status_section),
        (Focus::Status, StatusSection::Staged)
    );
    app.cycle_panel_focus(true);
    assert_eq!(app.focus, Focus::Diff);
    app.cycle_panel_focus(false);
    assert_eq!(
        (app.focus, app.status_section),
        (Focus::Status, StatusSection::Staged)
    );
    app.staged_indices.clear();
    app.focus = Focus::Diff;
    app.cycle_panel_focus(true);
    assert_eq!(
        (app.focus, app.status_section),
        (Focus::Status, StatusSection::Unstaged)
    );
    app.cycle_panel_focus(true);
    assert_eq!(app.focus, Focus::Diff);
    app.historical_commit = Some("m".into());
    app.focus = Focus::Status;
    app.cycle_panel_focus(true);
    assert_eq!(app.focus, Focus::Details);
    app.cycle_panel_focus(true);
    assert_eq!(app.focus, Focus::Diff);
}

#[test]
fn maximizing_and_graph_toggles_preserve_diff_state() {
    let mut app = App::new(
        synthetic_graph(),
        vec![StatusEntry {
            path: "a.txt".into(),
            index: ' ',
            worktree: 'M',
        }],
        None,
    );
    app.focus = Focus::Diff;
    app.diff_scroll = 4;
    app.diff_scroll_x = 8;
    app.diff_mode = DiffMode::SideBySide;
    app.handle_key(KeyEvent::from(KeyCode::Char('g')));
    assert_eq!(app.diff_scroll, 4);
    assert_eq!(app.diff_scroll_x, 8);
    assert_eq!(app.diff_mode, DiffMode::SideBySide);
    app.handle_key(KeyEvent::from(KeyCode::Char('z')));
    render_full_app(&mut app, 120, 40);
    assert_eq!(app.diff_area, Rect::new(0, 1, 120, 38));
    assert!(app.staging_area.is_empty());
    app.handle_key(KeyEvent::from(KeyCode::Char('z')));
    render_full_app(&mut app, 120, 40);
    assert!(!app.staging_area.is_empty());
    assert_eq!(app.diff_mode, DiffMode::SideBySide);
}

#[test]
fn help_overlay_clears_the_panels_behind_it() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.handle_key(KeyEvent::from(KeyCode::Char('?')));
    assert!(app.help_popup);
    let screen = render_full_app(&mut app, 120, 40);
    assert!(
        screen.contains("Help — keys"),
        "help box expected:\n{screen}"
    );
    assert!(
        !screen.contains("┌Graph") && !screen.contains("Tab focus"),
        "the panels behind the help overlay should be cleared:\n{screen}"
    );
}

#[test]
fn file_rows_hide_unknown_diff_totals() {
    let mut app = App::new(
        synthetic_graph(),
        vec![StatusEntry {
            path: "changed.txt".into(),
            index: ' ',
            worktree: 'M',
        }],
        None,
    );
    app.focus = Focus::Status;
    app.refresh_status_indices();
    // No `diff_stats` entry yet: the async totals have not arrived.
    let screen = render_full_app(&mut app, 120, 40);
    assert!(screen.contains("changed.txt"));
    assert!(
        !screen.contains("+0 -0"),
        "unknown totals should render blank, not zero:\n{screen}"
    );
}

#[test]
fn conflicts_do_not_enable_commit_actions() {
    let mut app = App::new(
        synthetic_graph(),
        vec![StatusEntry {
            path: "a.txt".into(),
            index: 'U',
            worktree: 'U',
        }],
        None,
    );
    assert!(
        !app.commit_actions_available(),
        "an unresolved conflict is not a staged change"
    );
    app.local_status = vec![StatusEntry {
        path: "a.txt".into(),
        index: 'M',
        worktree: ' ',
    }];
    assert!(app.commit_actions_available());
}

#[test]
fn conflicted_files_advertise_conflict_keys_instead_of_staging() {
    let mut app = App::new(
        synthetic_graph(),
        vec![StatusEntry {
            path: "a.txt".into(),
            index: 'U',
            worktree: 'U',
        }],
        None,
    );
    app.focus = Focus::Status;
    app.status_section = StatusSection::Unstaged;
    app.selected_status = 0;
    app.refresh_status_indices();
    let screen = render_full_app(&mut app, 120, 40);
    assert!(
        screen.contains("o/t preview side"),
        "conflict keys expected:\n{screen}"
    );
    assert!(
        !screen.contains("Space stage") && !screen.contains("x discard"),
        "a conflicted file must not advertise staging keys:\n{screen}"
    );
}

#[test]
fn three_way_rows_align_independent_insertions_and_replacements() {
    let versions = crate::backend::ConflictVersions {
        base: Some(b"first\nshared\nlast\n".to_vec()),
        ours: Some(b"first\nours\nextra ours\nlast\n".to_vec()),
        theirs: Some(b"first\nshared\nlast\nextra theirs\n".to_vec()),
        working: None,
    };
    let rows = three_way_rows(&versions);
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].base.as_ref().unwrap().text, "first");
    assert_eq!(rows[1].ours.as_ref().unwrap().text, "ours");
    assert_eq!(rows[1].theirs.as_ref().unwrap().text, "shared");
    assert!(rows[2].base.is_none());
    assert_eq!(rows[2].ours.as_ref().unwrap().text, "extra ours");
    assert!(rows[2].theirs.is_none());
    assert_eq!(rows[3].base.as_ref().unwrap().text, "last");
    assert_eq!(rows[4].theirs.as_ref().unwrap().text, "extra theirs");
    assert_eq!(rows[4].theirs.as_ref().unwrap().number, 4);
}

#[test]
fn three_way_rows_show_missing_conflict_stages_without_collapsing_columns() {
    let versions = crate::backend::ConflictVersions {
        base: Some(b"original\n".to_vec()),
        ours: None,
        theirs: Some(b"incoming\n".to_vec()),
        working: None,
    };
    let rows = three_way_rows(&versions);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].base.as_ref().unwrap().text, "original");
    assert!(rows[0].ours.is_none());
    assert_eq!(rows[0].theirs.as_ref().unwrap().text, "incoming");
}

#[test]
fn conflict_hunks_can_be_selected_independently() {
    let conflicted = "before\n<<<<<<< ours\nours one\n||||||| base\nbase one\n=======\ntheirs one\n>>>>>>> theirs\nbetween\n<<<<<<< ours\nours two\n=======\ntheirs two\n>>>>>>> theirs\nafter\n";
    let hunks = conflict_hunks(conflicted);
    assert_eq!(hunks.len(), 2);
    assert_eq!(hunks[0].ours, "ours one\n");
    assert_eq!(hunks[0].base.as_deref(), Some("base one\n"));
    assert_eq!(hunks[0].theirs, "theirs one\n");

    let first = replace_conflict_hunk(conflicted, 0, ConflictSide::Theirs).unwrap();
    assert_eq!(conflict_hunks(&first).len(), 1);
    assert!(first.contains("theirs one\nbetween\n<<<<<<< ours\n"));
    let second = replace_conflict_hunk(&first, 0, ConflictSide::Ours).unwrap();
    assert!(conflict_hunks(&second).is_empty());
    assert_eq!(second, "before\ntheirs one\nbetween\nours two\nafter\n");
}

#[test]
fn three_way_columns_fit_their_terminal_cell_width() {
    let cell = ThreeWayCell {
        number: 1234,
        text: "漢字 and more text".into(),
    };
    let line = three_way_column(Some(&cell), None, "file.txt", 12, 0, Color::LightBlue);
    assert_eq!(line.width(), 12);
    assert_eq!(Line::from(fit_to_cells("漢字 and more text", 5)).width(), 5);
}

#[test]
fn three_way_source_columns_use_syntax_colors() {
    let cell = ThreeWayCell {
        number: 1,
        text: "fn main() { let count = 3; }".into(),
    };
    let line = three_way_column(Some(&cell), None, "src/main.rs", 80, 0, Color::LightMagenta);
    let syntax_colors = line
        .spans
        .iter()
        .skip(1)
        .filter_map(|span| span.style.fg)
        .collect::<HashSet<_>>();
    assert!(
        syntax_colors.len() > 1,
        "expected token-level source colors: {line:?}"
    );
}

#[test]
fn repository_header_reports_head_and_count() {
    let app = App::new(synthetic_graph(), vec![], None);
    let text = app.repository_header(120).to_string();
    assert!(text.contains("main"), "status shows branch: {text}");
    assert!(text.contains("4 commits"), "status shows count: {text}");
}

#[test]
fn scrolling_reveals_later_rows() {
    let mut app = App::new(synthetic_graph(), vec![], None);
    app.bg = Some(Color::Rgb(0, 0, 0));
    app.scroll_y = 2;
    // height 3 -> 2 graph lines + status; only 2 of 4 rows fit
    let text = render(&mut app, 50, 3);
    assert!(text[0].starts_with("│ ● right"));
    assert!(text[1].starts_with(&format!("●─╯ {PILL_LEFT} {TAG_GLYPH} v1 {PILL_RIGHT} base")));
}

#[test]
fn explicit_background_makes_pill_text_background_colored() {
    let mut app = App::new(synthetic_graph(), vec![], None);
    app.bg = Some(Color::Rgb(0, 0, 0));
    let backend = TestBackend::new(50, 8);
    let mut terminal = Terminal::new(backend).unwrap();
    let row = app.render_row(&app.rows[0], 50);
    terminal
        .draw(|f| f.render_widget(Paragraph::new(vec![row]), f.area()))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();

    let main_column = (0..buffer.area.width)
        .find(|&column| buffer.cell((column, 0)).unwrap().symbol() == "m")
        .expect("main label cell");
    let cell = buffer.cell((main_column, 0)).expect("main label cell");
    // Explicit style: text (and caps) in the background color, pill bg =
    // the branch's lane color, and no reverse-video needed.
    let nc = app.rows[0].node_col.unwrap();
    let lane = PALETTE[app.rows[0].colors[nc] % PALETTE.len()];
    assert_eq!(cell.style().fg, Some(Color::Rgb(0, 0, 0)));
    assert_eq!(cell.style().bg, Some(lane));
    assert!(!cell.style().add_modifier.contains(Modifier::REVERSED));
}

#[test]
fn formats_dates_in_commit_timezone() {
    // epoch +0h
    assert_eq!(fmt_date(0, 0), "1970-01-01");
    assert_eq!(fmt_date(4, 0), "1970-01-01");
    // known recent date: 2026-08-05T00:00:00Z
    let ts = 1785888000;
    assert_eq!(fmt_date(ts, 0), "2026-08-05");
    // +02:00 offset shifts into the next day
    assert_eq!(fmt_date(ts, 2 * 3600), "2026-08-05");
    // -02:00 offset shifts into the previous day
    assert_eq!(fmt_date(ts, -2 * 3600), "2026-08-04");
}

#[test]
fn truncates_with_ellipsis() {
    assert_eq!(truncate("abc", 5), "abc");
    assert_eq!(truncate("abcdef", 5), "abcd…");
    assert_eq!(truncate("abcdef", 1), "…");
    assert_eq!(truncate("abcdef", 0), "");
}

#[test]
fn mouse_wheel_targets_only_panel_under_pointer() {
    let mut app = App::new(synthetic_graph(), vec![], None);
    app.staging_area = Rect::new(0, 0, 20, 20);
    app.graph_area = Rect::new(20, 0, 40, 10);
    app.diff_area = Rect::new(20, 10, 40, 10);

    app.handle_mouse_scroll(5, 5, 1);
    assert_eq!(app.diff_scroll, 0);
    assert_eq!(app.scroll_y, 0);

    app.handle_mouse_scroll(30, 15, 1);
    assert_eq!(app.diff_scroll, 1);
    assert_eq!(app.scroll_y, 0);
    assert_eq!(app.focus, Focus::Graph);

    app.focus = Focus::Graph;
    app.handle_mouse_scroll(30, 5, 1);
    assert_eq!(app.diff_scroll, 1);
    assert_eq!(app.scroll_y, 3);
}

#[test]
fn fixup_waits_for_a_new_graph_selection() {
    let mut app = App::new(
        synthetic_graph(),
        vec![StatusEntry {
            path: "file".into(),
            index: 'M',
            worktree: ' ',
        }],
        None,
    );
    app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
    assert_eq!(app.focus, Focus::Graph);
    assert!(app.pending_fixup);
    assert!(!app.commit_popup);

    app.select_graph_index(1);
    assert_eq!(app.focus, Focus::Graph);
    assert!(app.pending_fixup);
    assert!(!app.commit_popup);
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.focus, Focus::Commit);
    assert!(!app.pending_fixup);
    assert!(app.commit_popup);
    assert!(matches!(app.commit_mode, CommitMode::Fixup(_)));
}

#[test]
fn parses_osc11_background() {
    let cases = [
        (
            "\x1b]11;rgb:1010/2020/3030\x1b\\",
            Color::Rgb(0x10, 0x20, 0x30),
        ),
        (
            "\x1b]11;rgba:0a0a/0b0b/0c0c\x1b\\",
            Color::Rgb(0x0a, 0x0b, 0x0c),
        ),
        ("\x1b]11;#1a2b3c\x1b\\", Color::Rgb(0x1a, 0x2b, 0x3c)),
        ("\x1b]11;rgb:0a/0b/0c\x1b\\", Color::Rgb(0x0a, 0x0b, 0x0c)),
        (
            "\x1b]11;rgb:1010/2020/3030\x07",
            Color::Rgb(0x10, 0x20, 0x30),
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(
            parse_osc11(input.as_bytes()),
            Some(expected),
            "for {input:?}"
        );
    }
    assert_eq!(parse_osc11(b"no response"), None);
    assert_eq!(parse_osc11(b"\x1b]10;rgb:1111/2222/3333\x1b\\"), None);
}

#[test]
fn parses_osc4_terminal_palette_responses() {
    let palette = parse_osc4(b"\x1b]4;4;rgb:8080/4040/ffff\x1b\\\x1b]4;7;rgb:dddd/dddd/dddd\x1b\\");
    assert_eq!(palette[4], Some(Color::Rgb(0x80, 0x40, 0xff)));
    assert_eq!(palette[7], Some(Color::Rgb(0xdd, 0xdd, 0xdd)));
    assert_eq!(palette[1], None);
}

fn bench_diff(line_count: usize) -> String {
    let mut diff = String::from("@@ -1,1 +1,1 @@\n");
    for index in 0..line_count {
        if index % 7 == 0 {
            diff.push_str(&format!(
                "+    let value_{index} = compute({index}, \"text\"); // comment\n"
            ));
        } else {
            diff.push_str(&format!("+    let value_{index} = compute({index});\n"));
        }
    }
    diff
}

/// Perf harness for the diff render path. Run explicitly:
/// `cargo test --release --lib bench_render_hot_path -- --ignored --nocapture`
#[test]
#[ignore = "perf measurement; run with --ignored --nocapture"]
fn bench_render_hot_path() {
    const FRAMES: usize = 30;
    for diff_lines in [500usize, 2_000, 20_000] {
        let mut app = App::new(GraphModel::default(), Vec::new(), None);
        app.diff_path = Some(GitPath::from("bench.rs"));
        app.diff_target = DiffTarget::Worktree;
        app.apply_diff_text(bench_diff(diff_lines));

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| app.draw(f)).unwrap();
        let cached_start = Instant::now();
        for _ in 0..FRAMES {
            terminal.draw(|f| app.draw(f)).unwrap();
        }
        let cached = cached_start.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64;

        let miss_start = Instant::now();
        for _ in 0..FRAMES {
            app.diff_cache = None;
            terminal.draw(|f| app.draw(f)).unwrap();
        }
        let miss = miss_start.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64;

        println!(
            "BENCH diff_lines={diff_lines} rendered={} cached_frame={cached:.3}ms cache_miss_frame={miss:.3}ms",
            app.diff_lines.len()
        );
    }
}

fn bench_frame(app: &mut App, frames: usize) -> f64 {
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let start = Instant::now();
    for _ in 0..frames {
        terminal.draw(|f| app.draw(f)).unwrap();
    }
    start.elapsed().as_secs_f64() * 1000.0 / frames as f64
}

/// Perf harness for the status pane, commit details, and the three-way
/// resolver. Run explicitly:
/// `cargo test --release --lib bench_secondary_paths -- --ignored --nocapture`
#[test]
#[ignore = "perf measurement; run with --ignored --nocapture"]
fn bench_secondary_paths() {
    const FRAMES: usize = 30;

    for n in [500usize, 2_000, 5_000] {
        let status: Vec<StatusEntry> = (0..n)
            .map(|i| StatusEntry {
                path: GitPath::from(format!("src/module/file_{i}.rs")),
                index: ' ',
                worktree: 'M',
            })
            .collect();
        let mut app = App::new(GraphModel::default(), status, None);
        for i in 0..n {
            app.diff_stats.insert(
                (GitPath::from(format!("src/module/file_{i}.rs")), false),
                (12, 3),
            );
        }
        let ms = bench_frame(&mut app, FRAMES);
        println!("BENCH status_rows={n} frame={ms:.3}ms");
    }

    for n in [2_000usize, 20_000] {
        let message = (0..n)
            .map(|i| format!("line {i} of the commit body"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut commit = c("big", &[], "big message", 1);
        commit.message = message;
        let graph = GraphModel {
            commits: vec![commit],
            refs: Vec::new(),
            branch_tracking: Vec::new(),
            head_id: Some("big".into()),
            head_name: Some("main".into()),
        };
        let mut app = App::new(graph, Vec::new(), None);
        app.historical_commit = Some("big".into());
        let ms = bench_frame(&mut app, FRAMES);
        println!("BENCH details_lines={n} frame={ms:.3}ms");
    }

    for n in [2_000usize, 10_000] {
        let side = |prefix: &str| {
            (0..n)
                .map(|i| DiffLine {
                    old_no: Some(i as u32 + 1),
                    new_no: Some(i as u32 + 1),
                    text: format!("{prefix} line {i}"),
                    kind: DiffKind::Context,
                })
                .collect::<Vec<_>>()
        };
        let working = format!(
            "<<<<<<<\n{}\n=======\n{}\n>>>>>>>\n",
            "ours\n".repeat(5),
            "theirs\n".repeat(5)
        );
        let view = ThreeWayView {
            path: GitPath::from("conflict.txt"),
            rows: Vec::new(),
            ours_diff: side("ours"),
            theirs_diff: side("theirs"),
            result_diff: Vec::new(),
            working: working.into_bytes(),
            pending_resolution: None,
            result_preview: false,
            chosen_side: None,
            hunk_index: 0,
            ours_missing: false,
            theirs_missing: false,
            result_missing: false,
            scroll_y: 0,
            scroll_x: 0,
        };
        let mut app = App::new(GraphModel::default(), Vec::new(), None);
        app.three_way_view = Some(view);
        let ms = bench_frame(&mut app, FRAMES);
        println!("BENCH three_way_lines={n} frame={ms:.3}ms");
    }
}

#[test]
fn wrapped_text_preserves_characters_and_spaces() {
    let text = "aaaaa bbbbb ccccc ddddd eeeee fffff ggggg";
    let chars: Vec<char> = text.chars().collect();
    let lines = wrap_text(text, 20);
    assert!(lines.len() > 1);
    assert!(lines.iter().all(|line| {
        chars[line.start..line.end]
            .iter()
            .map(|&character| char_width(character))
            .sum::<usize>()
            <= 20
    }));
    let rejoined: String = lines
        .iter()
        .map(|line| chars[line.start..line.end].iter().collect::<String>())
        .collect();
    assert_eq!(rejoined, text);
}

#[test]
fn wrapped_cursor_follows_word_wrapped_lines() {
    let text = "aaaaa bbbbb ccccc ddddd";
    let chars: Vec<char> = text.chars().collect();
    let lines = wrap_text(text, 20);
    let word_start = text.find("ddddd").expect("word");
    assert_eq!(wrapped_cursor(&lines, &chars, word_start), (1, 0));
    assert_eq!(wrapped_cursor(&lines, &chars, word_start + 2), (1, 2));
}

#[test]
fn wrapped_cursor_handles_explicit_newlines() {
    let text = "ab\ncd";
    let chars: Vec<char> = text.chars().collect();
    let lines = wrap_text(text, 10);
    assert_eq!(lines.len(), 2);
    assert_eq!(wrapped_cursor(&lines, &chars, 2), (0, 2));
    assert_eq!(wrapped_cursor(&lines, &chars, 3), (1, 0));
}

fn review_app(scenario: &str) -> (tempfile::TempDir, App) {
    let directory = tempfile::tempdir().unwrap();
    let output = std::process::Command::new("bash")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/review-regressions.sh"))
        .arg(directory.path())
        .arg(scenario)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let backend = crate::backend::open(directory.path()).unwrap();
    let app = App::new(
        backend.build_graph().unwrap(),
        backend.status().unwrap(),
        Some(backend),
    );
    (directory, app)
}

fn review_git(path: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

#[test]
fn conflict_previews_leave_files_untouched_and_escape_cancels() {
    for (scenario, side) in [
        ("conflict", ConflictSide::Base),
        ("modify-delete", ConflictSide::Theirs),
        ("delete-modify", ConflictSide::Ours),
    ] {
        let (directory, mut app) = review_app(scenario);
        let original = std::fs::read(directory.path().join("file.txt")).unwrap();
        app.choose_selected_conflict_side(side);
        let screen = render_full_app(&mut app, 120, 40);
        assert!(screen.contains(if side == ConflictSide::Base {
            "Restore entire file from Base"
        } else {
            "Delete entire file"
        }));
        assert_eq!(
            std::fs::read(directory.path().join("file.txt")).unwrap(),
            original
        );
        assert!(!review_git(directory.path(), &["ls-files", "-u"]).is_empty());
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(!app.three_way_view.as_ref().unwrap().result_preview);
        assert_eq!(
            std::fs::read(directory.path().join("file.txt")).unwrap(),
            original
        );
        app.choose_selected_conflict_side(side);
        app.confirm_selected_conflict_result();
        assert!(review_git(directory.path(), &["ls-files", "-u"]).is_empty());
        if side == ConflictSide::Base {
            assert_eq!(
                std::fs::read(directory.path().join("file.txt")).unwrap(),
                b"base\n"
            );
        } else {
            assert!(!directory.path().join("file.txt").exists());
        }
    }
}

#[test]
fn external_conflict_edits_invalidate_previews_before_writing_or_staging() {
    for refresh_first in [false, true] {
        let (directory, mut app) = review_app("conflict");
        app.choose_selected_conflict_side(ConflictSide::Theirs);
        std::fs::write(directory.path().join("file.txt"), "external resolution\n").unwrap();
        if refresh_first {
            app.reload_external_snapshot(load_repository_snapshot(directory.path()).unwrap());
            assert!(!app.three_way_view.as_ref().unwrap().result_preview);
        }
        app.confirm_selected_conflict_result();
        assert_eq!(
            std::fs::read_to_string(directory.path().join("file.txt")).unwrap(),
            "external resolution\n"
        );
        assert!(!review_git(directory.path(), &["ls-files", "-u"]).is_empty());
    }
}

#[test]
fn content_refresh_updates_diff_and_preserves_then_clamps_viewport() {
    let (directory, mut app) = review_app("content");
    app.diff_area = Rect::new(0, 0, 60, 10);
    app.diff_scroll = 20;
    let selected = app.selected_status;
    let text = (0..60)
        .map(|i| format!("new content {i}\n"))
        .collect::<String>();
    std::fs::write(directory.path().join("file.txt"), text).unwrap();
    let snapshot = load_repository_snapshot(directory.path()).unwrap();
    assert_eq!(snapshot.status, app.local_status);
    let old_stats_generation = app.diff_stats_generation;
    app.reload_external_snapshot(snapshot);
    assert!(app
        .diff_lines
        .iter()
        .any(|line| line.text.contains("new content 40")));
    assert_eq!(app.diff_scroll, 20);
    assert_eq!(app.selected_status, selected);
    assert!(app.diff_stats_generation > old_stats_generation);
    std::fs::write(directory.path().join("file.txt"), "short\n").unwrap();
    app.reload_external_snapshot(load_repository_snapshot(directory.path()).unwrap());
    assert_eq!(app.diff_scroll, 0);
}

#[test]
fn external_amend_updates_graph_details_and_diff_to_the_same_commit() {
    let (directory, mut app) = review_app("history");
    app.select_graph_index(0);
    let old = app.historical_commit.clone().unwrap();
    review_git(directory.path(), &["rm", "old-only.txt"]);
    std::fs::write(directory.path().join("file.txt"), "new-history-content\n").unwrap();
    review_git(
        directory.path(),
        &["commit", "-am", "rewritten commit", "--amend"],
    );
    let head = review_git(directory.path(), &["rev-parse", "HEAD"]);
    assert_ne!(head, old);
    app.reload_external_snapshot(load_repository_snapshot(directory.path()).unwrap());
    assert_eq!(app.selected_commit_id().as_deref(), Some(head.as_str()));
    assert_eq!(app.historical_commit.as_deref(), Some(head.as_str()));
    assert!(app.status.iter().all(|entry| entry.path != "old-only.txt"));
    assert!(app
        .diff_lines
        .iter()
        .any(|line| line.text.contains("new-history-content")));
    let screen = render_full_app(&mut app, 100, 36);
    assert!(screen.contains("rewritten commit"));
    assert!(!screen.contains("selected commit"));
}

#[test]
fn clicks_use_the_rendered_file_list_offset_and_ignore_borders_and_blank_rows() {
    for (historical, staged) in [(false, false), (false, true), (true, false)] {
        let entries = (0..30)
            .map(|i| StatusEntry {
                path: format!("file-{i:02}.txt").into(),
                index: if staged || historical { 'M' } else { ' ' },
                worktree: if staged || historical { ' ' } else { 'M' },
            })
            .collect();
        let mut app = App::new(synthetic_graph(), entries, None);
        app.focus = Focus::Status;
        app.selected_status = 20;
        app.status_section = if staged {
            StatusSection::Staged
        } else {
            StatusSection::Unstaged
        };
        if historical {
            app.historical_commit = Some("m".into());
        }
        let screen = render_full_app(&mut app, 100, 36);
        let (row, line) = screen
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("file-"))
            .unwrap();
        let column = line[..line.find("file-").unwrap()].chars().count();
        let visible = &line[line.find("file-").unwrap()..];
        let expected: usize = visible[5..7].parse().unwrap();
        assert!(expected > 0, "fixture must scroll");
        app.handle_mouse(column as u16, row as u16);
        assert_eq!(
            app.selected_status, expected,
            "historical={historical}, staged={staged}"
        );
        let before = app.selected_status;
        app.handle_mouse(app.staging_area.x, app.staging_area.y);
        assert_eq!(app.selected_status, before);
        app.status.truncate(1);
        app.refresh_status_indices();
        app.selected_status = 0;
        render_full_app(&mut app, 100, 36);
        app.handle_mouse(column as u16, row as u16 + 2);
        assert_eq!(app.selected_status, 0);
    }
}

#[test]
fn modal_wheel_input_cannot_scroll_or_refocus_background_panels() {
    use crossterm::event::MouseEvent;
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.select_graph_index(0);
    render_full_app(&mut app, 100, 36);
    for overlay in 0..5 {
        app.help_popup = overlay == 0;
        app.graph_action = (overlay == 1).then_some(GraphAction::Reset);
        app.commit_popup = overlay == 2;
        app.abort_rebase_popup = overlay == 3;
        app.error_popup = (overlay == 4).then(|| "test error".into());
        for area in [app.diff_area, app.graph_area, app.details_area] {
            let mut wheel = Some((area.x + 1, area.y + 1, 1));
            app.collect_event(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    column: area.x + 1,
                    row: area.y + 1,
                    modifiers: KeyModifiers::NONE,
                }),
                &mut wheel,
            );
            assert!(wheel.is_none());
            let focus = app.focus;
            // Also reject a wheel event queued before the overlay opened.
            app.handle_mouse_scroll(area.x + 1, area.y + 1, 1);
            assert_eq!(app.focus, focus);
            assert_eq!(
                (app.diff_scroll, app.scroll_y, app.details_scroll),
                (0, 0, 0)
            );
        }
    }
}

#[test]
fn escape_cancels_fixup_and_restores_normal_enter_behavior() {
    let status = vec![StatusEntry {
        path: "file.txt".into(),
        index: 'M',
        worktree: ' ',
    }];
    let mut app = App::new(synthetic_graph(), status, None);
    app.focus = Focus::Status;
    app.handle_key(KeyEvent::from(KeyCode::Char('f')));
    assert!(app.pending_fixup);
    assert!(app
        .status_line()
        .to_string()
        .contains("Choose fixup target"));
    app.handle_key(KeyEvent::from(KeyCode::Esc));
    assert!(!app.pending_fixup);
    assert_eq!(app.focus, Focus::Status);
    app.select_graph_index(1);
    app.focus = Focus::Graph;
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(app.graph_action, Some(GraphAction::CheckoutBranch));
    assert!(!app.commit_popup);
}

#[test]
fn another_workflow_clears_fixup_selection() {
    let mut app = App::new(synthetic_graph(), Vec::new(), None);
    app.pending_fixup = true;
    app.open_search_popup();
    assert!(!app.pending_fixup);
    app.search_popup = None;
    app.pending_fixup = true;
    app.open_graph_action(GraphAction::Reset);
    assert!(!app.pending_fixup);
}

#[cfg(unix)]
#[test]
fn whole_file_side_confirmation_restores_git_file_mode() {
    use std::os::unix::fs::PermissionsExt;
    let (directory, mut app) = review_app("modify-delete");
    let path = directory.path().join("file.txt");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    app.refresh_diff();
    app.choose_selected_conflict_side(ConflictSide::Ours);
    assert_ne!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o111,
        0
    );
    app.confirm_selected_conflict_result();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o111,
        0
    );
    assert!(review_git(directory.path(), &["ls-files", "-u"]).is_empty());
}

mod common;

use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use common::{configure_peer, configure_user, git, git_ok};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use tempfile::TempDir;

struct Tui {
    _master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    screen: Arc<Mutex<vt100::Parser>>,
}

impl Tui {
    fn launch(repository: &Path) -> Self {
        Self::launch_with_editor(repository, None)
    }

    fn launch_with_editor(repository: &Path, editor: Option<&str>) -> Self {
        Self::launch_with_env(repository, editor, &[])
    }

    fn launch_with_env(repository: &Path, editor: Option<&str>, env: &[(&str, &str)]) -> Self {
        Self::launch_sized(repository, editor, env, 100, 36)
    }

    fn launch_sized(
        repository: &Path,
        editor: Option<&str>,
        env: &[(&str, &str)],
        cols: u16,
        rows: u16,
    ) -> Self {
        Self::launch_with_theme(
            repository,
            editor,
            env,
            cols,
            rows,
            DEFAULT_BACKGROUND_REPLY,
            DEFAULT_PALETTE_REPLY,
        )
    }

    /// Like `launch_sized`, but answer the terminal's OSC 11 background and OSC 4
    /// palette queries with these replies, so the screenshot generator can render
    /// a specific theme.
    fn launch_with_theme(
        repository: &Path,
        editor: Option<&str>,
        env: &[(&str, &str)],
        cols: u16,
        rows: u16,
        background_reply: &[u8],
        palette_reply: &[u8],
    ) -> Self {
        let pty = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open PTY");
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_figit"));
        command.arg(repository);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        for (key, value) in env {
            command.env(key, value);
        }
        if let Some(editor) = editor {
            command.env("VISUAL", editor);
        }
        let child = pty.slave.spawn_command(command).expect("launch figit");
        drop(pty.slave);

        let reader = pty.master.try_clone_reader().expect("clone PTY reader");
        let writer = Arc::new(Mutex::new(
            pty.master.take_writer().expect("take PTY writer"),
        ));
        let screen = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let reader_screen = Arc::clone(&screen);
        let response_writer = Arc::clone(&writer);
        let background_reply = background_reply.to_vec();
        let palette_reply = palette_reply.to_vec();
        thread::spawn(move || {
            let mut reader = reader;
            let mut bytes = [0_u8; 4096];
            let background_query = b"\x1b]11;?\x1b\\";
            let palette_query = b"\x1b]4;0;?;1;?;2;?;3;?;4;?;5;?;6;?;7;?;8;?;9;?;10;?;11;?;12;?;13;?;14;?;15;?\x1b\\";
            let cursor_query = b"\x1b[6n";
            let mut answered_background = false;
            let mut answered_palette = false;
            let mut answered_cursor = false;
            let mut pending = Vec::new();
            loop {
                match reader.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        reader_screen
                            .lock()
                            .expect("screen lock")
                            .process(&bytes[..count]);
                        pending.extend_from_slice(&bytes[..count]);
                        if !answered_background
                            && pending
                                .windows(background_query.len())
                                .any(|window| window == background_query)
                        {
                            let mut writer = response_writer.lock().expect("PTY writer lock");
                            let _ = writer.write_all(&background_reply);
                            let _ = writer.flush();
                            answered_background = true;
                        }
                        if !answered_palette
                            && pending
                                .windows(palette_query.len())
                                .any(|window| window == palette_query)
                        {
                            let palette = &palette_reply;
                            let mut writer = response_writer.lock().expect("PTY writer lock");
                            let _ = writer.write_all(palette);
                            let _ = writer.flush();
                            answered_palette = true;
                        }
                        if !answered_cursor
                            && pending
                                .windows(cursor_query.len())
                                .any(|window| window == cursor_query)
                        {
                            let mut writer = response_writer.lock().expect("PTY writer lock");
                            let _ = writer.write_all(b"\x1b[1;1R");
                            let _ = writer.flush();
                            answered_cursor = true;
                        }
                        let keep = palette_query
                            .len()
                            .max(background_query.len())
                            .max(cursor_query.len())
                            - 1;
                        if pending.len() > keep {
                            pending.drain(..pending.len() - keep);
                        }
                    }
                }
            }
        });

        let tui = Self {
            _master: pty.master,
            writer,
            child,
            screen,
        };
        tui.wait_for("Tab focus");
        tui
    }

    fn send(&mut self, bytes: &[u8]) {
        let mut writer = self.writer.lock().expect("PTY writer lock");
        writer.write_all(bytes).expect("write PTY input");
        writer.flush().expect("flush PTY input");
    }

    fn contents(&self) -> String {
        self.screen.lock().expect("screen lock").screen().contents()
    }

    fn wait_for(&self, expected: &str) {
        self.wait_until(
            Duration::from_secs(30),
            |contents| {
                format!("timed out waiting for {expected:?}; terminal contained:\n{contents}")
            },
            |_, contents| contents.contains(expected).then_some(()),
        );
    }

    /// Poll the screen until `predicate` returns `Some`, or panic with
    /// `describe(contents)` after `timeout`. The single place that decides how
    /// often to re-read the PTY and how long to wait keeps the timeouts
    /// consistent (and easy to tune when a machine is busy).
    fn wait_until<T>(
        &self,
        timeout: Duration,
        describe: impl FnOnce(&str) -> String,
        mut predicate: impl FnMut(&vt100::Parser, &str) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + timeout;
        loop {
            let screen = self.screen.lock().expect("screen lock");
            let contents = screen.screen().contents();
            if let Some(value) = predicate(&screen, &contents) {
                return value;
            }
            assert!(Instant::now() < deadline, "{}", describe(&contents));
            drop(screen);
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_text_background(&self, needle: &str, expected: vt100::Color) {
        self.wait_until(
            Duration::from_secs(5),
            |contents| {
                format!(
                    "timed out waiting for {needle:?} to have background {expected:?}:\n{contents}"
                )
            },
            |screen, contents| {
                contents.lines().enumerate().find_map(|(row, line)| {
                    let byte_column = line.find(needle)?;
                    let column = line[..byte_column].chars().count();
                    let cell = screen.screen().cell(row as u16, column as u16)?;
                    (cell.bgcolor() == expected).then_some(())
                })
            },
        );
    }

    fn wait_for_graph_focus(&self) {
        self.wait_for_title_focus("Graph");
    }

    /// Wait until the panel whose title contains `title` is the focused one
    /// (its border/title uses the light-cyan focus colour).
    fn wait_for_title_focus(&self, title: &str) {
        self.wait_until(
            Duration::from_secs(5),
            |contents| format!("{title:?} pane did not gain focus:\n{contents}"),
            |screen, contents| {
                contents.lines().enumerate().find_map(|(row, line)| {
                    let column = line.find(title)?;
                    let column = line[..column].chars().count();
                    let cell = screen.screen().cell(row as u16, column as u16)?;
                    (cell.fgcolor() == vt100::Color::Idx(14)).then_some(())
                })
            },
        );
    }

    fn wait_until_hidden(&self, text: &str) {
        self.wait_until(
            Duration::from_secs(30),
            |contents| {
                format!(
                    "timed out waiting for {text:?} to disappear; terminal contained:\n{contents}"
                )
            },
            |_, contents| (!contents.contains(text)).then_some(()),
        );
    }

    fn assert_input_highlighted(&self, input: &str) {
        self.wait_until(
            Duration::from_secs(5),
            |contents| {
                format!(
                    "input cursor/highlight not positioned at the end of {input:?}; terminal:\n{contents}"
                )
            },
            |screen, contents| {
                let (row, byte_col) = contents
                    .lines()
                    .enumerate()
                    .filter_map(|(row, line)| line.rfind(input).map(|col| (row, col)))
                    .last()?;
                let col = contents.lines().nth(row)?[..byte_col].chars().count();
                let cell = screen.screen().cell(row as u16, col as u16)?;
                let (cursor_row, cursor_col) = screen.screen().cursor_position();
                (cell.bgcolor() == vt100::Color::Idx(14)
                    && cursor_row == row as u16
                    && cursor_col == (col + input.chars().count()) as u16)
                    .then_some(())
            },
        );
    }

    fn assert_cursor_after(&self, input: &str) {
        self.wait_until(
            Duration::from_secs(5),
            |contents| format!("text cursor not at the end of {input:?}; terminal:\n{contents}"),
            |screen, contents| {
                let cursor = screen.screen().cursor_position();
                let line = contents.lines().nth(cursor.0 as usize)?;
                let byte_col = line.find(input)?;
                let col = line[..byte_col].chars().count();
                (cursor.1 == (col + input.chars().count()) as u16).then_some(())
            },
        );
    }

    fn quit(&mut self) {
        self.send(b"q");
        let status = self.child.wait().expect("wait for figit");
        assert!(status.success(), "figit exited unsuccessfully: {status:?}");
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        // Kill and reap the child so a panicking test never leaves a runaway
        // TUI process (and its PTY) behind.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn init_linear_repository() -> TempDir {
    let repository = TempDir::new().expect("temporary rebase repository");
    git_ok(
        repository.path(),
        &["init", "--quiet", "--initial-branch=main"],
    );
    configure_user(repository.path());
    std::fs::write(repository.path().join("file.txt"), "root\n").unwrap();
    git_ok(repository.path(), &["add", "file.txt"]);
    git_ok(
        repository.path(),
        &["commit", "--quiet", "-m", "root commit"],
    );

    std::fs::write(repository.path().join("file.txt"), "target\n").unwrap();
    std::fs::write(repository.path().join("other.txt"), "target side file\n").unwrap();
    git_ok(repository.path(), &["add", "file.txt", "other.txt"]);
    git_ok(
        repository.path(),
        &["commit", "--quiet", "-m", "target commit"],
    );

    std::fs::write(repository.path().join("file.txt"), "tip\n").unwrap();
    git_ok(
        repository.path(),
        &["commit", "--quiet", "-am", "tip commit"],
    );
    repository
}

fn init_hunk_staging_repository() -> TempDir {
    let repository = TempDir::new().expect("temporary hunk-staging repository");
    let path = repository.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    let base = "first base\ncontext 1\ncontext 2\ncontext 3\ncontext 4\ncontext 5\ncontext 6\ncontext 7\ncontext 8\ncontext 9\ncontext 10\nsecond base\ncontext 11\ncontext 12\ncontext 13\ncontext 14\ncontext 15\ncontext 16\ncontext 17\ncontext 18\ncontext 19\ncontext 20\nthird base\n";
    std::fs::write(path.join("file.txt"), base).unwrap();
    git_ok(path, &["add", "file.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "base file"]);
    std::fs::write(
        path.join("file.txt"),
        base.replace("first base", "first ours")
            .replace("third base", "third ours"),
    )
    .unwrap();
    repository
}

fn init_read_only_multi_hunk_repository() -> TempDir {
    let repository = TempDir::new().expect("temporary read-only multi-hunk repository");
    let path = repository.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    let base = (1..=120)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    std::fs::write(path.join("file.txt"), &base).unwrap();
    git_ok(path, &["add", "file.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "base file"]);
    let mut changed = base;
    for line in [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120] {
        changed = changed.replace(&format!("line {line}\n"), &format!("line {line} changed\n"));
    }
    std::fs::write(path.join("file.txt"), changed).unwrap();
    git_ok(path, &["add", "file.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "many hunks"]);
    repository
}

fn init_long_line_repository() -> TempDir {
    let repository = TempDir::new().expect("temporary long-line repository");
    let path = repository.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    std::fs::write(path.join("file.txt"), "seed\n").unwrap();
    git_ok(path, &["add", "file.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "seed"]);
    let long = format!("ZZSTARTZZ{}ZZENDZZ\n", "x".repeat(160));
    std::fs::write(path.join("file.txt"), long).unwrap();
    git_ok(path, &["add", "file.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "long line"]);
    repository
}

fn init_two_file_staging_repository() -> TempDir {
    let repository = TempDir::new().expect("temporary two-file staging repository");
    let path = repository.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    for name in ["a.txt", "b.txt"] {
        std::fs::write(path.join(name), format!("{name} base\n")).unwrap();
    }
    git_ok(path, &["add", "a.txt", "b.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "base files"]);
    for name in ["a.txt", "b.txt"] {
        std::fs::write(path.join(name), format!("{name} changed\n")).unwrap();
    }
    repository
}

fn init_stash_repository() -> TempDir {
    let repository = init_hunk_staging_repository();
    std::fs::write(repository.path().join("loose.txt"), "untracked content\n").unwrap();
    repository
}

/// A repo whose tip commit changes `tracked-a.txt` while `tracked-b.txt` is
/// left modified in the worktree.
fn init_commit_and_local_change_repository() -> TempDir {
    let repository = TempDir::new().expect("temporary commit + change repository");
    let path = repository.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    std::fs::write(path.join("tracked-a.txt"), "one\n").unwrap();
    std::fs::write(path.join("tracked-b.txt"), "one\n").unwrap();
    git_ok(path, &["add", "tracked-a.txt", "tracked-b.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "seed files"]);
    std::fs::write(path.join("tracked-a.txt"), "two\n").unwrap();
    git_ok(path, &["commit", "--quiet", "-am", "update tracked a"]);
    std::fs::write(path.join("tracked-b.txt"), "two\n").unwrap();
    repository
}

fn init_conflict_repository() -> TempDir {
    let repository = TempDir::new().expect("temporary conflict repository");
    let path = repository.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    std::fs::write(path.join("file.txt"), "base\n").unwrap();
    git_ok(path, &["add", "file.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "base"]);

    git_ok(path, &["checkout", "--quiet", "-b", "feature"]);
    std::fs::write(path.join("file.txt"), "theirs\n").unwrap();
    git_ok(path, &["commit", "--quiet", "-am", "theirs"]);

    git_ok(path, &["checkout", "--quiet", "main"]);
    std::fs::write(path.join("file.txt"), "ours\n").unwrap();
    git_ok(path, &["commit", "--quiet", "-am", "ours"]);
    let merge = git(path, &["merge", "--no-ff", "-m", "conflict", "feature"]);
    assert!(
        !merge.status.success(),
        "fixture merge should produce a conflict"
    );
    repository
}

fn init_two_file_conflict_repository() -> TempDir {
    let repository = TempDir::new().expect("temporary multi-file conflict repository");
    let path = repository.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    for file in ["file.txt", "other.txt"] {
        std::fs::write(path.join(file), "base\n").unwrap();
    }
    git_ok(path, &["add", "file.txt", "other.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "base"]);

    git_ok(path, &["checkout", "--quiet", "-b", "feature"]);
    for file in ["file.txt", "other.txt"] {
        std::fs::write(path.join(file), "theirs\n").unwrap();
    }
    git_ok(path, &["commit", "--quiet", "-am", "theirs"]);

    git_ok(path, &["checkout", "--quiet", "main"]);
    for file in ["file.txt", "other.txt"] {
        std::fs::write(path.join(file), "ours\n").unwrap();
    }
    git_ok(path, &["commit", "--quiet", "-am", "ours"]);
    let merge = git(
        path,
        &["merge", "--no-ff", "-m", "two-file conflict", "feature"],
    );
    assert!(!merge.status.success(), "both files should conflict");
    repository
}

fn init_multi_hunk_conflict_repository() -> TempDir {
    let repository = TempDir::new().expect("temporary multi-hunk conflict repository");
    let path = repository.path();
    git_ok(path, &["init", "--quiet", "--initial-branch=main"]);
    configure_user(path);
    let base = "first base\n\nline 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10\n\nsecond base\n";
    std::fs::write(path.join("file.txt"), base).unwrap();
    git_ok(path, &["add", "file.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "base"]);

    git_ok(path, &["checkout", "--quiet", "-b", "feature"]);
    let theirs = base
        .replace("first base", "first theirs")
        .replace("second base", "second theirs");
    std::fs::write(path.join("file.txt"), theirs).unwrap();
    git_ok(path, &["commit", "--quiet", "-am", "theirs"]);

    git_ok(path, &["checkout", "--quiet", "main"]);
    let ours = base
        .replace("first base", "first ours")
        .replace("second base", "second ours");
    std::fs::write(path.join("file.txt"), ours).unwrap();
    git_ok(path, &["commit", "--quiet", "-am", "ours"]);
    let merge = git(
        path,
        &["merge", "--no-ff", "-m", "multi-hunk conflict", "feature"],
    );
    assert!(!merge.status.success(), "both sections should conflict");
    let conflicted = std::fs::read_to_string(path.join("file.txt")).unwrap();
    assert_eq!(
        conflicted.matches("<<<<<<<").count(),
        2,
        "fixture should contain two conflict hunks"
    );
    repository
}

fn run_tui_fixture(path: &Path) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/tui-e2e.sh");
    let status = Command::new("bash")
        .arg(fixture)
        .arg(path)
        .status()
        .expect("create TUI fixture repository");
    assert!(status.success(), "TUI repository fixture failed");
}

fn init_repository() -> TempDir {
    // Always clone from a fixture source: `git clone` adds an `origin` remote,
    // which is what makes the branches look tracked. A bare fixture run leaves
    // them untracked, so the rendering tests would diverge between a local
    // checkout (which has `test_repo/`) and CI.
    let manual_fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_repo");
    let scratch;
    let source = if is_tui_fixture(&manual_fixture) {
        manual_fixture.clone()
    } else {
        scratch = TempDir::new().expect("fixture source");
        run_tui_fixture(scratch.path());
        scratch.path().to_path_buf()
    };

    let repository = TempDir::new().expect("temporary repository");
    let cloned = Command::new("git")
        .args(["clone", "--quiet", "--no-hardlinks"])
        .arg(&source)
        .arg(repository.path())
        .status()
        .expect("clone TUI fixture");
    assert!(cloned.success(), "failed to clone TUI fixture");
    assert!(
        git(repository.path(), &["config", "figit.fixture", "tui-e2e"])
            .status
            .success()
    );
    run_tui_fixture(repository.path());

    if git(
        repository.path(),
        &[
            "show-ref",
            "--verify",
            "--quiet",
            "refs/remotes/origin/main",
        ],
    )
    .status
    .success()
    {
        // A local fixture clone can inherit a source branch that has advanced
        // beyond the fixture tag. Keep the clone's remote main at the fixture
        // base so tests don't gain unrelated graph rows.
        git_ok(
            repository.path(),
            &[
                "update-ref",
                "refs/remotes/origin/main",
                "refs/tags/tui-e2e-base",
            ],
        );
        for branch in ["feature/lorem", "feature/ipsum", "release/dolor"] {
            let local = git(
                repository.path(),
                &[
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}"),
                ],
            )
            .status
            .success();
            let remote = format!("refs/remotes/origin/{branch}");
            if !local
                && git(
                    repository.path(),
                    &["show-ref", "--verify", "--quiet", &remote],
                )
                .status
                .success()
            {
                git_ok(repository.path(), &["branch", "--quiet", branch, &remote]);
            }
            if git(
                repository.path(),
                &[
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}"),
                ],
            )
            .status
            .success()
            {
                git_ok(
                    repository.path(),
                    &[
                        "update-ref",
                        &format!("refs/remotes/origin/{branch}"),
                        &format!("refs/heads/{branch}"),
                    ],
                );
            }
        }
    }
    repository
}

fn is_tui_fixture(path: &Path) -> bool {
    if !path.join(".git").exists() {
        return false;
    }
    let marker = Command::new("git")
        .args(["-C"])
        .arg(path)
        .args(["config", "--local", "--get", "figit.fixture"])
        .output();
    let has_base_tag = Command::new("git")
        .args(["-C"])
        .arg(path)
        .args(["show-ref", "--verify", "--quiet", "refs/tags/tui-e2e-base"])
        .status();
    marker.is_ok_and(|output| output.status.success() && output.stdout == b"tui-e2e\n")
        && has_base_tag.is_ok_and(|status| status.success())
}

#[test]
fn tui_renders_and_drives_git_workflows_in_a_real_terminal() {
    let repository = init_repository();
    let mut tui = Tui::launch(repository.path());

    tui.wait_for("Unstaged (2)");
    let initial_screen = tui.contents();
    assert!(initial_screen.contains("Graph"));
    assert!(initial_screen.contains("Commit"));
    assert!(initial_screen.contains("Branch"));
    assert!(
        initial_screen
            .lines()
            .any(|line| line.contains('\u{2713}') && line.contains("main")),
        "the checked-out branch pill should have a checkmark marker:\n{initial_screen}"
    );
    assert!(
        !initial_screen.contains("HEAD"),
        "the graph should not add a HEAD pill"
    );
    assert!(initial_screen.contains("✎ Local changes (2)"));
    let graph_lines: Vec<&str> = initial_screen.lines().collect();
    let local_row = graph_lines
        .iter()
        .position(|line| line.contains("Local changes"))
        .expect("local graph row");
    let unstaged_file_row = graph_lines
        .iter()
        .position(|line| line.contains("a-stage.txt"))
        .expect("selected status file row");
    {
        let screen = tui.screen.lock().expect("screen lock");
        assert_eq!(
            screen
                .screen()
                .cell(local_row as u16, 43)
                .unwrap()
                .bgcolor(),
            vt100::Color::Rgb(45, 50, 65),
            "selected Local changes graph row should fill the Graph width"
        );
        assert_eq!(
            screen
                .screen()
                .cell(unstaged_file_row as u16, 98)
                .unwrap()
                .bgcolor(),
            vt100::Color::Rgb(45, 50, 65),
            "selected status row should fill the Unstaged width"
        );
    }
    let release = graph_lines
        .iter()
        .position(|line| line.contains("release/dolor"))
        .unwrap_or_else(|| panic!("release tip not visible:\n{initial_screen}"));
    let release_byte_column = graph_lines[release].find("release/dolor").unwrap();
    let release_column = graph_lines[release][..release_byte_column].chars().count();
    {
        let screen = tui.screen.lock().expect("screen lock");
        let label = screen
            .screen()
            .cell(release as u16, release_column as u16)
            .unwrap();
        assert_eq!(label.bgcolor(), vt100::Color::Rgb(51, 76, 127));
        assert_eq!(label.fgcolor(), vt100::Color::Rgb(17, 17, 17));
        assert!(!label.dim());
        assert!(
            (0..100).any(|col| {
                let cell = screen.screen().cell(release as u16, col).unwrap();
                cell.fgcolor() == vt100::Color::Rgb(51, 76, 127)
                    && cell.bgcolor() == vt100::Color::Rgb(34, 34, 34)
                    && !cell.dim()
            }),
            "pill caps should use the same dim theme-derived blue as the fill"
        );
    }
    let node_column = graph_lines[release]
        .chars()
        .position(|ch| ch == '●')
        .expect("release tip node visible");
    let release_parent = git_ok(
        repository.path(),
        &["rev-parse", "--short=7", "release/dolor~1"],
    );
    let parent_row = graph_lines
        .iter()
        .position(|line| line.contains(&release_parent))
        .unwrap_or_else(|| {
            panic!("release parent {release_parent} not visible:\n{initial_screen}")
        });
    assert_eq!(
        graph_lines[parent_row].chars().nth(node_column),
        Some('●'),
        "consecutive release commits should stay on the same graph lane:\n{initial_screen}"
    );

    // In file mode, Space stages the whole selected file.
    tui.send(b" ");
    tui.wait_for("Staged (1)");
    tui.wait_for("Unstaged (1)");
    let staged = git(
        repository.path(),
        &["diff", "--cached", "--quiet", "--", "a-stage.txt"],
    );
    assert!(
        !staged.status.success(),
        "Space should stage the selected file in file mode"
    );
    let after_stage = tui.contents();
    let next_row = after_stage
        .lines()
        .position(|line| line.contains("z-discard.txt"))
        .expect("next unstaged file remains selected");
    let next_column = after_stage
        .lines()
        .nth(next_row)
        .unwrap()
        .find("z-discard.txt")
        .unwrap();
    let next_column = after_stage.lines().nth(next_row).unwrap()[..next_column]
        .chars()
        .count();
    assert_eq!(
        tui.screen
            .lock()
            .unwrap()
            .screen()
            .cell(next_row as u16, next_column as u16)
            .unwrap()
            .bgcolor(),
        vt100::Color::Rgb(45, 50, 65),
        "file staging should advance selection within Unstaged"
    );

    // Open the confirmation for the next unstaged file and discard it.
    tui.send(b"x");
    tui.wait_for("Discard 1 unstaged file?");
    tui.send(b"\r");
    tui.wait_for("Unstaged (0)");
    assert!(
        !repository.path().join("z-discard.txt").exists(),
        "discard should remove the untracked file"
    );

    // Open a text-input popup; `q` must be inserted rather than quitting the TUI.
    tui.send(b"\t"); // Focus Graph.
    tui.send(b"\x1b[B");
    tui.send(b"\x1b[B");
    tui.send(b"\x1b[B"); // Pass release history and land on the main branch tip.
    tui.send(b"b");
    tui.send(b"e2e-q-branch");
    tui.wait_for("e2e-q-branch");
    tui.send(b"\x1b[D\x1b[C"); // Move left/right within the branch-name field.
    tui.assert_input_highlighted("e2e-q-branch");
    tui.send(b"\r");
    tui.wait_until_hidden("New branch");
    let created = git(
        repository.path(),
        &["show-ref", "--verify", "--quiet", "refs/heads/e2e-q-branch"],
    );
    assert!(
        created.status.success(),
        "Enter should create the selected branch; terminal:\n{}",
        tui.contents()
    );

    // Focus stays on the graph after creating the branch. Go to the top of the
    // history and down one row to the release branch tip, then check it out.
    for _ in 0..5 {
        tui.send(b"\x1b[A");
    }
    tui.send(b"\x1b[B");
    tui.send(b"\r");
    tui.wait_for("Checkout local branch release/dolor?");
    tui.send(b"\r");
    tui.wait_until_hidden("Checkout");
    let current_branch = git(repository.path(), &["branch", "--show-current"]);
    assert_eq!(
        String::from_utf8_lossy(&current_branch.stdout).trim(),
        "release/dolor"
    );
    tui.wait_for("✓  release/dolor");

    // Move to the previous release commit, which has no branch ref. `c` must
    // check out that commit detached instead of following the release branch.
    let detached_target = git(repository.path(), &["rev-parse", "release/dolor~1"]);
    let detached_target = String::from_utf8_lossy(&detached_target.stdout)
        .trim()
        .to_string();
    // Focus is still on the graph after the branch checkout; go to the top and
    // down two rows to the release parent (no branch ref), then detach with `c`.
    for _ in 0..5 {
        tui.send(b"\x1b[A");
    }
    tui.send(b"\x1b[B");
    tui.send(b"\x1b[B");
    tui.send(b"c");
    tui.wait_for("Checkout selected commit (detached HEAD)?");
    tui.send(b"\r");
    tui.wait_until_hidden("Checkout");
    let detached_head = git(
        repository.path(),
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    );
    assert!(
        !detached_head.status.success(),
        "c should detach at an unbranched commit"
    );
    let checked_out = git(repository.path(), &["rev-parse", "HEAD"]);
    assert_eq!(
        String::from_utf8_lossy(&checked_out.stdout).trim(),
        detached_target
    );
    tui.wait_for("(detached)");

    tui.quit();
}

#[test]
fn selected_diff_hunks_can_be_staged_and_unstaged_independently() {
    let repository = init_hunk_staging_repository();
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("file.txt");
    tui.wait_for("2/2");
    tui.send(b"h");
    // Entering hunk mode highlights the selected hunk; sync on that because the
    // `[/] hunk` hint alone is shown for any diff with hunks.
    let selected = vt100::Color::Rgb(45, 50, 65);
    tui.wait_for_text_background("context 3", selected);
    let screen = tui.contents();
    assert!(
        screen.lines().any(|line| {
            line.contains("Space stage")
                && line.contains("[/] hunk")
                && line.contains("f/h/l • file/hunk/line")
        }),
        "staging and diff panels should show the shared mode hints:\n{screen}"
    );
    tui.send(b"[");
    tui.wait_for_text_background("context 18", selected);
    tui.send(b"]");
    tui.wait_for_text_background("context 3", selected);
    {
        let screen = tui.screen.lock().expect("screen lock");
        let contents = screen.screen().contents();
        let context_row = contents
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("context 1"))
            .unwrap_or_else(|| panic!("selected hunk context should be visible:\n{contents}"));
        let byte_column = context_row.1.find("context 1").unwrap();
        let column = context_row.1[..byte_column].chars().count();
        assert_eq!(
            screen
                .screen()
                .cell(context_row.0 as u16, column as u16)
                .unwrap()
                .bgcolor(),
            vt100::Color::Rgb(45, 50, 65),
            "the selected hunk should have a section highlight"
        );
        assert_eq!(
            screen
                .screen()
                .cell(context_row.0 as u16, 98)
                .unwrap()
                .bgcolor(),
            vt100::Color::Rgb(45, 50, 65),
            "the hunk highlight should continue to the Diff panel edge"
        );
        let added_row = contents
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("+first ours"))
            .unwrap_or_else(|| panic!("added line should be visible:\n{contents}"));
        assert_eq!(
            screen
                .screen()
                .cell(added_row.0 as u16, 98)
                .unwrap()
                .bgcolor(),
            vt100::Color::Rgb(38, 56, 43),
            "green added-line background should continue to the Diff edge"
        );
        let removed_row = contents
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("-first base"))
            .unwrap_or_else(|| panic!("removed line should be visible:\n{contents}"));
        assert_eq!(
            screen
                .screen()
                .cell(removed_row.0 as u16, 98)
                .unwrap()
                .bgcolor(),
            vt100::Color::Rgb(70, 44, 49),
            "red removed-line background should continue to the Diff edge"
        );
    }
    // Returning to file mode removes the selected-hunk overlay while keeping
    // the normal added/removed diff colors intact.
    tui.send(b"f");
    tui.wait_for_text_background("context 1", vt100::Color::Default);
    {
        let screen = tui.screen.lock().expect("screen lock");
        let contents = screen.screen().contents();
        let added_row = contents
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("+first ours"))
            .expect("added row visible in file mode");
        let byte_column = added_row.1.find("+first ours").unwrap();
        let column = added_row.1[..byte_column].chars().count();
        assert_eq!(
            screen
                .screen()
                .cell(added_row.0 as u16, column as u16)
                .unwrap()
                .bgcolor(),
            vt100::Color::Rgb(38, 56, 43),
            "file mode should preserve the ordinary added-line color"
        );
    }
    tui.send(b"h");
    tui.wait_for("[/] hunk");

    tui.send(b"]");
    tui.wait_for("third ours");
    tui.send(b"[");
    tui.wait_for("first ours");
    tui.send(b" ");

    tui.wait_for("Staged (1)");
    tui.wait_for("Unstaged (1)");
    tui.wait_for("1/2");
    let screen = tui.contents();
    assert!(screen.contains("1/2"));
    assert!(!screen.contains("1/2 staged"));
    assert!(!screen.contains("1/2 unstaged"));
    for row in screen
        .lines()
        .filter(|line| line.contains("file.txt") && line.contains("1/2"))
    {
        assert!(
            row.find("1/2").unwrap() < row.find("+1 -1").unwrap(),
            "hunk recap should appear immediately before the diff stats: {row}"
        );
    }
    let staged = git_ok(repository.path(), &["diff", "--cached", "--", "file.txt"]);
    let unstaged = git_ok(repository.path(), &["diff", "--", "file.txt"]);
    assert!(
        staged.contains("+first ours"),
        "wrong hunk staged:\n{staged}"
    );
    assert!(
        !staged.contains("+third ours"),
        "second hunk should remain unstaged:\n{staged}"
    );
    assert!(
        unstaged.contains("+third ours"),
        "second hunk should remain in the worktree:\n{unstaged}"
    );
    assert!(
        !unstaged.contains("+first ours"),
        "staged hunk should leave the worktree diff:\n{unstaged}"
    );

    // Stage the remaining working-tree hunk, then unstage only the second
    // staged hunk to prove the two directions use the proper diff layer.
    tui.wait_for("third ours");
    tui.send(b" ");
    tui.wait_for("Unstaged (0)");
    let remaining = git_ok(repository.path(), &["diff", "--", "file.txt"]);
    assert!(
        remaining.is_empty(),
        "expected no worktree diff after staging both hunks:\n{remaining}\n{}",
        tui.contents()
    );
    tui.send(b"]");
    tui.send(b" ");
    tui.wait_for("Unstaged (1)");
    tui.wait_for("1/2");
    let staged = git_ok(repository.path(), &["diff", "--cached", "--", "file.txt"]);
    let unstaged = git_ok(repository.path(), &["diff", "--", "file.txt"]);
    assert!(
        staged.contains("+first ours"),
        "first hunk should remain staged:\n{staged}"
    );
    assert!(
        !staged.contains("+third ours"),
        "second hunk should be unstaged:\n{staged}"
    );
    assert!(
        unstaged.contains("+third ours"),
        "unstaged second hunk missing:\n{unstaged}"
    );
    let screen = tui.contents();
    assert!(screen.contains("1/2"));
    assert!(!screen.contains("1/2 staged"));
    assert!(!screen.contains("1/2 unstaged"));

    tui.quit();
}

#[test]
fn line_mode_stages_only_the_selected_added_line() {
    let repository = init_hunk_staging_repository();
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("2/2");

    tui.send(b"l");
    tui.wait_for("f/h/l • file/hunk/line");
    assert!(tui.contents().contains("Space stage • x discard • s stash"));
    tui.send(b"f");
    tui.wait_for_text_background("+first ours", vt100::Color::Rgb(38, 56, 43));
    tui.send(b"l");
    tui.send(b"j"); // move from the removed line to its added replacement
    tui.wait_for_text_background("+first ours", vt100::Color::Rgb(54, 91, 60));
    {
        let screen = tui.screen.lock().expect("screen lock");
        let contents = screen.screen().contents();
        let selected = contents
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("+first ours"))
            .expect("added line visible in line mode");
        let byte_column = selected.1.find("+first ours").unwrap();
        let column = selected.1[..byte_column].chars().count();
        assert_eq!(
            screen
                .screen()
                .cell(selected.0 as u16, column as u16)
                .unwrap()
                .bgcolor(),
            vt100::Color::Rgb(54, 91, 60),
            "line selection should move to the added line:\n{contents}"
        );
    }
    tui.send(b" ");
    tui.wait_for("Staged (1)");

    let staged = git_ok(repository.path(), &["diff", "--cached", "--", "file.txt"]);
    let unstaged = git_ok(repository.path(), &["diff", "--", "file.txt"]);
    assert!(
        staged.contains("first ours"),
        "selected line should be in the index:\n{staged}\n{}",
        tui.contents()
    );
    assert!(
        !staged.contains("third ours"),
        "other hunk must remain unstaged:\n{staged}"
    );
    assert!(
        unstaged.contains("first base"),
        "unselected deletion should stay in worktree diff:\n{unstaged}"
    );
    assert!(
        unstaged.contains("third ours"),
        "other hunk should stay unstaged:\n{unstaged}"
    );

    tui.quit();
}

#[test]
fn file_staging_switches_sections_when_the_source_list_becomes_empty() {
    let repository = init_hunk_staging_repository();
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("Unstaged (1)");
    tui.send(b" ");
    tui.wait_for("Staged (1)");
    tui.wait_for("Unstaged (0)");

    // In file mode, Space acts on the selected Staged file. Unstaging it creates an
    // Unstaged entry, and focus should move there because Staged became empty.
    tui.send(b" ");
    tui.wait_for("Staged (0)");
    tui.wait_for("Unstaged (1)");
    let screen = tui.contents();
    assert!(
        screen.contains("Space stage • x discard • s stash"),
        "the empty Staged list should hand selection to Unstaged:\n{screen}"
    );
    tui.quit();
}

#[test]
fn staging_and_unstaging_stay_in_the_source_panel_until_it_is_empty() {
    let repository = init_two_file_staging_repository();
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("Unstaged (2)");

    tui.send(b" ");
    tui.wait_for("Staged (1)");
    tui.wait_for("Unstaged (1)");
    let screen = tui.contents();
    assert!(
        screen.contains("b.txt"),
        "next unstaged file should be selected:\n{screen}"
    );
    assert!(screen.contains("Space stage • x discard • s stash"));

    tui.send(b" ");
    tui.wait_for("Staged (2)");
    tui.wait_for("Unstaged (0)");
    tui.send(b" ");
    tui.wait_for("Staged (1)");
    tui.wait_for("Unstaged (1)");
    let screen = tui.contents();
    assert!(screen.contains("Space unstage • c commit"));
    assert!(
        screen.contains("a.txt"),
        "selection should stay in Staged on the remaining file:\n{screen}"
    );

    tui.send(b" ");
    tui.wait_for("Staged (0)");
    tui.wait_for("Unstaged (2)");
    assert!(tui.contents().contains("Space stage • x discard • s stash"));
    tui.quit();
}

#[test]
fn undo_shortcut_confirms_and_reverts_the_latest_commit() {
    let repository = init_linear_repository();
    let previous = git_ok(repository.path(), &["rev-parse", "HEAD^"]);
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("Graph");
    tui.wait_for("u undo");

    tui.send(b"u");
    tui.wait_for("Undo from reflog");
    tui.wait_for("commit: tip commit");
    tui.send(b"\r");

    tui.wait_for("Staged (1)");
    assert_eq!(git_ok(repository.path(), &["rev-parse", "HEAD"]), previous);
    assert!(git_ok(repository.path(), &["diff", "--cached", "--", "file.txt"]).contains("tip"));
    assert!(tui.contents().contains("Graph"));
    tui.quit();
}

#[test]
fn undo_picker_can_restore_an_older_reflog_entry() {
    let repository = init_linear_repository();
    let older = git_ok(repository.path(), &["rev-parse", "HEAD~2"]);
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("Graph");

    tui.send(b"u");
    tui.wait_for("Undo from reflog");
    tui.wait_for("HEAD@{2}");
    tui.send(b"\x1b[B");
    tui.wait_for_text_background("HEAD@{2}", vt100::Color::Idx(14));
    tui.send(b"\r");

    tui.wait_for("Staged (2)");
    let restored = git_ok(repository.path(), &["rev-parse", "HEAD"]);
    assert_eq!(
        restored,
        older,
        "restored the wrong reflog entry; popup was:\n{}",
        tui.contents()
    );
    tui.quit();
}

#[test]
fn status_rows_keep_hunk_recap_and_diff_stats_aligned_for_multiple_files() {
    let repository = init_two_file_staging_repository();
    std::fs::write(
        repository.path().join("a.txt"),
        "a.txt changed\nextra line\n",
    )
    .unwrap();
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("Unstaged (2)");
    tui.wait_for("+2 -1");
    tui.wait_for("+1 -1");

    let screen = tui.contents();
    let row_for = |path: &str| {
        screen
            .lines()
            .find(|line| line.contains(path) && line.contains("1/1"))
            .unwrap_or_else(|| panic!("status row for {path} missing:\n{screen}"))
    };
    let a_row = row_for("a.txt");
    let b_row = row_for("b.txt");
    for (path, row, expected) in [("a.txt", a_row, "+2 -1"), ("b.txt", b_row, "+1 -1")] {
        assert!(
            row.contains(expected),
            "{path} diff stats were clipped or shifted: {row}"
        );
        assert!(
            row.find("1/1").unwrap() < row.find(expected).unwrap(),
            "{path} hunk recap should precede the diff stats: {row}"
        );
    }
    let removal_column = |row: &str| {
        let byte = row.rfind("-1").unwrap();
        row[..byte].chars().count()
    };
    assert_eq!(
        removal_column(a_row),
        removal_column(b_row),
        "the red removal counts should align on the right:\n{a_row}\n{b_row}"
    );
    tui.quit();
}

#[test]
fn fetch_picker_selects_a_remote_without_changing_branch_config() {
    let repository = init_linear_repository();
    let remote_dir = TempDir::new().expect("remote repository directory");
    let remote = remote_dir.path().join("remote.git");
    let remote_arg = remote.to_str().unwrap();
    git_ok(
        repository.path(),
        &["clone", "--quiet", "--bare", ".", remote_arg],
    );
    git_ok(repository.path(), &["remote", "add", "origin", remote_arg]);
    git_ok(repository.path(), &["fetch", "--quiet", "origin"]);
    git_ok(
        repository.path(),
        &["remote", "add", "aaa-mirror", remote_arg],
    );
    git_ok(repository.path(), &["fetch", "--quiet", "aaa-mirror"]);
    let mirror_head = git_ok(
        repository.path(),
        &["rev-parse", "refs/remotes/aaa-mirror/main"],
    );
    git_ok(
        repository.path(),
        &["branch", "--set-upstream-to=origin/main", "main"],
    );
    std::fs::write(repository.path().join("local.txt"), "local commit\n").unwrap();
    git_ok(repository.path(), &["add", "local.txt"]);
    git_ok(
        repository.path(),
        &["commit", "--quiet", "-m", "local update"],
    );

    let peer_dir = TempDir::new().expect("peer repository directory");
    let peer = peer_dir.path().join("peer");
    let peer_arg = peer.to_str().unwrap();
    git_ok(peer_dir.path(), &["clone", "--quiet", remote_arg, peer_arg]);
    configure_peer(&peer);
    std::fs::write(peer.join("remote.txt"), "fetched\n").unwrap();
    git_ok(&peer, &["add", "remote.txt"]);
    git_ok(&peer, &["commit", "--quiet", "-m", "remote update"]);
    git_ok(&peer, &["push", "--quiet"]);
    let remote_head = git_ok(&peer, &["rev-parse", "HEAD"]);

    let mut tui = Tui::launch(repository.path());
    tui.wait_for("F fetch");
    tui.send(b"F");
    tui.wait_for("Fetch from remote");
    tui.wait_for("aaa-mirror");
    tui.wait_for("origin");
    tui.wait_for_text_background("origin", vt100::Color::Idx(14));
    tui.send(b"\r");
    tui.wait_for("↑1 ↓1");
    assert_eq!(
        git_ok(
            repository.path(),
            &["rev-parse", "refs/remotes/origin/main"]
        ),
        remote_head
    );
    assert_eq!(
        git_ok(
            repository.path(),
            &["rev-parse", "refs/remotes/aaa-mirror/main"]
        ),
        mirror_head,
        "fetch should update only the selected remote"
    );
    assert_eq!(
        git_ok(repository.path(), &["config", "branch.main.remote"]),
        "origin"
    );
    tui.quit();
}

#[test]
fn push_picker_selects_remote_before_push_mode() {
    let repository = init_linear_repository();
    let remote_dir = TempDir::new().expect("remote repository directory");
    let remote = remote_dir.path().join("remote.git");
    let remote_arg = remote.to_str().unwrap();
    git_ok(
        repository.path(),
        &["clone", "--quiet", "--bare", ".", remote_arg],
    );
    git_ok(repository.path(), &["remote", "add", "origin", remote_arg]);
    git_ok(repository.path(), &["fetch", "--quiet", "origin"]);
    git_ok(
        repository.path(),
        &["remote", "add", "aaa-mirror", remote_arg],
    );
    git_ok(repository.path(), &["fetch", "--quiet", "aaa-mirror"]);
    git_ok(
        repository.path(),
        &["branch", "--set-upstream-to=origin/main", "main"],
    );

    let mut tui = Tui::launch(repository.path());
    tui.wait_for("Graph");
    tui.send(b"+");
    tui.wait_for("Push to remote");
    tui.wait_for("aaa-mirror");
    tui.wait_for("origin");
    tui.wait_for_text_background("origin", vt100::Color::Idx(14));
    tui.send(b"\x1b[A"); // Select aaa-mirror instead of the default origin.
    tui.wait_for_text_background("aaa-mirror", vt100::Color::Idx(14));
    tui.send(b"\r");
    tui.wait_for("Select push mode");
    tui.send(b"\x1b");
    tui.wait_until_hidden("Select push mode");
    assert_eq!(
        git_ok(repository.path(), &["config", "branch.main.remote"]),
        "origin"
    );
    assert!(!git(
        repository.path(),
        &["config", "--get", "branch.main.pushRemote"]
    )
    .status
    .success());
    tui.quit();
}

#[test]
fn plus_pushes_the_selected_non_head_branch_and_returns_to_graph() {
    let repository = init_linear_repository();
    let remote_dir = TempDir::new().expect("remote repository directory");
    let remote = remote_dir.path().join("remote.git");
    let remote_arg = remote.to_str().unwrap();
    git_ok(
        repository.path(),
        &["clone", "--quiet", "--bare", ".", remote_arg],
    );
    git_ok(repository.path(), &["remote", "add", "origin", remote_arg]);
    git_ok(repository.path(), &["fetch", "--quiet", "origin"]);
    git_ok(repository.path(), &["branch", "feature"]);
    git_ok(repository.path(), &["checkout", "--quiet", "feature"]);
    git_ok(
        repository.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "feature tip"],
    );
    git_ok(
        repository.path(),
        &["branch", "--set-upstream-to=origin/main", "feature"],
    );
    git_ok(repository.path(), &["checkout", "--quiet", "main"]);
    let feature_tip = git_ok(repository.path(), &["rev-parse", "feature"]);

    let mut tui = Tui::launch(repository.path());
    tui.wait_for("feature tip");
    // Startup selects HEAD (main); move up to the feature branch tip.
    tui.send(b"\x1b[A");
    tui.wait_for_text_background("feature tip", vt100::Color::Rgb(45, 50, 65));
    tui.send(b"+");
    tui.wait_for("Select push mode");
    tui.send(b"\r");
    tui.wait_for("Push complete");

    assert_eq!(
        git_ok(&remote, &["rev-parse", "refs/heads/feature"]),
        feature_tip
    );
    assert_eq!(
        git_ok(&remote, &["rev-parse", "refs/heads/main"]),
        git_ok(repository.path(), &["rev-parse", "main"]),
        "pushing the selected branch should leave main alone"
    );
    tui.wait_for_graph_focus();
    tui.wait_for_text_background("feature tip", vt100::Color::Rgb(45, 50, 65));
    tui.quit();
}

#[test]
fn deleting_selected_branch_keeps_graph_focus_and_selects_a_remaining_commit() {
    let repository = init_linear_repository();
    git_ok(repository.path(), &["branch", "ephemeral"]);
    git_ok(repository.path(), &["checkout", "--quiet", "ephemeral"]);
    git_ok(
        repository.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "ephemeral tip"],
    );
    git_ok(repository.path(), &["checkout", "--quiet", "main"]);

    let mut tui = Tui::launch(repository.path());
    tui.wait_for("ephemeral tip");
    // Startup selects HEAD (main); move up to the ephemeral branch tip.
    tui.send(b"\x1b[A");
    tui.wait_for_text_background("ephemeral tip", vt100::Color::Rgb(45, 50, 65));
    tui.send(b"d");
    tui.wait_for("Delete branch ephemeral?");
    tui.send(b"\r");
    tui.wait_for("3 commits");
    tui.wait_for_graph_focus();
    tui.wait_for_text_background("tip commit", vt100::Color::Rgb(45, 50, 65));
    assert!(!git(
        repository.path(),
        &["show-ref", "--verify", "--quiet", "refs/heads/ephemeral"]
    )
    .status
    .success());
    tui.quit();
}

#[test]
fn failed_fetch_reports_the_git_error_after_background_completion() {
    let repository = init_linear_repository();
    let missing_remote = repository.path().join("missing-remote.git");
    git_ok(
        repository.path(),
        &["remote", "add", "origin", missing_remote.to_str().unwrap()],
    );
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("Graph");

    tui.send(b"F");
    tui.wait_for("Git operation error");
    tui.wait_for("Fetch failed");
    tui.send(b"\x1b");
    tui.wait_until_hidden("Git operation error");
    tui.quit();
}

#[test]
fn external_worktree_and_commit_changes_refresh_the_tui() {
    let repository = init_linear_repository();
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("3 commits");
    tui.wait_for("Commit Details");

    std::fs::write(repository.path().join("file.txt"), "external edit\n").unwrap();
    // The local-changes row appears above the selected HEAD commit.
    tui.wait_for("Local changes");
    tui.send(b"\x1b[A");
    tui.wait_for("Unstaged (1)");
    tui.wait_for("file.txt");
    assert!(tui.contents().contains("external edit"));

    git_ok(repository.path(), &["add", "file.txt"]);
    git_ok(
        repository.path(),
        &["commit", "--quiet", "-m", "external commit"],
    );
    tui.wait_for("external commit");
    tui.wait_for("4 commits");
    tui.wait_for("Unstaged (0)");
    tui.quit();
}

#[test]
fn external_rebase_refreshes_the_graph() {
    let repository = init_linear_repository();
    let path = repository.path();
    git_ok(path, &["checkout", "--quiet", "-b", "feature", "HEAD~1"]);
    std::fs::write(path.join("feature.txt"), "feature\n").unwrap();
    git_ok(path, &["add", "feature.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "feature work"]);
    let old_feature = git_ok(path, &["rev-parse", "HEAD"]);
    git_ok(path, &["checkout", "--quiet", "main"]);

    let mut tui = Tui::launch(path);
    tui.wait_for("feature work");
    tui.wait_for(&old_feature[..7]);

    // An external agent rewrites the branch while the TUI is running.
    git_ok(path, &["rebase", "--quiet", "main", "feature"]);
    let new_feature = git_ok(path, &["rev-parse", "feature"]);
    assert_ne!(old_feature, new_feature);

    tui.wait_for(&new_feature[..7]);
    tui.wait_for("feature work");
    tui.quit();
}

#[test]
fn external_rebase_refreshes_a_tui_running_in_a_linked_worktree() {
    let repository = init_linear_repository();
    let path = repository.path();
    let linked_parent = TempDir::new().expect("linked worktree parent");
    let linked = linked_parent.path().join("linked");
    git_ok(
        path,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "linked",
            linked.to_str().unwrap(),
            "HEAD~1",
        ],
    );

    let mut tui = Tui::launch(&linked);
    tui.wait_for("tip commit");

    // Rewriting a ref in the main worktree only touches the common git dir.
    std::fs::write(path.join("file.txt"), "rewritten\n").unwrap();
    git_ok(path, &["commit", "--quiet", "-am", "rewritten main"]);

    tui.wait_for("rewritten main");
    tui.quit();
}

#[test]
fn rebase_onto_popup_rewrites_the_current_branch() {
    let repository = init_linear_repository();
    let path = repository.path();
    git_ok(path, &["checkout", "--quiet", "-b", "feature", "HEAD~1"]);
    std::fs::write(path.join("feature.txt"), "feature\n").unwrap();
    git_ok(path, &["add", "feature.txt"]);
    git_ok(path, &["commit", "--quiet", "-m", "feature work"]);
    let old_feature = git_ok(path, &["rev-parse", "HEAD"]);

    let mut tui = Tui::launch(path);
    tui.wait_for("feature work");

    // Pick the main tip commit as the new base via search.
    tui.send(b"/");
    tui.wait_for("Search /");
    tui.send(b"tip commit");
    tui.send(b"\r");
    tui.wait_for("1 match");
    tui.send(b"\r");
    tui.send(b"\x1b");
    tui.wait_until_hidden("Search /");

    tui.send(b"R");
    tui.wait_for("Rebase onto");
    tui.send(b"\r");

    let new_feature = wait_for_branch_change(path, "feature", &old_feature);
    tui.wait_for(&new_feature[..7]);
    assert_eq!(
        git_ok(path, &["rev-parse", "HEAD~1"]),
        git_ok(path, &["rev-parse", "main"])
    );
    tui.quit();
}

fn wait_for_branch_change(path: &Path, branch: &str, old: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let current = git_ok(path, &["rev-parse", branch]);
        if current != old {
            return current;
        }
        assert!(Instant::now() < deadline, "branch {branch} was not rebased");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn older_history_loads_lazily_as_the_selection_reaches_the_end() {
    let repository = init_linear_repository();
    // 5 commits: three from the fixture plus the two added here.
    git_ok(
        repository.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "extra one"],
    );
    git_ok(
        repository.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "extra two"],
    );
    let head = git_ok(repository.path(), &["rev-parse", "HEAD"]);

    // A tiny page size makes paging observable without scrolling 2000 rows.
    let mut tui = Tui::launch_with_env(repository.path(), None, &[("FIGIT_GRAPH_PAGE", "3")]);
    tui.wait_for("3+ commits");

    // The oldest commit is not loaded yet.
    assert!(
        !tui.contents().contains("root commit"),
        "older history should not be loaded up front:\n{}",
        tui.contents()
    );

    // Walk to the end of the loaded page to pull in the rest.
    for _ in 0..6 {
        tui.send(b"\x1b[B");
    }
    tui.wait_for("5 commits");
    // Appending must not disturb the row the selection was on.
    tui.wait_for("extra two");
    assert_eq!(git_ok(repository.path(), &["rev-parse", "HEAD"]), head);
    tui.quit();
}

#[test]
fn search_finds_commit_titles_and_descriptions() {
    let repository = init_linear_repository();
    git_ok(
        repository.path(),
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Ordinary title",
            "-m",
            "Secret description phrase",
        ],
    );
    git_ok(
        repository.path(),
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "needle first commit",
        ],
    );
    git_ok(
        repository.path(),
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "second needle commit",
        ],
    );
    git_ok(
        repository.path(),
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "unrelated subject",
        ],
    );
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("7 commits");

    // A phrase that only appears in one commit's description body.
    tui.send(b"/");
    tui.wait_for("Search /");
    tui.send(b"secret description phrase");
    tui.send(b"\r");
    tui.wait_for("1 match · 1/1");
    tui.wait_for("Ordinary title");
    tui.wait_for_text_background("Secret description phrase", vt100::Color::Idx(11));
    tui.send(b"\x1b");

    // A case-insensitive title-only query with two matches.
    tui.send(b"/");
    tui.wait_for("Search /");
    tui.send(b"NEEDLE");
    tui.send(b"\r");
    tui.wait_for("2 matches · 1/2");
    tui.send(b"\x1b[B");
    tui.wait_for("2 matches · 2/2");
    tui.send(b"\x1b");
    tui.wait_until_hidden("Search /");
    tui.quit();
}

#[test]
fn stash_manager_saves_applies_pops_and_drops_stashes() {
    let repository = init_stash_repository();
    let mut tui = Tui::launch(repository.path());

    tui.wait_for("Unstaged (2)");
    tui.send(b"s");
    tui.wait_for("Stashes (0)");
    tui.send(b"u");
    tui.send(b"s");
    tui.wait_for("Stash message");
    tui.send(b"\x7f\x7f\x7f");
    tui.send(b"before experiment");
    tui.send(b"\r");
    tui.wait_until_hidden("Stashes (0)");
    tui.wait_for("Unstaged (0)");
    assert_eq!(
        git_ok(repository.path(), &["stash", "list", "--format=%s"])
            .lines()
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(repository.path().join("file.txt"))
            .unwrap()
            .lines()
            .next(),
        Some("first base")
    );
    assert!(!repository.path().join("loose.txt").exists());

    tui.send(b"S");
    tui.wait_for("Stashes (1)");
    assert!(tui.contents().contains("before experiment"));
    tui.send(b"p");
    tui.wait_until_hidden("Stashes (1)");
    tui.wait_for("Unstaged (2)");
    assert!(std::fs::read_to_string(repository.path().join("file.txt"))
        .unwrap()
        .contains("first ours"));
    assert_eq!(
        std::fs::read_to_string(repository.path().join("loose.txt")).unwrap(),
        "untracked content\n"
    );

    tui.send(b"S");
    tui.wait_for("Stashes (0)");
    tui.send(b"u");
    tui.send(b"s");
    tui.wait_for("Stash message");
    tui.send(b"\x7f\x7f\x7f");
    tui.send(b"drop me");
    tui.send(b"\r");
    tui.wait_until_hidden("Stashes (0)");
    tui.send(b"S");
    tui.wait_for("Stashes (1)");
    tui.send(b"d");
    tui.wait_for("Drop stash@{0}");
    tui.send(b"y");
    tui.wait_for("Stashes (0)");
    tui.send(b"\x1b");
    tui.wait_until_hidden("Stashes (0)");
    assert!(git_ok(repository.path(), &["stash", "list"]).is_empty());

    tui.quit();
}

#[test]
fn stash_from_a_selected_commit_keeps_the_commit_file_list() {
    let repository = init_commit_and_local_change_repository();
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("tracked-b.txt");

    // Focus the graph and select the tip commit ("update tracked a").
    tui.send(b"\t");
    tui.send(b"\x1b[B");
    tui.wait_for("Commit Details");
    tui.wait_for("tracked-a.txt");

    // Stash the working-tree change while that commit is still selected. The
    // stash closes and refreshes status within the same frame.
    tui.send(b"s");
    tui.wait_for("Stashes (0)");
    tui.send(b"s");
    tui.send(b"\r");
    tui.wait_until_hidden("Stashes (0)");

    let screen = tui.contents();
    assert!(
        screen.contains("tracked-a.txt"),
        "the selected commit's file list should survive a stash; terminal:\n{screen}"
    );
    assert!(
        !screen.contains("tracked-b.txt"),
        "the worktree file must not replace the commit's file list; terminal:\n{screen}"
    );
    tui.quit();
}

#[test]
fn reword_and_edit_shortcuts_execute_interactive_rebase_after_confirmation() {
    let repository = init_linear_repository();
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("tip commit");

    // Reword the middle commit and confirm the rebase from its message editor.
    tui.send(b"\x1b[B");
    tui.send(b"w");
    tui.wait_for("Reword selected commit");
    tui.send(b"\x1b[D\x1b[C"); // Reword uses the same arrow-editable message fields.
    tui.assert_cursor_after("target commit");
    for _ in 0..13 {
        tui.send(b"\x7f");
    }
    tui.send(b"renamed commit");
    tui.send(b"\r");
    tui.send(b"updated body");
    tui.assert_cursor_after("updated body");
    tui.send(b"\r\r");
    tui.wait_until_hidden("Reword selected commit");
    assert_eq!(
        git_ok(repository.path(), &["log", "--format=%s", "-3"]),
        "tip commit\nrenamed commit\nroot commit"
    );
    let pre_edit_head = git_ok(repository.path(), &["rev-parse", "HEAD"]);

    // Reload returns focus to Status. Select the rewritten middle commit and
    // confirm `s` to stop the next interactive rebase there for editing.
    tui.send(b"\t\x1b[B");
    tui.send(b"e");
    tui.wait_for("Rebase and stop to edit commit");
    tui.send(b"\r");
    tui.wait_for("edit paused");
    tui.wait_for("Staged (2)");
    assert_eq!(
        git_ok(repository.path(), &["show", "-s", "--format=%s", "HEAD"]),
        "root commit"
    );
    let state_path = git_ok(
        repository.path(),
        &["rev-parse", "--git-path", "rebase-merge"],
    );
    assert!(repository.path().join(state_path).exists());

    // The selected commit's patch is staged. Confirm a replacement commit.
    tui.send(b"c");
    tui.wait_for("Replace edited commit");
    tui.send(b"\r\r\r");
    tui.wait_until_hidden("Replace edited commit");
    assert_eq!(
        git_ok(repository.path(), &["show", "-s", "--format=%s", "HEAD"]),
        "renamed commit"
    );
    tui.send(b"\x01"); // Ctrl+A aborts globally from the Status panel.
    tui.wait_for("Abort rebase");
    tui.send(b"\r");
    tui.wait_until_hidden("Abort rebase");
    assert_eq!(
        git_ok(repository.path(), &["rev-parse", "HEAD"]),
        pre_edit_head
    );
    assert!(!repository
        .path()
        .join(git_ok(
            repository.path(),
            &["rev-parse", "--git-path", "rebase-merge"]
        ))
        .exists());
    assert_eq!(
        git_ok(repository.path(), &["log", "--format=%s", "-3"]),
        "tip commit\nrenamed commit\nroot commit"
    );

    // Start the edit again, split the staged two-file commit into two commits,
    // and continue with Ctrl+C.
    tui.send(b"\t\x1b[B");
    tui.send(b"e");
    tui.wait_for("Rebase and stop to edit commit");
    tui.send(b"\r");
    tui.wait_for("edit paused");
    tui.wait_for("Staged (2)");

    // Unstage file.txt so it can become a separate follow-up commit.
    tui.send(b" ");
    tui.wait_for("Unstaged (1)");
    tui.wait_for("Staged (1)");

    // The replacement commit now contains only other.txt.
    tui.send(b"c");
    tui.wait_for("Replace edited commit");
    tui.send(b"\r\r\r");
    tui.wait_until_hidden("Replace edited commit");

    // Commit the remaining file as a second commit in the paused rebase.
    tui.wait_for("Unstaged (1)");
    tui.send(b" ");
    tui.wait_for("Staged (1)");
    tui.send(b"c");
    tui.wait_for("Title");
    tui.send(b"split remainder");
    tui.send(b"\r\r\r");
    tui.wait_for("Staged (0)");
    tui.wait_for("Ctrl+C continue");
    tui.send(b"\x03");
    tui.wait_until_hidden("rebase paused");
    assert_eq!(
        git_ok(repository.path(), &["log", "--format=%s", "-4"]),
        "tip commit\nsplit remainder\nrenamed commit\nroot commit"
    );
    assert_eq!(
        git_ok(
            repository.path(),
            &["show", "--format=", "--name-only", "HEAD~2"]
        ),
        "other.txt"
    );
    assert_eq!(
        git_ok(
            repository.path(),
            &["show", "--format=", "--name-only", "HEAD~1"]
        ),
        "file.txt"
    );

    tui.quit();
}

#[test]
fn conflict_resolver_panel_renders_and_resolves_with_theirs() {
    let repository = init_conflict_repository();
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("Three-way merge — file.txt");
    tui.wait_for("Ours");
    tui.wait_for("Theirs");
    tui.wait_for("Result");
    tui.wait_for("o/t/b choose");
    let expanded = tui.contents();
    assert!(
        expanded.contains("Graph"),
        "the graph should remain visible beside the resolver"
    );
    assert!(
        expanded.contains("Unstaged (1)"),
        "the staging panel should remain visible beside the resolver"
    );
    assert!(
        expanded.contains("Ours"),
        "missing ours column:\n{expanded}"
    );
    assert!(
        expanded.contains("Theirs"),
        "missing theirs column:\n{expanded}"
    );
    assert!(
        expanded.contains("Result"),
        "missing result panel:\n{expanded}"
    );
    assert!(
        expanded.contains("Base → Ours"),
        "Ours should show its diff against Base:\n{expanded}"
    );
    assert!(
        expanded.contains("Base → Theirs"),
        "Theirs should show its diff against Base:\n{expanded}"
    );
    assert!(
        expanded.contains("-base") && expanded.contains("+ours") && expanded.contains("+theirs"),
        "side panes should render their base-relative changes:\n{expanded}"
    );
    assert!(
        expanded.contains("o/t/b choose"),
        "side-choice hints belong in the merge panel:\n{expanded}"
    );
    assert!(
        !expanded.contains("o ours • t theirs"),
        "the Unstaged footer should not repeat merge hints"
    );
    {
        let screen = tui.screen.lock().expect("screen lock");
        let conflict_row = expanded
            .lines()
            .position(|line| line.contains("file.txt"))
            .expect("conflicting file should be visible in Unstaged");
        let column = expanded
            .lines()
            .nth(conflict_row)
            .unwrap()
            .find("file.txt")
            .unwrap();
        let column = expanded.lines().nth(conflict_row).unwrap()[..column]
            .chars()
            .count();
        assert_eq!(
            screen
                .screen()
                .cell(conflict_row as u16, column as u16)
                .unwrap()
                .bgcolor(),
            vt100::Color::Rgb(45, 50, 65),
            "the file being resolved should stay highlighted in Unstaged"
        );
    }
    let side_header = expanded
        .lines()
        .position(|line| line.contains("Ours") && line.contains("Theirs"))
        .expect("ours and theirs should be side by side");
    let result_header = expanded
        .lines()
        .position(|line| line.contains("Result"))
        .expect("result pane visible");
    assert!(
        result_header > side_header,
        "result pane should be below the side panes:\n{expanded}"
    );
    let resolver_title = expanded
        .lines()
        .find(|line| line.contains("Three-way merge — file.txt"))
        .expect("resolver title row");
    assert!(
        resolver_title.starts_with('┌') && resolver_title.ends_with('┐'),
        "resolver should span the terminal width:\n{resolver_title}"
    );
    let base_id = git_ok(repository.path(), &["merge-base", "HEAD", "feature"]);
    let base_short = git_ok(repository.path(), &["rev-parse", "--short=7", &base_id]);
    {
        let screen = tui.screen.lock().expect("screen lock");
        let resolver_row = expanded
            .lines()
            .position(|line| line.contains("Three-way merge — file.txt"))
            .unwrap();
        assert_eq!(
            screen
                .screen()
                .cell(resolver_row as u16, 0)
                .unwrap()
                .fgcolor(),
            vt100::Color::Rgb(255, 105, 180),
            "three-way panel border should be pink"
        );
        let base_row = expanded
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains(&base_short))
            .expect("merge-base commit should appear in the graph");
        let byte_column = base_row.1.find(&base_short).unwrap();
        let column = base_row.1[..byte_column].chars().count();
        assert_eq!(
            screen
                .screen()
                .cell(base_row.0 as u16, column as u16)
                .unwrap()
                .fgcolor(),
            vt100::Color::Idx(13),
            "merge-base SHA should be purple"
        );
    }
    // In a merge, Git's --theirs is the incoming feature-branch version. The
    // choice previews in place; Enter confirms and stages it.
    tui.send(b"t");
    tui.wait_for("Result vs Base — Theirs");
    tui.wait_for("Staged (0)");
    let preview = tui.contents();
    assert!(
        preview.contains("Result vs Base — Theirs"),
        "selected side should be shown in the pane title:\n{preview}"
    );
    assert!(
        preview.contains("-base"),
        "result should diff against Base:\n{preview}"
    );
    assert!(
        preview.contains("+theirs"),
        "chosen side should appear in Result:\n{preview}"
    );
    assert!(
        preview.contains("Unstaged (1)"),
        "the conflict should remain pending until confirmation"
    );
    {
        let screen = tui.screen.lock().expect("screen lock");
        let side_header = preview
            .lines()
            .position(|line| line.contains("Theirs") && line.contains("Ours"))
            .expect("side pane headers");
        let header = preview.lines().nth(side_header).unwrap();
        let ours_title_column = header[..header.find("Ours").unwrap()].chars().count();
        let theirs_title_column = header[..header.find("Theirs").unwrap()].chars().count();
        let ours_border_column = header[..header.find('┌').unwrap()].chars().count();
        let theirs_border_column = header[..header.rfind('┌').unwrap()].chars().count();
        let cell_color = |column: usize| {
            screen
                .screen()
                .cell(side_header as u16, column as u16)
                .unwrap()
                .fgcolor()
        };
        assert_eq!(
            cell_color(theirs_border_column),
            cell_color(theirs_title_column),
            "selected Theirs border should retain its commit color"
        );
        let ours_color = cell_color(ours_title_column);
        assert!(
            matches!(ours_color, vt100::Color::Idx(12))
                || matches!(ours_color, vt100::Color::Rgb(red, green, blue) if blue > red && blue > green),
            "Ours should use blue, got {ours_color:?}"
        );
        let theirs_color = cell_color(theirs_title_column);
        assert!(
            matches!(theirs_color, vt100::Color::Idx(11))
                || matches!(theirs_color, vt100::Color::Rgb(red, green, blue) if red > blue && green > blue),
            "Theirs should use yellow, got {theirs_color:?}"
        );
        assert_ne!(
            cell_color(ours_border_column),
            cell_color(ours_title_column),
            "non-selected Ours border should be dimmed"
        );
        let side_row = preview
            .lines()
            .enumerate()
            .skip(side_header + 1)
            .find(|(_, line)| line.contains("+theirs"))
            .map(|(row, _)| row)
            .expect("selected Theirs pane row");
        let column = preview
            .lines()
            .nth(side_row)
            .unwrap()
            .find("theirs")
            .unwrap();
        let column = preview.lines().nth(side_row).unwrap()[..column]
            .chars()
            .count();
        let theirs_commit_color = cell_color(theirs_title_column);
        let expected_diff_background = match theirs_commit_color {
            vt100::Color::Rgb(red, green, blue) => vt100::Color::Rgb(red / 2, green / 2, blue / 2),
            vt100::Color::Idx(11) => vt100::Color::Rgb(127, 127, 42),
            vt100::Color::Idx(index @ 9..=15) => vt100::Color::Idx(index - 8),
            other => panic!("expected bright Theirs color, got {other:?}"),
        };
        let side_background = screen
            .screen()
            .cell(side_row as u16, column as u16)
            .unwrap()
            .bgcolor();
        assert_eq!(
            side_background, expected_diff_background,
            "diff lines should use the dimmed Theirs color, without a separate selection overlay"
        );

        let result_header = preview
            .lines()
            .position(|line| line.contains("Result vs Base — Theirs"))
            .unwrap();
        for (needle, expected) in [("-base", "red"), ("+theirs", "yellow")] {
            let (row, line) = preview
                .lines()
                .enumerate()
                .skip(result_header + 1)
                .find(|(_, line)| line.contains(needle))
                .expect("base-relative result line");
            let column = line[..line.find(needle).unwrap()].chars().count();
            let color = screen
                .screen()
                .cell(row as u16, column as u16)
                .unwrap()
                .fgcolor();
            let matches_expected = match expected {
                "red" => {
                    matches!(color, vt100::Color::Idx(9))
                        || matches!(color, vt100::Color::Rgb(red, green, blue) if red > green && red > blue)
                }
                _ => {
                    matches!(color, vt100::Color::Idx(11))
                        || matches!(color, vt100::Color::Rgb(red, green, blue) if red > blue && green > blue)
                }
            };
            assert!(
                matches_expected,
                "{needle} should be {expected}, got {color:?}"
            );
        }
    }
    assert!(git_ok(repository.path(), &["status", "--porcelain"]).contains("UU file.txt"));
    tui.send(b"\r");
    tui.wait_for("Staged (1)");
    tui.wait_until_hidden("Three-way merge — file.txt");
    assert_eq!(
        std::fs::read_to_string(repository.path().join("file.txt")).unwrap(),
        "theirs\n",
        "terminal:\n{}",
        tui.contents()
    );
    git_ok(repository.path(), &["merge", "--abort"]);
    tui.quit();
}

#[test]
fn conflict_resolver_can_open_the_external_editor() {
    let repository = init_conflict_repository();
    let editor_dir = TempDir::new().expect("temporary editor script directory");
    let editor = editor_dir.path().join("resolve-conflict.sh");
    std::fs::write(
        &editor,
        "#!/bin/sh\nprintf 'resolved by editor\\n' > \"$1\"\n",
    )
    .unwrap();
    let editor_command = format!("sh {}", editor.display());
    let mut tui = Tui::launch_with_editor(repository.path(), Some(&editor_command));
    tui.wait_for("Three-way merge — file.txt");
    tui.send(b"e");
    tui.wait_for("Result vs Base — Edited");
    tui.wait_for("Staged (0)");
    tui.send(b"\r");
    tui.wait_for("Staged (1)");
    tui.wait_until_hidden("Three-way merge — file.txt");
    assert_eq!(
        std::fs::read_to_string(repository.path().join("file.txt")).unwrap(),
        "resolved by editor\n"
    );
    git_ok(repository.path(), &["merge", "--abort"]);
    tui.quit();
}

#[test]
fn graph_selection_keeps_the_active_conflict_resolver_visible() {
    let repository = init_conflict_repository();
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("Three-way merge — file.txt");

    // Move focus to the Graph and select a commit, as a graph click would.
    tui.send(b"\t\t\x1b[B");
    tui.wait_for("Three-way merge — file.txt");
    tui.send(b"t");
    tui.wait_for("Result vs Base — Theirs");
    let screen = tui.contents();
    assert!(screen.contains("Graph"));
    assert!(
        screen.contains("Three-way merge — file.txt"),
        "resolver should remain pinned:\n{screen}"
    );
    tui.send(b"\r");
    tui.wait_for("Staged (1)");
    tui.wait_until_hidden("Three-way merge — file.txt");
    assert_eq!(
        std::fs::read_to_string(repository.path().join("file.txt")).unwrap(),
        "theirs\n"
    );

    git_ok(repository.path(), &["merge", "--abort"]);
    tui.quit();
}

#[test]
fn conflict_result_confirmation_advances_to_the_next_file() {
    let repository = init_two_file_conflict_repository();
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("Three-way merge — file.txt");
    assert!(tui.contents().contains("Unstaged (2)"));

    tui.send(b"t");
    tui.wait_for("Result vs Base — Theirs");
    assert!(tui.contents().contains("Unstaged (2)"));
    assert!(tui.contents().contains("Staged (0)"));
    tui.send(b"\r");

    tui.wait_for("Three-way merge — other.txt");
    let next_file = tui.contents();
    assert!(
        next_file.contains("Unstaged (1)"),
        "one conflict should remain:\n{next_file}"
    );
    assert!(
        next_file.contains("Staged (1)"),
        "the confirmed file should be staged:\n{next_file}"
    );
    assert_eq!(
        std::fs::read_to_string(repository.path().join("file.txt")).unwrap(),
        "theirs\n"
    );

    tui.send(b"o");
    tui.wait_for("Result vs Base — Ours");
    assert!(tui.contents().contains("Three-way merge — other.txt"));
    tui.send(b"\r");
    tui.wait_until_hidden("Three-way merge — other.txt");
    assert!(tui.contents().contains("Staged (1)"));
    assert_eq!(
        std::fs::read_to_string(repository.path().join("other.txt")).unwrap(),
        "ours\n"
    );

    git_ok(repository.path(), &["merge", "--abort"]);
    tui.quit();
}

#[test]
fn conflict_hunks_can_be_chosen_and_confirmed_individually() {
    let repository = init_multi_hunk_conflict_repository();
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("Hunk 1/2");

    tui.send(b"[");
    tui.wait_for("Hunk 2/2");
    tui.send(b"]");
    tui.wait_for("Hunk 1/2");
    tui.send(b"]");
    tui.wait_for("Hunk 2/2");
    tui.send(b"o");
    tui.wait_for("Result vs Base — Ours");
    let second_preview = tui.contents();
    assert!(second_preview.contains("Hunk 2/2"));
    assert_eq!(
        std::fs::read_to_string(repository.path().join("file.txt"))
            .unwrap()
            .matches("<<<<<<<")
            .count(),
        2,
        "preview must not mutate the conflict before confirmation"
    );
    tui.send(b"\r");

    tui.wait_for("Hunk 1/1");
    let after_first = std::fs::read_to_string(repository.path().join("file.txt")).unwrap();
    assert!(after_first.contains("second ours"));
    assert!(
        after_first.contains("<<<<<<<"),
        "the remaining hunk stays unresolved"
    );
    assert!(!after_first.contains("second theirs"));

    tui.send(b"t");
    tui.wait_for("Result vs Base — Theirs");
    tui.send(b"\r");
    tui.wait_for("Staged (1)");
    tui.wait_until_hidden("Three-way merge — file.txt");
    let resolved = std::fs::read_to_string(repository.path().join("file.txt")).unwrap();
    assert!(resolved.contains("first theirs"));
    assert!(resolved.contains("second ours"));
    assert!(!resolved.contains("<<<<<<<"));

    git_ok(repository.path(), &["merge", "--abort"]);
    tui.quit();
}

#[test]
fn interactive_rebase_plan_rewords_a_todo_item_before_running() {
    let repository = init_linear_repository();
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("tip commit");

    tui.send(b"\x1b[B");
    tui.send(b"i");
    tui.wait_for("Interactive rebase");
    tui.wait_for("target commit");

    tui.send(b"w");
    tui.wait_for("Reword in rebase plan");
    for _ in 0..13 {
        tui.send(b"\x7f");
    }
    tui.send(b"planned target");
    tui.send(b"\r\r\r");
    tui.wait_until_hidden("Reword in rebase plan");
    tui.wait_for("reword");

    tui.send(b"\r");
    tui.wait_until_hidden("Interactive rebase");
    assert_eq!(
        git_ok(repository.path(), &["log", "--format=%s", "-3"]),
        "tip commit\nplanned target\nroot commit"
    );

    tui.quit();
}

#[test]
fn interactive_rebase_conflicts_can_be_resolved_and_continued() {
    let repository = init_linear_repository();
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("tip commit");

    // Reordering these dependent patches makes Git report a conflict.
    tui.send(b"\x1b[B");
    tui.send(b"i");
    tui.wait_for("Interactive rebase");
    tui.send(b"\x1b[B");
    tui.send(b"\x1b[1;3A"); // Alt+Up moves tip before its prerequisite.
    tui.send(b"\r");

    tui.wait_for("Rebase error");
    tui.wait_for("could not apply");
    let error_screen = tui.contents();
    assert!(
        error_screen.contains("all conflicts manually"),
        "full error text should be readable:\n{error_screen}"
    );

    tui.send(b"\x1b");
    tui.wait_until_hidden("Rebase error");
    tui.wait_for("Three-way merge — file.txt");
    let short_id =
        |revision: &str| git_ok(repository.path(), &["rev-parse", "--short=7", revision]);
    let root = short_id("HEAD");
    let incoming = short_id("REBASE_HEAD");
    {
        let screen = tui.screen.lock().expect("screen lock");
        let contents = screen.screen().contents();
        let side_header = contents
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("Ours") && line.contains("Theirs"))
            .unwrap_or_else(|| panic!("side panes should be visible together:\n{contents}"));
        let color_at = |needle: &str| {
            let byte_column = side_header.1.find(needle).unwrap();
            let column = side_header.1[..byte_column].chars().count();
            screen
                .screen()
                .cell(side_header.0 as u16, column as u16)
                .unwrap()
                .fgcolor()
        };
        let ours_color = color_at(&root);
        let theirs_color = color_at(&incoming);
        assert!(
            matches!(ours_color, vt100::Color::Idx(12))
                || matches!(ours_color, vt100::Color::Rgb(red, green, blue) if blue > red && blue > green),
            "ours SHA should be blue, got {ours_color:?}"
        );
        assert!(
            matches!(theirs_color, vt100::Color::Idx(11))
                || matches!(theirs_color, vt100::Color::Rgb(red, green, blue) if red > blue && green > blue),
            "incoming SHA should be yellow, got {theirs_color:?}"
        );
        let ours_graph_row = contents
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains(&root))
            .unwrap_or_else(|| panic!("ours/base commit SHA should be in the graph:\n{contents}"));
        let byte_column = ours_graph_row.1.find(&root).unwrap();
        let column = ours_graph_row.1[..byte_column].chars().count();
        let base_sha_color = screen
            .screen()
            .cell(ours_graph_row.0 as u16, column as u16)
            .unwrap()
            .fgcolor();
        assert_eq!(
            base_sha_color,
            vt100::Color::Idx(12),
            "when merge-base and Ours are the same commit, keep its SHA blue"
        );
        assert!(contents.contains("Result"));
        assert!(contents.contains("<<<<<<<"));
        assert!(contents.contains(">>>>>>>"));
    }
    tui.send(b"t");
    tui.wait_for("Result vs Base — Theirs");
    tui.wait_for("Staged (0)");
    tui.send(b"\r");
    tui.wait_for("Staged (1)");
    tui.send(b"\x03"); // Continue, which reaches a second conflicting todo item.
    tui.wait_for("Rebase error");
    tui.wait_for("could not apply");
    tui.send(b"\x1b");
    tui.wait_until_hidden("Rebase error");
    tui.wait_for("Three-way merge — file.txt");
    tui.send(b"t");
    tui.wait_for("Result vs Base — Theirs");
    tui.send(b"\r");
    tui.wait_for("Staged (2)");
    tui.send(b"\x03");
    let rebase_state = repository.path().join(git_ok(
        repository.path(),
        &["rev-parse", "--git-path", "rebase-merge"],
    ));
    let deadline = Instant::now() + Duration::from_secs(8);
    while rebase_state.exists() {
        assert!(
            Instant::now() < deadline,
            "rebase still active after Ctrl+C; terminal:\n{}",
            tui.contents()
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        git_ok(repository.path(), &["log", "--format=%s", "-3"]),
        "target commit\ntip commit\nroot commit"
    );
    assert_eq!(
        std::fs::read_to_string(repository.path().join("file.txt")).unwrap(),
        "target\n"
    );
    assert!(!repository
        .path()
        .join(git_ok(
            repository.path(),
            &["rev-parse", "--git-path", "rebase-merge"]
        ))
        .exists());
    tui.quit();
}

#[test]
fn creating_a_tag_can_push_it() {
    let repository = init_linear_repository();
    let remote_dir = TempDir::new().expect("remote repository directory");
    let remote = remote_dir.path().join("remote.git");
    let remote_arg = remote.to_str().unwrap();
    git_ok(
        repository.path(),
        &["clone", "--quiet", "--bare", ".", remote_arg],
    );
    git_ok(repository.path(), &["remote", "add", "origin", remote_arg]);
    git_ok(repository.path(), &["fetch", "--quiet", "origin"]);

    let mut tui = Tui::launch(repository.path());
    tui.wait_for("tip commit");
    tui.send(b"t");
    tui.wait_for("New tag");
    tui.send(b"v1");
    tui.send(b"\x10"); // Ctrl-P toggles "push after create"
    tui.wait_for("Push after: yes");
    tui.send(b"\r");
    tui.wait_for("Push complete");
    assert_eq!(
        git_ok(&remote, &["rev-parse", "refs/tags/v1"]),
        git_ok(repository.path(), &["rev-parse", "HEAD"]),
        "the new tag should reach the remote"
    );
    tui.quit();
}

#[test]
fn push_shortcut_pushes_the_selected_tag() {
    let repository = init_linear_repository();
    // Tag a commit that has no local branch so `+` targets the tag.
    git_ok(repository.path(), &["tag", "v9", "HEAD~1"]);
    let remote_dir = TempDir::new().expect("remote repository directory");
    let remote = remote_dir.path().join("remote.git");
    let remote_arg = remote.to_str().unwrap();
    git_ok(
        repository.path(),
        &["clone", "--quiet", "--bare", ".", remote_arg],
    );
    git_ok(repository.path(), &["remote", "add", "origin", remote_arg]);
    git_ok(repository.path(), &["fetch", "--quiet", "origin"]);

    let mut tui = Tui::launch(repository.path());
    tui.wait_for("v9");
    tui.send(b"\x1b[B"); // move to the tagged commit
    tui.send(b"+");
    tui.wait_for("Select push mode");
    tui.send(b"\r");
    tui.wait_for("Push complete");
    assert_eq!(
        git_ok(&remote, &["rev-parse", "refs/tags/v9"]),
        git_ok(repository.path(), &["rev-parse", "HEAD~1"]),
        "the selected tag should reach the remote"
    );
    tui.quit();
}

#[test]
fn delete_popup_can_target_the_tag_and_leave_the_branch() {
    let repository = init_linear_repository();
    git_ok(repository.path(), &["tag", "v9"]);

    let mut tui = Tui::launch(repository.path());
    tui.wait_for("v9");
    tui.send(b"d");
    tui.wait_for("Delete branch main?");
    tui.send(b"\x1b[C"); // move the target from the branch to the tag
    tui.wait_for("Delete tag v9?");
    tui.send(b"\r");
    tui.wait_until_hidden("Delete tag v9?");
    assert!(
        !git(
            repository.path(),
            &["show-ref", "--verify", "--quiet", "refs/tags/v9"]
        )
        .status
        .success(),
        "the tag should be deleted"
    );
    assert!(
        git(
            repository.path(),
            &["show-ref", "--verify", "--quiet", "refs/heads/main"]
        )
        .status
        .success(),
        "the branch should be left untouched"
    );
    tui.quit();
}

#[test]
fn tab_reaches_the_read_only_commit_diff_without_staging_hints() {
    let repository = init_linear_repository();
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("tip commit");

    // Inspect a commit so its read-only diff and details appear.
    tui.send(b"\x1b[B");
    tui.wait_for("Commit Details");
    tui.wait_for("Diff — ");

    // Tab now cycles Graph -> Commit Details -> Diff.
    tui.send(b"\t");
    tui.wait_for_title_focus("Commit Details");
    tui.send(b"\t");
    tui.wait_for_title_focus("Diff");

    let screen = tui.contents();
    assert!(
        !screen.contains("Space stage") && !screen.contains("file/hunk/line"),
        "a read-only commit diff must not advertise staging keys:\n{screen}"
    );
    tui.quit();
}

#[test]
fn bracket_keys_jump_between_hunks_in_a_read_only_commit_diff() {
    let repository = init_read_only_multi_hunk_repository();
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("many hunks");

    // Move onto the tip commit so its read-only diff is shown.
    tui.send(b"\x1b[A");
    tui.wait_for("Commit Details");
    tui.wait_for("Diff — file.txt");
    tui.wait_for("line 10 changed");

    // Tab through the details pane to the diff and step through hunks with
    // `[`/`]`, even though there is no staging selection.
    tui.send(b"\t");
    tui.wait_for_title_focus("Commit Details");
    tui.send(b"\t");
    tui.wait_for_title_focus("Diff");
    tui.send(b"]");
    tui.wait_until_hidden("line 10 changed");
    tui.wait_for("line 20 changed");

    // `[` returns to the first hunk.
    tui.send(b"[");
    tui.wait_for("line 10 changed");
    tui.quit();
}

#[test]
fn arrow_keys_pan_the_diff_horizontally() {
    let repository = init_long_line_repository();
    let mut tui = Tui::launch(repository.path());
    tui.wait_for("long line");

    // Inspect the tip commit and focus its read-only diff.
    tui.send(b"\x1b[A");
    tui.wait_for("Diff — file.txt");
    tui.wait_for("ZZSTARTZZ");
    tui.send(b"\t");
    tui.wait_for_title_focus("Commit Details");
    tui.send(b"\t");
    tui.wait_for_title_focus("Diff");

    // Right pans the content left, pushing the start of the long line away.
    tui.send(b"\x1b[C");
    tui.wait_until_hidden("ZZSTARTZZ");
    tui.send(b"\x1b[D");
    tui.wait_for("ZZSTARTZZ");

    tui.quit();
}

// --- README screenshot generation -------------------------------------------
//
// Run with `cargo test --test tui_pty -- --ignored --nocapture
// generate_readme_screenshots` to rebuild every `assets/screenshot*.png` from
// the fixtures. Kept out of the normal suite because it writes into the repo. It
// rasterizes the terminal SVG with `rsvg-convert` (librsvg) and a Nerd Font, so
// both have to be installed; the rasterizer bakes in the real glyphs, which an
// SVG shown in a browser cannot.

/// Catppuccin Mocha ANSI palette used for the README screenshots, paired with
/// the `base` background (`TERM_BG`) so the images match a real terminal theme.
const TERM_PALETTE: [&str; 16] = [
    "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
    "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8",
];

/// Palette the PTY answers an OSC 4 query with for ordinary test runs.
const DEFAULT_PALETTE_REPLY: &[u8] = b"\x1b]4;0;rgb:0000/0000/0000\x1b\\\x1b]4;1;rgb:cdcd/0000/0000\x1b\\\x1b]4;2;rgb:0000/cdcd/0000\x1b\\\x1b]4;3;rgb:cdcd/cdcd/0000\x1b\\\x1b]4;4;rgb:0000/0000/cdcd\x1b\\\x1b]4;5;rgb:cdcd/0000/cdcd\x1b\\\x1b]4;6;rgb:0000/cdcd/cdcd\x1b\\\x1b]4;7;rgb:dddd/dddd/dddd\x1b\\\x1b]4;8;rgb:4444/4444/4444\x1b\\\x1b]4;9;rgb:ffff/5555/5555\x1b\\\x1b]4;10;rgb:5555/ffff/5555\x1b\\\x1b]4;11;rgb:ffff/ffff/5555\x1b\\\x1b]4;12;rgb:6666/9999/ffff\x1b\\\x1b]4;13;rgb:ffff/5555/ffff\x1b\\\x1b]4;14;rgb:5555/ffff/ffff\x1b\\\x1b]4;15;rgb:ffff/ffff/ffff\x1b\\";

/// Background the PTY answers an OSC 11 query with for ordinary test runs.
const DEFAULT_BACKGROUND_REPLY: &[u8] = b"\x1b]11;rgb:2222/2222/2222\x1b\\";

/// Build the OSC 4 reply describing `palette`, each channel doubled to the
/// four-hex-digit form the app's parser expects.
fn osc4_palette_reply(palette: &[&str; 16]) -> Vec<u8> {
    let mut reply = String::new();
    for (index, hex) in palette.iter().enumerate() {
        let (r, g, b) = (&hex[1..3], &hex[3..5], &hex[5..7]);
        reply.push_str(&format!("\x1b]4;{index};rgb:{r}{r}/{g}{g}/{b}{b}\x1b\\"));
    }
    reply.into_bytes()
}

/// Build the OSC 11 background reply for a `#rrggbb` color.
fn osc11_background_reply(hex: &str) -> Vec<u8> {
    let (r, g, b) = (&hex[1..3], &hex[3..5], &hex[5..7]);
    format!("\x1b]11;rgb:{r}{r}/{g}{g}/{b}{b}\x1b\\").into_bytes()
}
const TERM_BG: &str = "#1e1e2e";
const TERM_FG: &str = "#cdd6f4";

fn svg_color(color: vt100::Color, default: &str) -> String {
    match color {
        vt100::Color::Default => default.to_string(),
        vt100::Color::Idx(index) => TERM_PALETTE
            .get(index as usize)
            .copied()
            .unwrap_or(default)
            .to_string(),
        vt100::Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

fn escape_svg(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn terminal_svg(parser: &vt100::Parser, cols: u16, rows: u16) -> String {
    let screen = parser.screen();
    let (cell_w, cell_h) = (8.4_f32, 18.0_f32);
    let (pad, bar) = (12.0_f32, 30.0_f32);
    let width = f32::from(cols) * cell_w;
    let height = f32::from(rows) * cell_h;
    let total_w = width + pad * 2.0;
    let total_h = height + pad * 2.0 + bar;
    let (ox, oy) = (pad, pad + bar);

    let mut out = String::new();
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{total_w:.0}\" height=\"{total_h:.0}\" \
         viewBox=\"0 0 {total_w:.0} {total_h:.0}\" xml:space=\"preserve\" \
         font-family=\"'MesloLGS Nerd Font Mono', 'MesloLGM Nerd Font Mono', 'JetBrainsMono Nerd Font Mono', monospace\" \
         font-size=\"14\">"
    ));
    out.push_str(&format!(
        "<rect width=\"{total_w:.0}\" height=\"{total_h:.0}\" rx=\"10\" fill=\"#11111b\"/>"
    ));
    for (index, color) in ["#ff5f56", "#ffbd2e", "#27c93f"].iter().enumerate() {
        out.push_str(&format!(
            "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"5.5\" fill=\"{color}\"/>",
            pad + 10.0 + index as f32 * 18.0,
            pad + bar / 2.0
        ));
    }
    out.push_str(&format!(
        "<rect x=\"{ox:.1}\" y=\"{oy:.1}\" width=\"{width:.1}\" height=\"{height:.1}\" fill=\"{TERM_BG}\"/>"
    ));

    for row in 0..rows {
        let mut col = 0u16;
        while col < cols {
            let bg = screen.cell(row, col).map_or_else(
                || TERM_BG.to_string(),
                |cell| svg_color(cell.bgcolor(), TERM_BG),
            );
            let mut end = col + 1;
            while end < cols {
                let next = screen.cell(row, end).map_or_else(
                    || TERM_BG.to_string(),
                    |cell| svg_color(cell.bgcolor(), TERM_BG),
                );
                if next != bg {
                    break;
                }
                end += 1;
            }
            if bg != TERM_BG {
                out.push_str(&format!(
                    "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{cell_h:.1}\" fill=\"{bg}\"/>",
                    ox + f32::from(col) * cell_w,
                    oy + f32::from(row) * cell_h,
                    f32::from(end - col) * cell_w,
                ));
            }
            col = end;
        }
    }

    for row in 0..rows {
        let baseline = oy + f32::from(row) * cell_h + cell_h * 0.76;
        for col in 0..cols {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            let Some(glyph) = cell.contents().chars().next() else {
                continue;
            };
            if glyph == ' ' {
                continue;
            }
            out.push_str(&format!(
                "<text x=\"{:.1}\" y=\"{baseline:.1}\" fill=\"{}\" textLength=\"{cell_w:.1}\" lengthAdjust=\"spacingAndGlyphs\"",
                ox + f32::from(col) * cell_w,
                svg_color(cell.fgcolor(), TERM_FG),
            ));
            if cell.bold() {
                out.push_str(" font-weight=\"bold\"");
            }
            if cell.dim() {
                out.push_str(" opacity=\"0.55\"");
            }
            out.push_str(&format!(">{}</text>", escape_svg(&glyph.to_string())));
        }
    }

    out.push_str("</svg>\n");
    out
}

/// Rasterize the terminal SVG to a 2x PNG at `path` with `rsvg-convert`. The
/// rasterizer draws the real Nerd Font glyphs and terminal colors, which is why
/// the README ships PNG: a browser can't supply the font an SVG would need.
fn write_screenshot_png(svg: &str, path: &str) {
    let temp = std::env::temp_dir().join("figit-readme-screenshot.svg");
    std::fs::write(&temp, svg).expect("write temporary SVG");
    let status = Command::new("rsvg-convert")
        .arg("--zoom=2")
        .arg(&temp)
        .arg("-o")
        .arg(path)
        .status()
        .expect("run rsvg-convert (install librsvg / librsvg2-bin)");
    assert!(status.success(), "rsvg-convert failed to write {path}");
    let _ = std::fs::remove_file(&temp);
}

/// Launch figit in a PTY with the README's Catppuccin theme.
fn launch_themed(repository: &Path) -> Tui {
    let background = osc11_background_reply(TERM_BG);
    let palette = osc4_palette_reply(&TERM_PALETTE);
    Tui::launch_with_theme(repository, None, &[], 120, 35, &background, &palette)
}

/// Rasterize the current screen to `path`.
fn capture_render(tui: &Tui, path: &str) {
    let screen = tui.screen.lock().expect("screen lock");
    write_screenshot_png(&terminal_svg(&screen, 120, 35), path);
}

#[test]
#[ignore = "regenerates the README screenshots; run with --ignored --nocapture"]
fn generate_readme_screenshots() {
    // Staging + diff, then a commit's details and syntax-highlighted diff.
    let dir = TempDir::new().expect("temp dir");
    let dest = dir.path().join("repo");
    assert!(Command::new("cp")
        .args(["-R", "test_repo", dest.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let mut tui = launch_themed(&dest);
    capture_render(&tui, "assets/screenshot.png");
    tui.send(b"\t");
    tui.send(b"\x1b[B\x1b[B\x1b[B\x1b[B");
    tui.wait_for("Commit Details");
    thread::sleep(Duration::from_millis(600));
    capture_render(&tui, "assets/screenshot-commit.png");
    drop(tui);

    // Interactive rebase plan.
    let repository = init_linear_repository();
    let mut tui = launch_themed(repository.path());
    tui.wait_for("tip commit");
    // Let the selected commit's details finish loading before opening the
    // overlay: once it is open the placeholder never clears.
    thread::sleep(Duration::from_millis(700));
    tui.send(b"\x1b[B");
    thread::sleep(Duration::from_millis(700));
    tui.send(b"i");
    tui.wait_for("Interactive rebase");
    tui.wait_for("target commit");
    thread::sleep(Duration::from_millis(300));
    capture_render(&tui, "assets/screenshot-rebase.png");
    drop(tui);

    // Three-way merge conflict resolver.
    let repository = init_conflict_repository();
    let tui = launch_themed(repository.path());
    tui.wait_for("Three-way merge — file.txt");
    tui.wait_for("Base → Ours");
    capture_render(&tui, "assets/screenshot-conflict.png");
    drop(tui);

    // Stash manager with one saved stash.
    let repository = init_stash_repository();
    git_ok(
        repository.path(),
        &["stash", "push", "-u", "-m", "wip before experiment"],
    );
    let mut tui = launch_themed(repository.path());
    tui.send(b"s");
    tui.wait_for("Stashes (1)");
    capture_render(&tui, "assets/screenshot-stash.png");
    drop(tui);
}

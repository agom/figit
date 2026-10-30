//! Terminal setup and restoration, frame scheduling, and OSC color queries.

use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::{event, execute};
use ratatui::style::Color;

use super::{App, GitPath};

pub(super) type TerminalAnsiPalette = [Option<Color>; 16];

/// Ratatui's initializer installs a hook to restore raw mode and the alternate
/// screen. Chain one that also releases the mouse-capture mode enabled below.
fn install_mouse_capture_panic_hook() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        previous_hook(panic_info);
    }));
}

impl App {
    pub fn run(&mut self) -> Result<()> {
        crate::profile::mark("tui start");
        self.no_color = std::env::var("NO_COLOR").is_ok_and(|value| !value.is_empty());
        (self.bg, self.terminal_palette) = detect_terminal_colors();
        crate::profile::mark("terminal colours");
        let mut terminal = ratatui::init();
        install_mouse_capture_panic_hook();
        let mouse = execute!(std::io::stdout(), EnableMouseCapture);
        crate::profile::mark("terminal ready");
        let res = (|| -> Result<()> {
            mouse?;
            // Paint before any filesystem watcher registration. On Linux a
            // recursive watch visits every directory and can take seconds.
            terminal.draw(|frame| self.draw(frame))?;
            crate::profile::mark("first frame");
            self.start_update_check(false);
            self.update_check_deadline =
                Some(Instant::now() + super::update::UPDATE_CHECK_INTERVAL);
            self.start_repository_watcher();
            if self.loading {
                self.refresh.deadline = Some(Instant::now());
            }
            self.event_loop(&mut terminal)
        })();
        self.watch_setup = None;
        self.repository_watcher = None;
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        ratatui::restore();
        res
    }

    pub(super) fn redraw_interval(&self) -> Option<Duration> {
        if self.redraw {
            Some(Duration::from_millis(16))
        } else if self.loading || self.git_busy() {
            Some(Duration::from_millis(100))
        } else {
            None
        }
    }

    fn event_loop(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        self.redraw = false;
        let mut last_frame = Instant::now();
        loop {
            self.poll_repository_watcher();
            self.poll_visible_file_metadata();
            self.poll_remote_operation();
            self.poll_local_job();
            if self
                .update_check_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                self.start_update_check(false);
                self.update_check_deadline =
                    Some(Instant::now() + super::update::UPDATE_CHECK_INTERVAL);
            }
            self.poll_update();
            self.poll_repository_refresh();
            self.poll_graph_page();
            self.poll_graph_details();
            self.poll_diff_stats();
            if self
                .redraw_interval()
                .is_some_and(|interval| last_frame.elapsed() >= interval)
            {
                terminal.draw(|f| self.draw(f))?;
                last_frame = Instant::now();
                self.redraw = false;
            }
            // Background completions are observed within 50 ms even while idle.
            let wait = self
                .redraw_interval()
                .map_or(Duration::from_millis(50), |interval| {
                    interval
                        .saturating_sub(last_frame.elapsed())
                        .min(Duration::from_millis(50))
                });
            if event::poll(wait)? {
                let mut wheel: Option<(u16, u16, i32)> = None;
                if self.collect_event(event::read()?, &mut wheel) {
                    return Ok(());
                }
                // Bound event coalescing so a trackpad burst cannot starve
                // rendering, while avoiding a full redraw for every wheel tick.
                for _ in 0..31 {
                    if !event::poll(Duration::from_millis(0))? {
                        break;
                    }
                    if self.collect_event(event::read()?, &mut wheel) {
                        return Ok(());
                    }
                    if self.pending_conflict_editor.is_some() {
                        break;
                    }
                }
                if let Some(path) = self.pending_conflict_editor.take() {
                    if let Err(error) = self.run_conflict_editor(terminal, &path) {
                        self.show_rebase_error(error.to_string());
                    }
                }
                if let Some((x, y, delta)) = wheel {
                    self.handle_mouse_scroll(x, y, delta.clamp(-8, 8));
                }
            }
        }
    }
}

/// Query the terminal background and ANSI palette. The palette lets dimmed
/// graph colors retain the user's theme hue while using a darker RGB shade.
#[cfg(unix)]
fn detect_terminal_colors() -> (Option<Color>, TerminalAnsiPalette) {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

    if enable_raw_mode().is_err() {
        return (None, [None; 16]);
    }
    let result = (|| {
        let mut out = std::io::stdout();
        out.write_all(b"\x1b]11;?\x1b\\").ok()?;
        out.flush().ok()?;

        let fd = std::io::stdin().as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Some((None, [None; 16]));
        }
        // Non-blocking reads so a non-answering terminal cannot hang the app.
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        let mut acc = Vec::new();
        let mut buf = [0u8; 256];
        // Terminals may answer an OSC query with ST (`ESC \\`) or BEL. Cap the
        // wait low: a non-responding terminal must not delay startup.
        let deadline = Instant::now() + Duration::from_millis(150);
        loop {
            if Instant::now() >= deadline {
                break;
            }
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                break;
            }
            if n == 0 {
                break;
            }
            acc.extend_from_slice(&buf[..n as usize]);
            if acc.windows(2).any(|w| w == b"\x1b\\") || acc.contains(&0x07) {
                break;
            }
        }
        let background = parse_osc11(&acc);

        let palette_query =
            b"\x1b]4;0;?;1;?;2;?;3;?;4;?;5;?;6;?;7;?;8;?;9;?;10;?;11;?;12;?;13;?;14;?;15;?\x1b\\";
        let palette_start = acc.len();
        if out.write_all(palette_query).is_err() || out.flush().is_err() {
            unsafe {
                libc::fcntl(fd, libc::F_SETFL, flags);
            }
            return Some((background, [None; 16]));
        }
        let palette_deadline = Instant::now() + Duration::from_millis(60);
        while Instant::now() < palette_deadline {
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                break;
            }
            if n == 0 {
                break;
            }
            acc.extend_from_slice(&buf[..n as usize]);
            let palette = parse_osc4(&acc[palette_start..]);
            if (0..=15).all(|index| palette[index].is_some()) {
                break;
            }
        }
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, flags);
        }
        Some((background, parse_osc4(&acc[palette_start..])))
    })();
    let _ = disable_raw_mode();
    result.unwrap_or((None, [None; 16]))
}

#[cfg(not(unix))]
fn detect_terminal_colors() -> (Option<Color>, TerminalAnsiPalette) {
    (None, [None; 16])
}

/// Parse an OSC 11 response, e.g. `\x1b]11;rgb:0a0a/0b0b/0c0c\x1b\\`,
/// `rgba:...`, or `#RRGGBB`. Tolerates 16-bit and 8-bit channel widths.
pub fn parse_osc11(acc: &[u8]) -> Option<Color> {
    let s = String::from_utf8_lossy(acc);
    const PREFIX: &str = "\x1b]11;";
    let start = s.find(PREFIX)? + PREFIX.len();
    let rest = &s[start..];
    let end = rest
        .find("\x1b\\")
        .or_else(|| rest.find('\x07'))
        .unwrap_or(rest.len());
    let payload = rest[..end].trim();

    parse_rgb_color(payload)
}

/// Parse the responses to an OSC 4 ANSI palette query.
pub fn parse_osc4(acc: &[u8]) -> TerminalAnsiPalette {
    let mut palette = [None; 16];
    let text = String::from_utf8_lossy(acc);
    for response in text.split("\x1b]4;").skip(1) {
        let end = response
            .find("\x1b\\")
            .or_else(|| response.find('\x07'))
            .unwrap_or(response.len());
        let Some((index, value)) = response[..end].split_once(';') else {
            continue;
        };
        let Ok(index) = index.parse::<usize>() else {
            continue;
        };
        if index < palette.len() {
            palette[index] = parse_rgb_color(value.trim());
        }
    }
    palette
}

fn parse_rgb_color(payload: &str) -> Option<Color> {
    let channel = |hex: &str| {
        // 16-bit "0a0a" or 8-bit "0a" -> use the most significant byte.
        let hex = hex.trim().strip_prefix("0x").unwrap_or(hex.trim());
        let take = hex.len().clamp(1, 2);
        u8::from_str_radix(&hex[..take], 16).ok()
    };
    if let Some(hex) = payload.strip_prefix('#') {
        return Some(Color::Rgb(
            channel(hex.get(0..2)?)?,
            channel(hex.get(2..4)?)?,
            channel(hex.get(4..6)?)?,
        ));
    }
    let rgb = payload
        .strip_prefix("rgb:")
        .or_else(|| payload.strip_prefix("rgba:"))?;
    let mut channels = rgb.split('/');
    Some(Color::Rgb(
        channel(channels.next()?)?,
        channel(channels.next()?)?,
        channel(channels.next()?)?,
    ))
}

impl App {
    pub(super) fn run_conflict_editor(
        &mut self,
        terminal: &mut ratatui::DefaultTerminal,
        path: &GitPath,
    ) -> Result<()> {
        self.cancel_conflict_preview();
        terminal.hide_cursor()?;
        crossterm::terminal::disable_raw_mode()?;
        execute!(
            std::io::stdout(),
            DisableMouseCapture,
            crossterm::terminal::LeaveAlternateScreen
        )?;

        let edit_result = self
            .backend
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("repository backend is unavailable"))?
            .edit_conflict_file(path);

        let restore_result = (|| -> Result<()> {
            execute!(
                std::io::stdout(),
                crossterm::terminal::EnterAlternateScreen,
                EnableMouseCapture
            )?;
            crossterm::terminal::enable_raw_mode()?;
            terminal.clear()?;
            Ok(())
        })();
        restore_result?;
        edit_result?;

        self.refresh_diff();
        if let Some(view) = self
            .three_way_view
            .as_mut()
            .filter(|view| view.path == *path)
        {
            view.result_preview = true;
            view.chosen_side = None;
        }
        Ok(())
    }
}

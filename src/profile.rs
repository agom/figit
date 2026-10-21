//! Opt-in startup profiling.
//!
//! Set `FIGIT_PROFILE_STARTUP=1` to collect timing marks from the first line of
//! `main` through the first repository snapshot. The report is printed after the
//! TUI restores the terminal; writing to stderr while the alternate screen is
//! active would corrupt the UI.

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

struct Mark {
    at: Duration,
    label: &'static str,
}

static START: OnceLock<Instant> = OnceLock::new();
static ENABLED: OnceLock<bool> = OnceLock::new();
static MARKS: OnceLock<Mutex<Vec<Mark>>> = OnceLock::new();

pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var_os("FIGIT_PROFILE_STARTUP").is_some())
}

fn start_instant() -> Instant {
    *START.get_or_init(Instant::now)
}

fn marks() -> MutexGuard<'static, Vec<Mark>> {
    MARKS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Start the clock. Call this as early as possible in `main`.
pub fn start() {
    if !enabled() {
        return;
    }
    let _ = start_instant();
    mark("process start");
}

/// Record a phase boundary. No-op unless profiling is enabled.
pub fn mark(label: &'static str) {
    if !enabled() {
        return;
    }
    let at = start_instant().elapsed();
    marks().push(Mark { at, label });
}

/// Format the collected marks, oldest first, with cumulative and delta times.
pub fn report() -> String {
    if !enabled() {
        return String::new();
    }
    let mut marks = marks();
    if marks.is_empty() {
        return String::new();
    }
    marks.sort_by_key(|mark| mark.at);
    let total = marks.last().map(|mark| mark.at).unwrap_or_default();
    let mut report = format!(
        "startup profile (total {:.3}s)\n  elapsed  phase                         delta\n",
        total.as_secs_f64()
    );
    let mut previous = Duration::ZERO;
    for mark in marks.iter() {
        let delta = mark.at.saturating_sub(previous);
        report.push_str(&format!(
            "  {:>7.3}s  {:<28} +{:.3}s\n",
            mark.at.as_secs_f64(),
            mark.label,
            delta.as_secs_f64()
        ));
        previous = mark.at;
    }
    report
}

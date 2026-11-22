//! Logging to stderr that can never hurt the match.
//!
//! * A write error (a closed or full pipe) is ignored: `eprintln!` panics on it.
//! * Ordinary messages share a byte budget per process, so a pathological match (a
//!   parse failure or a slow decision on every tick) cannot fill a stderr pipe that
//!   nobody drains and then block the thread that decides.
//! * `MECH_VERBOSE` output is asked for explicitly and is not capped.
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Bytes of ordinary log output per process (a normal match writes under 10 KB).
pub const BUDGET: usize = 48 * 1024;

static USED: AtomicUsize = AtomicUsize::new(0);
static CLOSED: AtomicBool = AtomicBool::new(false);

/// Bytes of capped log output written so far.
pub fn used() -> usize {
    USED.load(Ordering::Relaxed)
}

/// What to do with a capped message of `len` bytes when `before` bytes were already used.
#[derive(Debug, PartialEq, Eq)]
enum Admit {
    Write,
    /// The message that crosses the budget is replaced by one short notice.
    Notice,
    Drop,
}
fn admit(before: usize, len: usize) -> Admit {
    if before >= BUDGET {
        Admit::Drop
    } else if before + len >= BUDGET {
        Admit::Notice
    } else {
        Admit::Write
    }
}

#[doc(hidden)]
pub fn write(args: std::fmt::Arguments, capped: bool) {
    if CLOSED.load(Ordering::Relaxed) {
        return;
    }
    let line = format!("{args}\n");
    if capped {
        let before = USED.fetch_add(line.len(), Ordering::Relaxed);
        match admit(before, line.len()) {
            Admit::Write => {}
            Admit::Notice => {
                let _ = std::io::stderr().write_all(b"[bot] log budget used up; further messages are dropped\n");
                return;
            }
            Admit::Drop => return,
        }
    }
    if std::io::stderr().write_all(line.as_bytes()).is_err() {
        CLOSED.store(true, Ordering::Relaxed);
    }
}

/// Like `eprintln!`, capped and never panicking.
#[macro_export]
macro_rules! logln {
    ($($arg:tt)*) => { $crate::log::write(format_args!($($arg)*), true) };
}

/// Verbose diagnostics (`MECH_VERBOSE`): never panicking, not capped.
#[macro_export]
macro_rules! vlogln {
    ($($arg:tt)*) => { $crate::log::write(format_args!($($arg)*), false) };
}

/// Replace the default panic hook (which prints through `eprintln!` machinery, uncapped)
/// with one line through the capped logger.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let msg = info.payload().downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| info.payload().downcast_ref::<String>().cloned()).unwrap_or_default();
        write(format_args!("[bot] panic at {at}: {msg}"), true);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The budget admits a normal match's output, replaces the message that crosses it
    /// by one notice, and drops everything after.
    #[test]
    fn budget_caps_the_output() {
        assert_eq!(admit(0, 120), Admit::Write);
        assert_eq!(admit(BUDGET - 200, 120), Admit::Write);
        assert_eq!(admit(BUDGET - 100, 120), Admit::Notice);
        assert_eq!(admit(BUDGET, 1), Admit::Drop);
        assert_eq!(admit(BUDGET * 3, 120), Admit::Drop);
        // a per-tick message for a whole match (4800 decisions) stays within the budget
        let mut used = 0;
        let mut written = 0;
        for _ in 0..4800 {
            if admit(used, 60) == Admit::Write {
                written += 60;
            }
            used += 60;
        }
        assert!(written < BUDGET);
    }
}

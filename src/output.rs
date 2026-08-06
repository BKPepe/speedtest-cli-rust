//! Clean stdout/stderr separation for program output.
//!
//! Design:
//!   - stdout: machine-readable data for piping (JSON, CSV, --simple, --list, --version)
//!   - stderr: human-readable UI (progress, errors, debugging)
//!   - quiet mode: suppresses informational UI output (for --csv, --json, --simple)
//!   - debug mode: enables verbose debug output (for --debug)

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static DEBUG: AtomicBool = AtomicBool::new(false);
static QUIET: AtomicBool = AtomicBool::new(false);

/// Enables or disables debug output. Called when --debug is set.
pub fn set_debug(v: bool) {
    DEBUG.store(v, Ordering::Relaxed);
}

/// Enables or disables quiet mode (suppresses `write_ui!`).
/// Called when --csv, --json, or --simple is set.
pub fn set_quiet(v: bool) {
    QUIET.store(v, Ordering::Relaxed);
}

/// Reports whether debug output is enabled.
pub fn is_debug() -> bool {
    DEBUG.load(Ordering::Relaxed)
}

/// Reports whether informational UI output is suppressed.
pub fn is_quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

/// Reports whether the UI stream (stderr) is an interactive terminal.
/// Used to decide whether the spinner may animate.
pub fn ui_is_terminal() -> bool {
    std::io::stderr().is_terminal()
}

#[doc(hidden)]
pub fn _out(args: std::fmt::Arguments<'_>) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.write_fmt(args);
    let _ = lock.flush();
}

#[doc(hidden)]
pub fn _ui(args: std::fmt::Arguments<'_>) {
    if is_quiet() {
        return;
    }
    _err(args);
}

#[doc(hidden)]
pub fn _err(args: std::fmt::Arguments<'_>) {
    let stderr = std::io::stderr();
    let mut lock = stderr.lock();
    let _ = lock.write_fmt(args);
    let _ = lock.flush();
}

#[doc(hidden)]
pub fn _dbg(args: std::fmt::Arguments<'_>) {
    if !is_debug() {
        return;
    }
    _err(args);
}

/// Writes formatted data to stdout.
/// Used for JSON, CSV, --simple results, --list, and --version.
/// Does NOT append a newline; the caller controls formatting.
#[macro_export]
macro_rules! write_out {
    ($($arg:tt)*) => { $crate::output::_out(format_args!($($arg)*)) };
}

/// Writes informational messages to stderr.
/// Suppressed in quiet mode (--csv, --json, --simple).
#[macro_export]
macro_rules! write_ui {
    ($($arg:tt)*) => { $crate::output::_ui(format_args!($($arg)*)) };
}

/// Writes debug messages to stderr. Only shown when --debug is set.
#[macro_export]
macro_rules! write_debug {
    ($($arg:tt)*) => { $crate::output::_dbg(format_args!($($arg)*)) };
}

/// Writes error messages to stderr. Always shown regardless of mode.
#[macro_export]
macro_rules! write_error {
    ($($arg:tt)*) => { $crate::output::_err(format_args!($($arg)*)) };
}

/// Writes a blank line to stderr.
///
/// Unlike `write_ui!`, this is NOT suppressed in quiet mode, because
/// multi-server results and --list mode need the spacing.
pub fn write_ui_blank() {
    _err(format_args!("\n"));
}

/// Writes a message to stderr with a trailing newline and exits with code 1.
pub fn fatal(msg: impl std::fmt::Display) -> ! {
    _err(format_args!("{msg}\n"));
    std::process::exit(1)
}

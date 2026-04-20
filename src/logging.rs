//! Shared logging infrastructure with module-based file routing.
//!
//! All Bifrost binaries use the same log format:
//!
//! ```text
//! 2026-04-17 21:00:00 [INFO] bifrost::frame::backup_job - message
//! ```
//!
//! # Routing
//!
//! The `RoutingLogger` directs log records to specific files based on the
//! **module path** (`record.target()`).  Routes are configured by calling
//! [`add_route`].  More-specific prefixes take priority over less-specific
//! ones (e.g. `bifrost::nfs::scanner` beats `bifrost::nfs`).
//!
//! | Record target         | Matching route (example) | Destination      |
//! |-----------------------|--------------------------|------------------|
//! | `bifrost::scanner::*` | `bifrost::scanner`       | `scan.log`       |
//! | `bifrost::frame::*`   | `bifrost::frame`         | `frame.log`      |
//! | `bifrost::backup::*`  | `bifrost::backup`        | `subtask_{u}.log`|
//! | *(no match)*          | —                        | **stdout**       |
//!
//! Unmatched records (e.g. from the CLI binary itself) go to stdout only.
//!
//! The `--log-file` option adds a **catch-all** file that receives every
//! record regardless of routing.
//!
//! # API
//!
//! ```rust,ignore
//! // 1. One-time init (registers the global logger, sets max level)
//! bifrost::logging::init(verbose);
//!
//! // 2. Add a catch-all file (--log-file)
//! bifrost::logging::add_file(&path);
//!
//! // 3. Add module routes (after dirs exist)
//! bifrost::logging::add_route("bifrost::scanner", scan_log_path);
//! bifrost::logging::add_route("bifrost::frame",   frame_log_path);
//! bifrost::logging::add_route("bifrost::backup",  subtask_log_path);
//! ```

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

// ---------------------------------------------------------------------------
// Shared global state
// ---------------------------------------------------------------------------

/// A single routing rule: records whose target starts with `prefix` go to `file`.
struct LogRoute {
    prefix: String,
    file:   Mutex<std::fs::File>,
}

/// Global logger state shared between `init()` / `add_route()` / `add_file()`
/// and the `RoutingLogger` instance registered with the `log` crate.
struct LoggerState {
    level:  log::LevelFilter,
    routes: Vec<LogRoute>,
    /// Catch-all files that receive every record (--log-file).
    extra:  Vec<Mutex<std::fs::File>>,
}

static STATE: LazyLock<Arc<Mutex<LoggerState>>> =
    LazyLock::new(|| Arc::new(Mutex::new(LoggerState {
        level:  log::LevelFilter::Info,
        routes: Vec::new(),
        extra:  Vec::new(),
    })));

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Initialise global logging (stdout-only, no file routing).
///
/// * `verbose` — 0 = INFO, 1 = DEBUG, >=2 = TRACE.
///
/// This must be called at least once.  Subsequent calls only update the
/// max level; the global logger is registered on the first call only.
pub fn init(verbose: u8) {
    let level = match verbose {
        0 => log::LevelFilter::Info,
        1 => log::LevelFilter::Debug,
        _ => log::LevelFilter::Trace,
    };

    {
        let mut st = STATE.lock().unwrap();
        st.level = level;
    }

    let logger = Box::new(RoutingLogger {
        state: Arc::clone(&STATE),
    });
    let _ = log::set_boxed_logger(logger);
    log::set_max_level(level);
}

/// Add a catch-all file that receives **every** log record (append mode).
///
/// Typically used for `--log-file`.
pub fn add_file(path: &Path) {
    if let Ok(f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
    {
        STATE.lock().unwrap().extra.push(Mutex::new(f));
    }
}

/// Add a module->file route.
///
/// Records whose `target()` starts with `prefix` will be written to `path`
/// (append mode) instead of stdout.  If multiple routes match, the
/// **longest** (most specific) prefix wins.
pub fn add_route(prefix: &str, path: &Path) {
    if let Ok(f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
    {
        let mut st = STATE.lock().unwrap();
        st.routes.push(LogRoute {
            prefix: prefix.to_string(),
            file:   Mutex::new(f),
        });
        // Keep routes sorted longest-prefix-first for fast lookup.
        st.routes.sort_by(|a, b| b.prefix.len().cmp(&a.prefix.len()));
    }
}

/// Remove all routes whose prefix starts with the given string.
///
/// Used to clear the `bifrost::backup` route between subtasks so each
/// subtask gets its own log file.
pub fn remove_route(prefix: &str) {
    let mut st = STATE.lock().unwrap();
    st.routes.retain(|r| !r.prefix.starts_with(prefix));
}

// ---------------------------------------------------------------------------
// RoutingLogger — the `log::Log` implementation
// ---------------------------------------------------------------------------

struct RoutingLogger {
    state: Arc<Mutex<LoggerState>>,
}

impl log::Log for RoutingLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        let st = self.state.lock().unwrap();
        metadata.level() <= st.level
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        if should_suppress_record(record) {
            return;
        }

        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let line = format!(
            "{} [{}] {} - {}",
            ts,
            record.level(),
            record.target(),
            record.args()
        );

        let st = self.state.lock().unwrap();
        let target = record.target();

        // Find the most specific (longest) matching route.
        let mut matched: Option<&Mutex<std::fs::File>> = None;
        for route in &st.routes {
            if target.starts_with(&route.prefix) {
                matched = Some(&route.file);
                break; // routes are sorted longest-prefix-first
            }
        }

        if let Some(file_mtx) = matched {
            // Route match -> write to the route file only (not stdout).
            if let Ok(mut file) = file_mtx.lock() {
                let _ = writeln!(file, "{line}");
                let _ = file.flush();
            }
        } else {
            // No route -> write to stdout.
            let _ = writeln!(std::io::stdout(), "{line}");
        }

        // Always write to catch-all extra files (--log-file).
        for f_mtx in &st.extra {
            if let Ok(mut f) = f_mtx.lock() {
                let _ = writeln!(f, "{line}");
                let _ = f.flush();
            }
        }
    }

    fn flush(&self) {
        let _ = std::io::stdout().flush();
        let st = self.state.lock().unwrap();
        for route in &st.routes {
            if let Ok(mut f) = route.file.lock() {
                let _ = f.flush();
            }
        }
        for f_mtx in &st.extra {
            if let Ok(mut f) = f_mtx.lock() {
                let _ = f.flush();
            }
        }
    }
}

fn should_suppress_record(record: &log::Record) -> bool {
    if record.target() != "smb::resource" {
        return false;
    }

    let msg = record.args().to_string();
    msg.starts_with("Error closing file:")
        && (msg.contains("Unexpected Message, Received message for different tree, or tree disconnecting.")
            || msg.contains("Network Name Deleted (0xc00000c9)"))
}

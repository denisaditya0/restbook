//! Where log lines go when nobody is looking at a terminal. A GUI binary
//! launched from Explorer, a shortcut, or an editor task has no stderr, and
//! GPUI installs no sink, so `main.rs` installs one here: a tee where
//! `env_logger` formats and the same bytes go to stderr (when attached) and to
//! `<data dir>/restbook.log` (`RESTBOOK_LOG_FILE`; `RUST_LOG` filters). `init`
//! also installs a panic hook, since a panic never reaches a log line.
//!
//! The normal file is deliberately quiet: facts, failures, one send outcome.
//! The play-by-play (every click and command, full URLs, bodies) goes to a
//! separate capture file that only exists while the user has asked for one.
//! A capture is never written into `restbook.log`, so expiring it cannot erase
//! the ordinary record.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The name of the log in the data directory.
pub(crate) const LOG_FILE: &str = "restbook.log";

/// The prefix of an opt-in capture file. The timestamp follows, then `.log`.
const DEBUG_PREFIX: &str = "restbook-debug-";

/// The filter used when `RUST_LOG` is unset. `scripts/dev.cmd --logs` sets the
/// same value, so both ways of running the app show the same lines.
pub(crate) const DEFAULT_FILTER: &str =
    "warn,restbook_app=info,restbook_core=info,restbook_store=info,restbook_transport=info";

/// The filter of the capture logger. It is always `debug`-capable; the sink
/// discards its records unless a window is open, so no runtime reconfiguration
/// is needed to start or stop a capture.
const DEBUG_FILTER: &str =
    "warn,restbook_app=debug,restbook_core=debug,restbook_store=debug,restbook_transport=debug";

/// A capture records for this long. The UI names the same number, so the
/// countdown and the logger cannot disagree.
const DEBUG_WINDOW: Duration = Duration::from_secs(10 * 60);

/// The file `init` opened, if any. Resolved once so chrome can name the exact
/// log being written, including a `RESTBOOK_LOG_FILE` override.
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Set once the window has written its last state. From then on target-less
/// `ERROR` records are gpui-pre's teardown, not app activity, and [`Closing`]
/// drops them.
static CLOSING: AtomicBool = AtomicBool::new(false);

/// `true` when the caller supplied `RUST_LOG`. An explicit filter is the
/// developer's switch and outranks the UI capture window in both directions.
static ENV_FILTER: AtomicBool = AtomicBool::new(false);

/// When the current capture stops, in epoch milliseconds. `0` means no capture.
static DEBUG_UNTIL: AtomicU64 = AtomicU64::new(0);

/// The open capture, if any. A capture is one file, so starting another
/// replaces it rather than appending.
static DEBUG: Mutex<DebugCapture> = Mutex::new(DebugCapture {
    file: None,
    path: None,
    bytes: 0,
});

struct DebugCapture {
    file: Option<File>,
    path: Option<PathBuf>,
    bytes: u64,
}

/// Tell the sink that the close path has finished, so the platform teardown
/// that follows is not reported as an app failure.
pub(crate) fn mark_closing() {
    CLOSING.store(true, Ordering::Relaxed);
}

/// Truncate the file at startup once it passes this. The normal file carries
/// one line per send and the failures between them, so a megabyte is months of
/// use; the cap is a backstop, not a budget.
const MAX_BYTES: u64 = 1024 * 1024;

/// A ten-minute capture cannot fill this in normal use, but a runaway loop
/// should not fill the disk either.
const MAX_DEBUG_BYTES: u64 = 8 * 1024 * 1024;

/// Install the logger. Returns the file's path when it was opened, `None` when
/// only stderr is available (a read-only data directory, say).
pub(crate) fn init() -> Option<PathBuf> {
    let path = std::env::var_os("RESTBOOK_LOG_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::workspace::storage::data_dir().join(LOG_FILE));

    ENV_FILTER.store(std::env::var_os("RUST_LOG").is_some(), Ordering::Relaxed);

    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(DEFAULT_FILTER));
    // `2026-11-17T09:12:03Z INFO  restbook_app::workspace::storage: …` — a
    // format that is read in a file, not skimmed in a terminal.
    builder.format_timestamp_secs();

    let opened = open(&path);
    let file = match opened {
        Some(file) => {
            let _ = LOG_PATH.set(path.clone());
            builder.target(env_logger::Target::Pipe(Box::new(Tee { file })));
            Some(path)
        }
        None => None,
    };
    let logger = builder.build();

    // The capture logger formats the same lines but writes only to the capture
    // file. It is built once, with its filter fixed at `debug`; whether it
    // writes at all is decided per record by [`DebugCapture::file`].
    let mut capture_builder = env_logger::Builder::new();
    capture_builder.format_timestamp_secs();
    capture_builder.parse_filters(DEBUG_FILTER);
    capture_builder.target(env_logger::Target::Pipe(Box::new(DebugSink)));
    let capture_logger = capture_builder.build();

    log::set_max_level(std::cmp::max(logger.filter(), capture_logger.filter()));
    // Equivalent to `builder.init()`, but through [`Closing`]. Failing to set a
    // second logger is not fatal (a test may own it), which `init` also ignored.
    let _ = log::set_boxed_logger(Box::new(Closing {
        normal: logger,
        capture: capture_logger,
    }));
    // A capture from a previous run is a report that was never sent. Keeping it
    // would surprise the user and contradict the ten-minute promise.
    prune_debug_files(&log_dir());
    install_panic_hook();
    file
}

/// The file the logger is writing to, resolved by [`init`]. `None` when the
/// data directory could not be opened, so only stderr is captured.
pub(crate) fn file_path() -> Option<&'static Path> {
    LOG_PATH.get().map(PathBuf::as_path)
}

/// The last `max_lines` lines of the ordinary log, for the "copy recent log"
/// action. Reading is cheap and safe beside the open append handle: the file is
/// shared for read on every platform Rust supports.
pub(crate) fn tail(max_lines: usize) -> Option<String> {
    let path = LOG_PATH.get()?;
    let text = fs::read_to_string(path).ok()?;
    let lines = text.lines().collect::<Vec<_>>();
    let start = lines.len().saturating_sub(max_lines);
    Some(lines[start..].join("\n"))
}

/// Start a ten-minute capture in a new timestamped file. The ordinary log is
/// untouched; the returned path is the file to attach to a report.
pub(crate) fn start_debug_capture() -> Result<PathBuf, &'static str> {
    if env_locked() {
        return Err("RUST_LOG is set; the log follows that filter instead");
    }

    let dir = log_dir();
    restbook_store::secure::create_dir(&dir).map_err(|_| "could not create the log directory")?;

    // One capture at a time. Drop the old handle before pruning, or Windows
    // refuses to remove the file that is still open.
    {
        let mut capture = DEBUG
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        capture.file = None;
        capture.path = None;
        capture.bytes = 0;
    }
    prune_debug_files(&dir);

    let path = dir.join(format!(
        "{DEBUG_PREFIX}{}.log",
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
    ));
    let file =
        restbook_store::secure::create(&path).map_err(|_| "could not create the debug log")?;

    {
        let mut capture = DEBUG
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *capture = DebugCapture {
            file: Some(file),
            path: Some(path.clone()),
            bytes: 0,
        };
    }
    DEBUG_UNTIL.store(
        now_millis() + DEBUG_WINDOW.as_millis() as u64,
        Ordering::Relaxed,
    );
    Ok(path)
}

/// Whether a capture is running and has not expired.
pub(crate) fn debug_active() -> bool {
    debug_window_open(
        now_millis(),
        DEBUG_UNTIL.load(Ordering::Relaxed),
        env_locked(),
    )
}

/// The time left in the active capture, for the settings row.
pub(crate) fn debug_remaining() -> Option<Duration> {
    if !debug_active() {
        return None;
    }
    let until = DEBUG_UNTIL.load(Ordering::Relaxed);
    Some(Duration::from_millis(until.saturating_sub(now_millis())))
}

pub(crate) fn debug_capture_path() -> Option<PathBuf> {
    DEBUG
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .path
        .clone()
}

/// Stop the capture and remove its file. Returns the path that was removed.
pub(crate) fn delete_debug_capture() -> Option<PathBuf> {
    DEBUG_UNTIL.store(0, Ordering::Relaxed);
    let path = {
        let mut capture = DEBUG
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        capture.file = None;
        capture.bytes = 0;
        capture.path.take()
    };
    if let Some(path) = &path {
        let _ = fs::remove_file(path);
    }
    path
}

/// One line for the interaction transcript. The ordinary logger's `info`
/// filter drops it; a running capture keeps it.
pub(crate) fn event(line: impl std::fmt::Display) {
    log::debug!(target: "restbook_app::ui", "{line}");
}

/// A URL safe for the ordinary log: scheme, host, and path, with userinfo and
/// the query dropped. A token in `?access_token=` must not reach a file that is
/// meant to be attachable to a bug report.
pub(crate) fn redact_url(url: &str) -> String {
    let without_fragment = url.split('#').next().unwrap_or(url);
    let without_query = without_fragment
        .split('?')
        .next()
        .unwrap_or(without_fragment);
    match without_query.split_once("://") {
        Some((scheme, rest)) => {
            let host = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
            format!("{scheme}://{host}")
        }
        None => without_query.to_owned(),
    }
}

/// [`redact_url`] for a whole message. `reqwest` quotes the URL it was given —
/// userinfo, query, and all — in a transport error's text, and that text reaches
/// both the log and the failure Details. Each URL in the message is reduced to
/// scheme, host, and path; the cause chain around it is kept.
pub(crate) fn redact_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let scheme_start = rest[..at]
            .rfind(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.')))
            .map_or(0, |index| index + 1);
        out.push_str(&rest[..scheme_start]);

        let url = &rest[scheme_start..];
        let end = url
            .find(|ch: char| {
                ch.is_whitespace() || matches!(ch, ')' | ']' | '}' | '"' | '\'' | '<' | '>' | '`')
            })
            .unwrap_or(url.len());
        let (head, tail) = url.split_at(end);
        out.push_str(&redact_url(head));
        rest = tail;
    }
    out.push_str(rest);
    out
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn env_locked() -> bool {
    ENV_FILTER.load(Ordering::Relaxed)
}

/// Whether a window is open, stated as a pure function so the deadline and the
/// environment rule can be tested without sleeping.
fn debug_window_open(now: u64, until: u64, env_locked: bool) -> bool {
    !env_locked && until > now
}

fn log_dir() -> PathBuf {
    LOG_PATH
        .get()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(crate::workspace::storage::data_dir)
}

/// Remove every capture file in `dir`. Called on startup and before a new
/// capture, which is how a report that was never sent disappears.
fn prune_debug_files(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(DEBUG_PREFIX) && name.ends_with(".log") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Log the panic, then let the default hook run so a terminal still sees what it
/// always saw. Only the file makes the reason reachable after a crash from
/// Explorer or an editor task, where the process simply vanishes.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panicked: {info}");
        // A detached run has no `RUST_BACKTRACE` habit, so a stack is logged
        // when the variable asks for one, not always: frames cost a lot of
        // bytes and most panics carry a message that names the cause.
        if backtrace_requested() {
            log::error!("\n{}", std::backtrace::Backtrace::force_capture());
        }
        previous(info);
    }));
}

fn backtrace_requested() -> bool {
    std::env::var_os("RUST_BACKTRACE").is_some_and(|value| value != "0")
}

/// `env_logger`, plus the second sink a capture writes to.
///
/// The ordinary logger owns `restbook.log` and filters at `info` unless
/// `RUST_LOG` says otherwise — an explicit filter is the developer asking for
/// the transcript in that file too. The capture logger filters at `debug` but
/// its sink drops everything unless a file is open.
struct Closing {
    normal: env_logger::Logger,
    capture: env_logger::Logger,
}

impl log::Log for Closing {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        self.normal.enabled(metadata) || (debug_active() && self.capture.enabled(metadata))
    }

    fn log(&self, record: &log::Record<'_>) {
        if teardown_noise(
            record.target(),
            record.level(),
            CLOSING.load(Ordering::Relaxed),
        ) {
            return;
        }
        self.normal.log(record);
        if debug_active() {
            self.capture.log(record);
        }
    }

    fn flush(&self) {
        self.normal.flush();
        self.capture.flush();
    }
}

/// The capture file, written only while a window is open. Routing happens at
/// the record level rather than in [`Tee`], so a debug line never reaches the
/// ordinary file.
struct DebugSink;

impl Write for DebugSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut capture = DEBUG
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let DebugCapture { file, bytes, .. } = &mut *capture;
        let mut close = false;
        if let Some(open) = file.as_mut() {
            if *bytes < MAX_DEBUG_BYTES {
                let room = (MAX_DEBUG_BYTES - *bytes) as usize;
                let take = room.min(buf.len());
                open.write_all(&buf[..take])?;
                *bytes += take as u64;
                if take < buf.len() {
                    let _ = open.write_all(b"debug log truncated at its size cap\n");
                    close = true;
                }
            }
        }
        if close {
            *file = None;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut capture = DEBUG
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(file) = capture.file.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}

/// `true` for the `window not found` / `Invalid window handle` records gpui-pre
/// writes while dropping a window.
fn teardown_noise(target: &str, level: log::Level, closing: bool) -> bool {
    closing && level == log::Level::Error && target.is_empty()
}

/// Both streams, one line at a time.
struct Tee {
    file: File,
}

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // stderr first: when it is a console the line appears live. A detached
        // or broken stderr is not worth losing the file line over, so its
        // result is ignored.
        let _ = io::stderr().write_all(buf);
        self.file.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let _ = io::stderr().flush();
        self.file.flush()
    }
}

/// Create the directory and open the file for appending, archiving it first
/// when it is full or was written by another version. One previous file is the
/// whole history: the ordinary log is a backstop, and an unbounded set of
/// rotated files is not.
fn open(path: &Path) -> Option<File> {
    if let Some(parent) = path.parent() {
        restbook_store::secure::create_dir(parent).ok()?;
    }
    let len = fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    if len > MAX_BYTES || (len > 0 && from_another_version(path)) {
        let backup = path.with_extension("log.1");
        let _ = fs::remove_file(&backup);
        let _ = fs::rename(path, backup);
    }
    restbook_store::secure::open_append(path).ok()
}

/// Whether the file holds no line from this build. The startup line names the
/// version, so a file without one is an older build's record: an upgrade
/// archives it instead of appending a new version's lines to that transcript,
/// which is how a fresh install starts with an empty log.
fn from_another_version(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    !text.contains(&format!("restbook {} starting", env!("CARGO_PKG_VERSION")))
}

#[cfg(test)]
mod tests {
    use super::{
        debug_window_open, install_panic_hook, open, prune_debug_files, redact_url, redact_urls,
        teardown_noise, Tee, MAX_BYTES,
    };
    use std::io::Write as _;

    /// Captures what the logger writes so the hook can be observed without
    /// touching the filesystem.
    struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// The only test that installs the process-wide logger, so it owns it.
    #[test]
    fn a_panic_reaches_the_log() {
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        env_logger::Builder::new()
            .filter_level(log::LevelFilter::Error)
            .target(env_logger::Target::Pipe(Box::new(Shared(buffer.clone()))))
            .try_init()
            .ok();

        install_panic_hook();
        // The hook runs before the unwind, so the line is written even though
        // the panic is caught here.
        let _ = std::panic::catch_unwind(|| panic!("a test panic"));

        let logged = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
        // `PanicHookInfo`'s own Display is `panicked at <file>:<line>:\n<message>`,
        // so the record arrives as two lines.
        assert!(logged.contains("panicked: panicked at"), "{logged}");
        assert!(logged.contains("a test panic"), "{logged}");
    }

    #[test]
    fn teardown_noise_is_only_hidden_after_the_close_path() {
        assert!(!teardown_noise("", log::Level::Error, false));
        assert!(teardown_noise("", log::Level::Error, true));
        // A targeted record is app activity even during close.
        assert!(!teardown_noise("restbook_app::ui", log::Level::Error, true));
        assert!(!teardown_noise("", log::Level::Warn, true));
    }

    #[test]
    fn the_tee_writes_the_line_to_the_file() {
        let dir = std::env::temp_dir().join("restbook-log-tee-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("restbook.log");

        let mut tee = Tee {
            file: open(&path).expect("a fresh log file"),
        };
        tee.write_all(b"hello from the tee\n").unwrap();
        tee.flush().unwrap();
        drop(tee);

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "hello from the tee\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The log carries redacted URLs but still enough detail to be worth
    /// keeping private: `0600` where the platform has a mode.
    #[cfg(unix)]
    #[test]
    fn the_log_file_is_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = std::env::temp_dir().join("restbook-log-private-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("restbook.log");

        drop(open(&path).expect("a fresh log file"));

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the log should be private");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_oversized_log_is_rotated_not_lost() {
        let dir = std::env::temp_dir().join("restbook-log-rotate-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("restbook.log");

        // One byte over the cap: `set_len` is cheaper than writing 1 MiB.
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();

        let mut file = open(&path).expect("a fresh file after rotation");
        file.write_all(b"next run\n").unwrap();
        drop(file);

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "next run\n");
        let backup = path.with_extension("log.1");
        assert_eq!(
            std::fs::metadata(backup).unwrap().len(),
            MAX_BYTES + 1,
            "the previous file is kept, not deleted"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An upgrade must not append the new build's lines to the old build's
    /// transcript: the previous file is archived, so an installation of a new
    /// version starts with a log that describes only that version.
    #[test]
    fn a_log_from_another_version_is_archived() {
        let dir = std::env::temp_dir().join("restbook-log-upgrade-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("restbook.log");
        std::fs::write(&path, b"[old] INFO  restbook_app] event: titlebar\n").unwrap();

        let mut file = open(&path).expect("a fresh file after the upgrade");
        file.write_all(b"next run\n").unwrap();
        drop(file);

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "next run\n");
        assert_eq!(
            std::fs::read_to_string(path.with_extension("log.1")).unwrap(),
            "[old] INFO  restbook_app] event: titlebar\n",
            "the previous version's log is kept, not deleted"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A restart of the same version appends; the marker in its startup line is
    /// what tells the two cases apart.
    #[test]
    fn a_log_from_this_version_keeps_appending() {
        let dir = std::env::temp_dir().join("restbook-log-same-version-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("restbook.log");
        let started = format!("restbook {} starting; log: x\n", env!("CARGO_PKG_VERSION"));
        std::fs::write(&path, started.as_bytes()).unwrap();

        let mut file = open(&path).expect("the existing file");
        file.write_all(b"next run\n").unwrap();
        drop(file);

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{started}next run\n")
        );
        assert!(!path.with_extension("log.1").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_directory_is_created() {
        let dir = std::env::temp_dir().join("restbook-log-mkdir-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("restbook.log");

        let file = open(&path);
        assert!(file.is_some());
        assert!(path.exists());
        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_debug_window_obeys_the_deadline_and_the_environment() {
        assert!(debug_window_open(1_000, 2_000, false));
        assert!(!debug_window_open(2_000, 2_000, false));
        // An explicit RUST_LOG is the developer's switch; a UI window cannot
        // raise or lower it.
        assert!(!debug_window_open(1_000, 2_000, true));
    }

    #[test]
    fn pruning_removes_old_captures_and_nothing_else() {
        let dir = std::env::temp_dir().join("restbook-log-prune-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let capture = dir.join("restbook-debug-20260101T000000Z.log");
        let ordinary = dir.join("restbook.log");
        let unrelated = dir.join("settings.json");
        for path in [&capture, &ordinary, &unrelated] {
            std::fs::write(path, b"x").unwrap();
        }

        prune_debug_files(&dir);

        assert!(!capture.exists());
        assert!(ordinary.exists());
        assert!(unrelated.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The ordinary log is attached to a bug report, so a URL in it must not
    /// carry a token from the query or a password from the userinfo.
    #[test]
    fn urls_lose_their_query_and_userinfo_in_the_ordinary_log() {
        assert_eq!(
            redact_url("https://user:pass@example.com/a/b?token=secret#frag"),
            "https://example.com/a/b"
        );
        assert_eq!(
            redact_url("http://127.0.0.1:8787/path?q=1"),
            "http://127.0.0.1:8787/path"
        );
        assert_eq!(redact_url("not a url?q=1"), "not a url");
    }

    /// `reqwest` quotes the URL it was handed inside a transport error's text, so
    /// redacting a message has to reach every URL in it and leave the cause chain
    /// around them readable.
    #[test]
    fn a_message_loses_every_url_secret_but_keeps_its_cause_chain() {
        assert_eq!(
            redact_urls(
                "error sending request for url (https://alice:secret@api.test/v1/items?token=abc): \
                 tcp connect error: refused; retry http://127.0.0.1:8787/path?q=1"
            ),
            "error sending request for url (https://api.test/v1/items): \
             tcp connect error: refused; retry http://127.0.0.1:8787/path"
        );
        assert_eq!(redact_urls("no url here"), "no url here");
    }
}

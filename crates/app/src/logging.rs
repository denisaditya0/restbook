//! Where log lines go when nobody is looking at a terminal. A GUI binary
//! launched from Explorer, a shortcut, or an editor task has no stderr, and
//! GPUI installs no sink, so `main.rs` installs one here: a tee where
//! `env_logger` formats and the same bytes go to stderr (when attached) and to
//! `<data dir>/restbook.log` (`RESTBOOK_LOG_FILE`; `RUST_LOG` filters). `init`
//! also installs a panic hook, since a panic never reaches a log line.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

/// The name of the log in the data directory.
pub(crate) const LOG_FILE: &str = "restbook.log";

/// The filter used when `RUST_LOG` is unset. `scripts/dev.cmd --logs` sets the
/// same value, so both ways of running the app show the same lines.
pub(crate) const DEFAULT_FILTER: &str =
    "warn,restbook_app=info,restbook_core=info,restbook_store=info,restbook_transport=info";

/// The file `init` opened, if any. Resolved once so chrome can name the exact
/// log being written, including a `RESTBOOK_LOG_FILE` override.
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Set once the window has written its last state. From then on target-less
/// `ERROR` records are gpui-pre's teardown, not app activity, and [`Closing`]
/// drops them.
static CLOSING: AtomicBool = AtomicBool::new(false);

/// Tell the sink that the close path has finished, so the platform teardown
/// that follows is not reported as an app failure.
pub(crate) fn mark_closing() {
    CLOSING.store(true, Ordering::Relaxed);
}

/// Truncate the file at startup once it passes this. A few lines per run is
/// normal, so 4 MiB is many months; the cap only exists so a long-lived
/// install cannot grow it forever.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Install the logger. Returns the file's path when it was opened, `None` when
/// only stderr is available (a read-only data directory, say).
pub(crate) fn init() -> Option<PathBuf> {
    let path = std::env::var_os("RESTBOOK_LOG_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::workspace::storage::data_dir().join(LOG_FILE));

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
    log::set_max_level(logger.filter());
    // Equivalent to `builder.init()`, but through [`Closing`]. Failing to set a
    // second logger is not fatal (a test may own it), which `init` also ignored.
    let _ = log::set_boxed_logger(Box::new(Closing { inner: logger }));
    install_panic_hook();
    file
}

/// The file the logger is writing to, resolved by [`init`]. `None` when the
/// data directory could not be opened, so only stderr is captured.
pub(crate) fn file_path() -> Option<&'static Path> {
    LOG_PATH.get().map(PathBuf::as_path)
}

/// The last `max_lines` lines of the log, for the "copy recent log" action.
/// Reading is cheap and safe beside the open append handle: the file is shared
/// for read on every platform Rust supports.
pub(crate) fn tail(max_lines: usize) -> Option<String> {
    let path = LOG_PATH.get()?;
    let text = fs::read_to_string(path).ok()?;
    let lines = text.lines().collect::<Vec<_>>();
    let start = lines.len().saturating_sub(max_lines);
    Some(lines[start..].join("\n"))
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

/// `env_logger`, minus the target-less errors that `gpui-pre` emits while it
/// tears a window down.
///
/// Those records carry no target because the dependency logs them through a
/// `log_err` helper that passes an empty one, so no `RUST_LOG` directive can
/// name them. Dropping them here is the only place the app controls, and it is
/// gated on [`CLOSING`]: before the close path finishes, a target-less error is
/// still reported.
struct Closing {
    inner: env_logger::Logger,
}

impl log::Log for Closing {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &log::Record<'_>) {
        if teardown_noise(
            record.target(),
            record.level(),
            CLOSING.load(Ordering::Relaxed),
        ) {
            return;
        }
        self.inner.log(record);
    }

    fn flush(&self) {
        self.inner.flush();
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

/// Create the directory and open the file for appending, truncating a file
/// that has grown past [`MAX_BYTES`]. A failure here is not fatal: the caller
/// falls back to stderr alone.
fn open(path: &Path) -> Option<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok()?;
    }
    if fs::metadata(path).map(|meta| meta.len()).unwrap_or(0) > MAX_BYTES {
        // Truncation, not rotation: one file to find, and the old contents are
        // worth less than the next run's lines.
        let _ = fs::remove_file(path);
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

#[cfg(test)]
mod tests {
    use super::{install_panic_hook, open, teardown_noise, Tee, MAX_BYTES};
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

    #[test]
    fn an_oversized_log_is_truncated_before_the_next_run() {
        let dir = std::env::temp_dir().join("restbook-log-truncate-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("restbook.log");

        // One byte over the cap: `set_len` is cheaper than writing 4 MiB.
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();

        let mut file = open(&path).expect("a fresh file after truncation");
        file.write_all(b"next run\n").unwrap();
        drop(file);

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "next run\n");
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
}

use super::*;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;
use std::time::Duration;

/// A scratch directory under the workspace's `target/t`.
fn scratch() -> tempfile::TempDir {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/t");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("log")
        .tempdir_in(base.canonicalize().unwrap())
        .unwrap()
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn snitchwatch_log_wins_and_adds_the_file() {
    assert_eq!(
        settings(Some("snitchwatch_kirigami=debug"), Some("warn")),
        Settings {
            filter: "snitchwatch_kirigami=debug".into(),
            to_file: true,
        }
    );
}

#[test]
fn rust_log_alone_logs_to_stderr_only() {
    assert_eq!(
        settings(None, Some("zbus=info,debug")),
        Settings {
            filter: "zbus=info,debug".into(),
            to_file: false,
        }
    );
}

#[test]
fn nothing_set_or_blank_means_info_to_stderr() {
    for (ours, rust) in [(None, None), (Some(""), Some("  ")), (Some(" "), None)] {
        assert_eq!(
            settings(ours, rust),
            Settings {
                filter: "info".into(),
                to_file: false,
            },
            "{ours:?} {rust:?}"
        );
    }
}

/// The directory is created 0700 and the log 0600. A restart keeps the
/// previous run's log as `gui.log.1`, made 0600 even if an older build
/// left it readable by others, and starts this run's log empty.
#[test]
fn the_log_is_owner_only_and_the_previous_one_is_kept() {
    let dir = scratch();
    let path = dir.path().join("state/snitchwatch/gui.log");
    keep_previous(&path).unwrap();
    let mut file = open_log(&path).unwrap();
    writeln!(file, "first run").unwrap();
    drop(file);
    assert_eq!(mode(path.parent().unwrap()), 0o700);
    assert_eq!(mode(&path), 0o600);

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    keep_previous(&path).unwrap();
    let mut file = open_log(&path).unwrap();
    writeln!(file, "second run").unwrap();
    drop(file);
    let previous = previous_path(&path);
    assert_eq!(read(&previous), "first run\n");
    assert_eq!(mode(&previous), 0o600);
    assert_eq!(read(&path), "second run\n");
    assert_eq!(mode(&path), 0o600);
}

/// Opening this run's log empties a log that couldn't be kept aside, and
/// writes from the start.
#[test]
fn an_existing_log_is_emptied_when_opened() {
    let dir = scratch();
    let path = dir.path().join("gui.log");
    std::fs::write(&path, "an old run\n").unwrap();
    let mut file = open_log(&path).unwrap();
    writeln!(file, "this run").unwrap();
    drop(file);
    assert_eq!(read(&path), "this run\n");
}

#[test]
fn a_symlink_at_the_log_path_is_refused() {
    let dir = scratch();
    let victim = dir.path().join("victim");
    std::fs::write(&victim, "keep me").unwrap();
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
    let path = dir.path().join("gui.log");
    std::os::unix::fs::symlink(&victim, &path).unwrap();
    assert!(keep_previous(&path).is_err());
    assert!(open_log(&path).is_err());
    assert_eq!(read(&victim), "keep me");
    assert_eq!(mode(&victim), 0o644);
    assert!(std::fs::symlink_metadata(&path)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(!previous_path(&path).exists());
}

#[test]
fn a_hard_linked_log_is_refused() {
    let dir = scratch();
    let other = dir.path().join("other");
    std::fs::write(&other, "keep me").unwrap();
    let path = dir.path().join("gui.log");
    std::fs::hard_link(&other, &path).unwrap();
    assert!(keep_previous(&path).is_err());
    assert!(open_log(&path).is_err());
    assert_eq!(read(&other), "keep me");
}

/// A FIFO at the path is refused without waiting for a reader or writer.
#[test]
fn a_fifo_at_the_log_path_neither_blocks_nor_is_used() {
    let dir = scratch();
    let path = dir.path().join("gui.log");
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `c_path` is a valid NUL-terminated path for the call's duration.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let (tx, rx) = std::sync::mpsc::channel();
    let fifo = path.clone();
    std::thread::spawn(move || {
        let _ = tx.send((keep_previous(&fifo).is_err(), open_log(&fifo).is_err()));
    });
    let refused = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("opening the FIFO blocked");
    assert_eq!(refused, (true, true));
}

#[test]
fn only_a_regular_file_of_ours_with_one_name_is_used() {
    let ours = 1000;
    let facts = |regular, uid, links| FileFacts {
        regular,
        uid,
        links,
    };
    assert_eq!(file_problem(&facts(true, ours, 1), ours), None);
    assert_eq!(
        file_problem(&facts(false, ours, 1), ours),
        Some("isn't a regular file")
    );
    assert_eq!(
        file_problem(&facts(true, 0, 1), ours),
        Some("belongs to another user")
    );
    assert_eq!(
        file_problem(&facts(true, ours, 2), ours),
        Some("has a hard link elsewhere")
    );
}

#[test]
fn the_frame_cap_covers_both_crates_and_nothing_else() {
    for (target, level, kept) in [
        ("tungstenite::protocol", Level::TRACE, false),
        ("tungstenite", Level::DEBUG, false),
        ("tokio_tungstenite::compat", Level::TRACE, false),
        ("tungstenite::protocol", Level::INFO, true),
        ("tokio_tungstenite", Level::WARN, true),
        ("tungstenite_extra", Level::TRACE, true),
        ("snitchwatch_kirigami::bridge_runtime", Level::TRACE, true),
    ] {
        assert_eq!(
            within_the_frame_cap(target, level),
            kept,
            "{target} {level}"
        );
    }
}

/// A writer into a shared buffer.
struct Sink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Even a filter asking for tungstenite at trace, by its most specific
/// target, never logs a frame: tungstenite's own `log` records (bridged by
/// `LogTracer`, as `init` installs) and tracing events alike.
#[test]
fn a_trace_filter_still_caps_tungstenite() {
    let _ = tracing_log::LogTracer::init();
    for filter in [
        "trace",
        "trace,tungstenite=trace,tungstenite::protocol=trace,tokio_tungstenite=trace",
    ] {
        let out = Arc::new(Mutex::new(Vec::new()));
        let sink = out.clone();
        let writer = BoxMakeWriter::new(move || Sink(sink.clone()));
        tracing::subscriber::with_default(
            subscriber(EnvFilter::new(filter), writer, false),
            || {
                log::trace!(target: "tungstenite::protocol", "Sending frame: TOKEN-1");
                log::debug!(target: "tokio_tungstenite", "TOKEN-2");
                tracing::trace!(target: "tungstenite::protocol", "TOKEN-3");
                log::info!(target: "tungstenite::protocol", "closed cleanly");
                tracing::trace!(target: "snitchwatch_kirigami::bridge_runtime", "our trace");
            },
        );
        let text = String::from_utf8(out.lock().unwrap().clone()).unwrap();
        assert!(!text.contains("TOKEN"), "{filter}: {text}");
        assert!(
            text.contains("closed cleanly") && text.contains("our trace"),
            "{filter}: {text}"
        );
    }
}

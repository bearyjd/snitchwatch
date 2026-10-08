//! The GUI's log, made easy to find (r11: a Flatpak run under
//! `systemd-run --user` left an empty log).
//!
//! - **Filter**: `SNITCHWATCH_LOG`, else `RUST_LOG`, else `info`, in
//!   `tracing`'s `EnvFilter` syntax (e.g. `snitchwatch_kirigami=debug,info`).
//!   A filter that doesn't parse falls back to `info`, and says so.
//! - **Never WebSocket frames.** tungstenite logs every frame it sends or
//!   receives, payload and all, at trace (through `log`, which `init`
//!   bridges), and the first text frame to the bridge is its handshake
//!   token. So `tungstenite` and `tokio_tungstenite` are capped at info
//!   whatever the filter says: by a filter of their own, not a directive,
//!   since a more specific directive (`tungstenite::protocol=trace`) would
//!   win over one.
//! - **stderr**, always. `flatpak run` moves itself into a transient
//!   `app-flatpak-<app id>-<instance>.scope` before starting the app
//!   (flatpak 1.16 `flatpak_run_in_transient_unit`), and journald files a
//!   stream's lines under the writer's current unit. So under
//!   `systemd-run --user --unit=X flatpak run …` the app's lines are in
//!   `journalctl --user -u 'app-flatpak-org.snitchwatch.Snitchwatch-*'`,
//!   not in `journalctl --user -u X`.
//! - **A file**, `gui.log` in the state directory, only when
//!   `SNITCHWATCH_LOG` is set. In the Flatpak that is
//!   `~/.var/app/org.snitchwatch.Snitchwatch/.local/state/snitchwatch/gui.log`
//!   on the host (`XDG_STATE_HOME` is per-app there). It is owner-only: the
//!   directory is created 0700, and the file is opened without following a
//!   symlink or waiting on a FIFO, must be a regular file of ours with no
//!   other hard link, and is then made 0600 and emptied. The previous run's
//!   log, if it was such a file, is kept as `gui.log.1`.

use std::fs::File;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::Level;
use tracing_subscriber::filter::{filter_fn, FilterExt};
use tracing_subscriber::fmt::writer::{BoxMakeWriter, MakeWriterExt};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// When nothing else is asked for.
const DEFAULT_FILTER: &str = "info";

/// Crates that log WebSocket frames with their payload below info
/// (tungstenite 0.30 `protocol/mod.rs`: "Received message …", "Sending
/// frame: …").
const FRAME_LOGGERS: [&str; 2] = ["tungstenite", "tokio_tungstenite"];

/// The log's settings, from the two environment variables.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Settings {
    /// `EnvFilter` directives.
    filter: String,
    /// Whether to also write `gui.log`.
    to_file: bool,
}

/// A variable's value, unless it is unset or blank.
fn set(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn settings(snitchwatch_log: Option<&str>, rust_log: Option<&str>) -> Settings {
    match (set(snitchwatch_log), set(rust_log)) {
        (Some(filter), _) => Settings {
            filter: filter.to_owned(),
            to_file: true,
        },
        (None, Some(filter)) => Settings {
            filter: filter.to_owned(),
            to_file: false,
        },
        (None, None) => Settings {
            filter: DEFAULT_FILTER.to_owned(),
            to_file: false,
        },
    }
}

/// Whether an event may be logged whatever the filter says: nothing below
/// info from a crate that logs frames.
fn within_the_frame_cap(target: &str, level: Level) -> bool {
    let logs_frames = FRAME_LOGGERS.iter().any(|krate| {
        target
            .strip_prefix(krate)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
    });
    !logs_frames || level <= Level::INFO
}

/// The GUI's subscriber: `filter`, under the frame cap, to `writer`.
fn subscriber(
    filter: EnvFilter,
    writer: BoxMakeWriter,
    ansi: bool,
) -> impl tracing::Subscriber + Send + Sync {
    let cap = filter_fn(|meta| within_the_frame_cap(meta.target(), *meta.level()));
    tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(ansi)
            .with_filter(filter.and(cap)),
    )
}

/// Install the GUI's subscriber. Call once, first thing in `main`.
pub fn init() {
    let settings = settings(
        std::env::var("SNITCHWATCH_LOG").ok().as_deref(),
        std::env::var("RUST_LOG").ok().as_deref(),
    );
    let (filter, bad_filter) = match EnvFilter::try_new(&settings.filter) {
        Ok(filter) => (filter, None),
        Err(error) => (EnvFilter::new(DEFAULT_FILTER), Some(error)),
    };
    let log_path = crate::paths::gui_log_path();
    let (kept, file) = if settings.to_file {
        (Some(keep_previous(&log_path)), Some(open_log(&log_path)))
    } else {
        (None, None)
    };
    let (writer, ansi, file) = match file {
        Some(Ok(file)) => (
            BoxMakeWriter::new(std::io::stderr.and(Arc::new(file))),
            false,
            Some(Ok(())),
        ),
        Some(Err(error)) => (BoxMakeWriter::new(std::io::stderr), false, Some(Err(error))),
        None => (
            BoxMakeWriter::new(std::io::stderr),
            std::io::stderr().is_terminal(),
            None,
        ),
    };
    subscriber(filter, writer, ansi).init();
    if let Some(error) = bad_filter {
        tracing::warn!(%error, filter = %settings.filter, "log filter not understood; using info");
    }
    if let Some(Err(error)) = kept {
        tracing::warn!(%error, "the previous log was not kept");
    }
    match file {
        Some(Ok(())) => tracing::info!(path = %log_path.display(), "also logging to this file"),
        Some(Err(error)) => {
            tracing::warn!(%error, path = %log_path.display(), "log file not opened")
        }
        None => {}
    }
}

/// `gui.log.1`, next to `path`.
fn previous_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

/// Keep the previous run's log as `gui.log.1` (replacing an older one), if
/// it is a regular file of ours with no other hard link, made 0600 first.
/// Anything else at `path` is left for [`open_log`] to refuse.
fn keep_previous(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let old = match open_no_follow(path, std::fs::OpenOptions::new().read(true)) {
        Ok(old) => old,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    refuse_unless_ours(path, &old)?;
    old.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(path, previous_path(path))
}

/// Open this run's log at `path`, owner-only and empty: see the module doc.
fn open_log(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let file = open_no_follow(
        path,
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600),
    )?;
    refuse_unless_ours(path, &file)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.set_len(0)?;
    Ok(file)
}

/// `options` plus `O_NOFOLLOW`, `O_NONBLOCK` (a FIFO doesn't hold us up)
/// and `O_CLOEXEC`.
fn open_no_follow(path: &Path, options: &mut std::fs::OpenOptions) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    options
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}

/// What matters about a log file, from its handle.
struct FileFacts {
    regular: bool,
    uid: u32,
    /// Hard links (`st_nlink`).
    links: u64,
}

/// Why a log file can't be used, if it can't: it must be a regular file
/// (not a FIFO or device) owned by `euid` whose only name is this one.
/// `snitchwatch-bridge`'s `sqlite_file::file_problem`, for the same reasons.
fn file_problem(facts: &FileFacts, euid: u32) -> Option<&'static str> {
    if !facts.regular {
        Some("isn't a regular file")
    } else if facts.uid != euid {
        Some("belongs to another user")
    } else if facts.links != 1 {
        Some("has a hard link elsewhere")
    } else {
        None
    }
}

fn refuse_unless_ours(path: &Path, file: &File) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata()?;
    let facts = FileFacts {
        regular: meta.is_file(),
        uid: meta.uid(),
        links: meta.nlink(),
    };
    match file_problem(&facts, effective_uid()) {
        None => Ok(()),
        Some(why) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} {why}, so it was left as it is", path.display()),
        )),
    }
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, touches no memory and always
    // succeeds.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
#[path = "logging/tests.rs"]
mod tests;

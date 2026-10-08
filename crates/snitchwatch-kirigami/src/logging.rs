//! The GUI's log, made easy to find (r11: a Flatpak run under
//! `systemd-run --user` left an empty log).
//!
//! - **Filter**: `SNITCHWATCH_LOG`, else `RUST_LOG`, else `info`, in
//!   `tracing`'s `EnvFilter` syntax (e.g. `snitchwatch_kirigami=debug,info`).
//!   A filter that doesn't parse falls back to `info`, and says so.
//! - **stderr**, always. `flatpak run` moves itself into a transient
//!   `app-flatpak-<app id>-<instance>.scope` before starting the app
//!   (flatpak 1.16 `flatpak_run_in_transient_unit`), and journald files a
//!   stream's lines under the writer's current unit. So under
//!   `systemd-run --user --unit=X flatpak run …` the app's lines are in
//!   `journalctl --user -u 'app-flatpak-org.snitchwatch.Snitchwatch-*'`,
//!   not in `journalctl --user -u X`.
//! - **A file**, `gui.log` in the state directory, truncated at start, only
//!   when `SNITCHWATCH_LOG` is set. In the Flatpak that is
//!   `~/.var/app/org.snitchwatch.Snitchwatch/.local/state/snitchwatch/gui.log`
//!   on the host (`XDG_STATE_HOME` is per-app there).

use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;

use tracing_subscriber::fmt::writer::{BoxMakeWriter, MakeWriterExt};
use tracing_subscriber::EnvFilter;

/// When nothing else is asked for.
const DEFAULT_FILTER: &str = "info";

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
    let file = settings.to_file.then(|| open_log(&log_path)).transpose();
    let (writer, ansi, file_error) = match file {
        Ok(Some(file)) => (
            BoxMakeWriter::new(std::io::stderr.and(Arc::new(file))),
            false,
            None,
        ),
        Ok(None) => (
            BoxMakeWriter::new(std::io::stderr),
            std::io::stderr().is_terminal(),
            None,
        ),
        Err(error) => (BoxMakeWriter::new(std::io::stderr), false, Some(error)),
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(ansi)
        .init();
    if let Some(error) = bad_filter {
        tracing::warn!(%error, filter = %settings.filter, "log filter not understood; using info");
    }
    match file_error {
        Some(error) => tracing::warn!(%error, path = %log_path.display(), "log file not opened"),
        None if settings.to_file => {
            tracing::info!(path = %log_path.display(), "also logging to this file")
        }
        None => {}
    }
}

fn open_log(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::File::create(path)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn the_log_file_is_opened_fresh_in_a_new_directory() {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/t");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("log")
            .tempdir_in(base.canonicalize().unwrap())
            .unwrap();
        let path = dir.path().join("state/snitchwatch/gui.log");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "an old run\n").unwrap();
        drop(open_log(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        let fresh = dir.path().join("new/dir/gui.log");
        drop(open_log(&fresh).unwrap());
        assert!(fresh.is_file());
    }
}

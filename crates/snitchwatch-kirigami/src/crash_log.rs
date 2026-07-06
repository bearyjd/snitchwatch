//! Crash log tail-reading logic (Task 16).
//!
//! `panic_hook.rs` already writes timestamped panic entries to
//! `paths::crash_log_path()`; this module is the Qt-free logic behind the
//! Diagnostics page's "read_crash_log_tail" surface — reads the file, keeps
//! only the last `max_lines` lines, and degrades to a friendly message rather
//! than an error string when the file is missing or unreadable (a fresh
//! install with no crashes yet must not look like a broken feature).

use std::path::Path;

const NO_CRASH_LOG_MESSAGE: &str = "No crash log yet — Snitchwatch hasn't recorded any crashes.";

/// Read the last `max_lines` lines of the crash log at `path`. Never panics or
/// returns an `Err`: a missing file reads as "no crashes yet", and any other
/// I/O error (permissions, non-UTF8 content, …) surfaces as a short
/// human-readable note instead of propagating.
pub fn read_crash_log_tail(path: &Path, max_lines: usize) -> String {
    match std::fs::read_to_string(path) {
        Ok(contents) => tail_lines(&contents, max_lines),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => NO_CRASH_LOG_MESSAGE.to_string(),
        Err(e) => format!("Could not read crash log ({e}). Path: {}", path.display()),
    }
}

/// Keep the last `max_lines` lines of `contents`, preserving order.
fn tail_lines(contents: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = contents.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    if start == 0 {
        contents.to_string()
    } else {
        lines[start..].join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_reads_as_friendly_message() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("crash.log");
        assert_eq!(read_crash_log_tail(&p, 200), NO_CRASH_LOG_MESSAGE);
    }

    #[test]
    fn short_file_returns_full_contents() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("crash.log");
        std::fs::write(&p, "line1\nline2\nline3").unwrap();
        assert_eq!(read_crash_log_tail(&p, 200), "line1\nline2\nline3");
    }

    #[test]
    fn long_file_keeps_only_last_n_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("crash.log");
        let body: String = (0..500)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&p, &body).unwrap();

        let tail = read_crash_log_tail(&p, 200);
        let tail_lines: Vec<&str> = tail.lines().collect();
        assert_eq!(tail_lines.len(), 200);
        assert_eq!(tail_lines[0], "line300");
        assert_eq!(tail_lines[199], "line499");
    }

    #[test]
    fn unreadable_directory_path_surfaces_a_message_not_a_panic() {
        // A directory can never be read as a file: exercises the non-NotFound
        // error branch without depending on chmod/permission quirks in CI.
        let dir = tempfile::tempdir().unwrap();
        let msg = read_crash_log_tail(dir.path(), 200);
        assert!(msg.contains("Could not read crash log"));
    }

    #[test]
    fn tail_lines_helper_returns_full_body_when_under_limit() {
        assert_eq!(tail_lines("a\nb", 200), "a\nb");
    }
}

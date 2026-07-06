//! Process-wide panic hook.
//!
//! Writes a timestamped backtrace to crash.log and re-emits to tracing::error!
//! so the Diagnostics tab (Task 16) can surface it. The GUI shell stays alive;
//! only the bridge tokio task is restarted (handled in bridge_runtime).
//!
//! Ported verbatim from `snitchwatch-tauri::panic_hook` (Task 14): pure `std`,
//! no Tauri dependency.

use std::fs::OpenOptions;
use std::io::Write;
use std::panic::PanicHookInfo;
use std::path::PathBuf;
use std::sync::Mutex;

static CRASH_LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn install(crash_log_path: PathBuf) {
    if let Some(parent) = crash_log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    *CRASH_LOG_PATH.lock().unwrap() = Some(crash_log_path);

    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
        write_crash_entry(info);
        prev(info);
    }));
}

fn write_crash_entry(info: &PanicHookInfo<'_>) {
    let path = match CRASH_LOG_PATH.lock().unwrap().clone() {
        Some(p) => p,
        None => return,
    };
    let now = chrono_lite_now_iso();
    let payload = format!(
        "----- snitchwatch panic @ {now} -----\n{info}\n{:?}\n\n",
        std::backtrace::Backtrace::force_capture()
    );
    if let Ok(mut f) = OpenOptions::new().append(true).create(true).open(&path) {
        let _ = f.write_all(payload.as_bytes());
    }
    tracing::error!(target: "snitchwatch::panic", "{payload}");
}

fn chrono_lite_now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn install_creates_parent_dir_and_writes_on_panic() {
        let dir = tempfile::tempdir().unwrap();
        let crash_log = dir.path().join("nested").join("crash.log");
        install(crash_log.clone());

        // Trigger a panic in a child thread so the test doesn't die.
        let _ = std::thread::spawn(|| {
            panic!("test panic from snitchwatch-kirigami::panic_hook");
        })
        .join();

        assert!(crash_log.exists(), "crash log should have been created");
        let body = fs::read_to_string(&crash_log).unwrap();
        assert!(body.contains("test panic from snitchwatch-kirigami"));
        assert!(body.contains("snitchwatch panic @"));
    }
}

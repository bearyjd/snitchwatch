//! Rule import/export files (roadmap P2.7) without Qt: the bounded import
//! read and the owner-only export write. [`super::io_view`] lays out what
//! the bridge answers; [`crate::rules_io_controller`] binds both to QML.
//!
//! The bridge is authoritative (it parses and checks every rule again);
//! these checks only fail fast and keep the UI thread safe: the file must be
//! a regular file (opening a FIFO would block), at most
//! [`MAX_IMPORT_FILE_BYTES`], and JSON; the request built from it must fit
//! the bridge's client message cap, which re-serializing can change.

use snitchwatch_bridge::ws_messages::ClientMessage;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Largest import file read, the bridge's document cap.
pub const MAX_IMPORT_FILE_BYTES: usize = snitchwatch_bridge::rule_io::MAX_DOCUMENT_BYTES;

/// The import limit in words. An exported rule (compact JSON) takes about
/// 500 bytes when it names a program, host and port, and about 285 when it
/// names only a host.
const SIZE_LIMIT: &str = "This file is too large to import. Snitchwatch imports files of up \
     to 960 KiB, which is about 2,000 to 3,500 rules.";

/// Why a file wasn't read or written. Fixed text: nothing echoes the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileError {
    NotLocal,
    NotAFile,
    TooLarge,
    Unreadable,
    NotJson,
    NotWritten,
}

impl FileError {
    pub fn describe(self) -> &'static str {
        match self {
            Self::NotLocal => "Choose a file on this computer.",
            Self::NotAFile => "That isn't a regular file.",
            Self::TooLarge => SIZE_LIMIT,
            Self::Unreadable => "The file couldn't be read.",
            Self::NotJson => "This file isn't valid JSON, so it isn't a Snitchwatch rules file.",
            Self::NotWritten => "The file couldn't be saved there.",
        }
    }
}

/// Read an import file: a regular file of at most [`MAX_IMPORT_FILE_BYTES`]
/// holding JSON.
pub fn read_import_file(path: &Path) -> Result<serde_json::Value, FileError> {
    // Checked before opening: opening a FIFO blocks until a writer appears.
    let meta = std::fs::metadata(path).map_err(|_| FileError::Unreadable)?;
    if !meta.is_file() {
        return Err(FileError::NotAFile);
    }
    if meta.len() > MAX_IMPORT_FILE_BYTES as u64 {
        return Err(FileError::TooLarge);
    }
    // Non-blocking, so a FIFO swapped in since the check can't block the
    // UI thread.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| FileError::Unreadable)?;
    // The path may have been swapped since the check.
    if !file.metadata().is_ok_and(|opened| opened.is_file()) {
        return Err(FileError::NotAFile);
    }
    let mut bytes = Vec::new();
    file.take(MAX_IMPORT_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| FileError::Unreadable)?;
    if bytes.len() > MAX_IMPORT_FILE_BYTES {
        return Err(FileError::TooLarge);
    }
    serde_json::from_slice(&bytes).map_err(|_| FileError::NotJson)
}

/// The `PreviewRulesImport` message for `document`, refused when its JSON
/// would exceed the bridge's client message cap (the bridge would drop it).
pub fn preview_request(
    request_id: String,
    document: serde_json::Value,
) -> Result<ClientMessage, FileError> {
    let message = ClientMessage::PreviewRulesImport {
        request_id,
        document,
        reply: None,
    };
    let bytes = serde_json::to_vec(&message).map_err(|_| FileError::NotJson)?;
    if bytes.len() > snitchwatch_bridge::ws_server::MAX_CLIENT_MESSAGE_BYTES {
        return Err(FileError::TooLarge);
    }
    Ok(message)
}

/// Save an export readable only by its owner: it names programs and hosts.
/// Written to a new file beside `path` (`O_CREAT|O_EXCL|O_NOFOLLOW`, mode
/// 0600), synced, then renamed over `path`: a symlink there is replaced,
/// never followed, and an existing file is never truncated in place.
pub fn write_export_file(path: &Path, contents: &str) -> Result<(), FileError> {
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()) {
        return Err(FileError::NotWritten);
    }
    let (temp, mut file) = create_temp_beside(path)?;
    let written = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
        return Err(FileError::NotWritten);
    }
    Ok(())
}

fn create_temp_beside(path: &Path) -> Result<(PathBuf, std::fs::File), FileError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().ok_or(FileError::NotWritten)?;
    let name = path
        .file_name()
        .ok_or(FileError::NotWritten)?
        .to_string_lossy();
    for _ in 0..8 {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let temp = dir.join(format!(".{name}.{}-{n}.tmp", std::process::id()));
        let created = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&temp);
        match created {
            Ok(file) => return Ok((temp, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(FileError::NotWritten),
        }
    }
    Err(FileError::NotWritten)
}

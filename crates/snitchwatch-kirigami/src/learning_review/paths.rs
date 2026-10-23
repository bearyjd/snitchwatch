//! Which paths a rule may be made for, and which are system paths.

use snitchwatch_bridge::translator::process_binding::is_bindable_process_path;

use super::types::NonSelectable;
use crate::learned::Program;

/// The suffix `/proc/<pid>/exe` gets once the file was removed.
const DELETED_SUFFIX: &str = "(deleted)";

/// Directories whose files count as the system's own for the block warning
/// and the "named like a system program" flag.
const SYSTEM_DIRS: [&str; 6] = ["/usr/", "/bin/", "/sbin/", "/lib/", "/lib32/", "/lib64/"];

/// Directories directly under `/usr` that hold programs.
const USR_PROGRAM_DIRS: [&str; 7] = ["bin", "sbin", "libexec", "lib", "lib32", "lib64", "libx32"];

/// The program's path when a rule can be made for it, else why not: no path,
/// a file that was deleted, a path that was cut or changed to be kept
/// (so it is not the program's exact path), or a string that names no
/// program file (a relative or non-canonical path, `/proc/...`).
pub fn selectability(program: &Program) -> Result<&str, NonSelectable> {
    let path = program
        .raw_path
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .ok_or(NonSelectable::NoPath)?;
    if path.trim_end().ends_with(DELETED_SUFFIX) {
        Err(NonSelectable::Deleted)
    } else if program.truncated {
        Err(NonSelectable::Truncated)
    } else if !is_bindable_process_path(path) {
        Err(NonSelectable::NotBindable)
    } else {
        Ok(path)
    }
}

/// A file directly under `/usr/bin`, `/usr/sbin`, `/usr/libexec` or
/// `/usr/lib*` (`lib`, `lib32`, `lib64`, `libx32`): no deeper, so
/// `/usr/lib/firefox/firefox` and `/var/lib/flatpak/...` do not count, and
/// neither does `/opt`, `/usr/local` or the user Flatpak tree. A closed set
/// rather than a glob, so a made-up `/usr/library` is not one.
pub fn is_direct_system_path(path: &str) -> bool {
    let Some((parent, file)) = path.rsplit_once('/') else {
        return false;
    };
    !file.is_empty()
        && parent
            .strip_prefix("/usr/")
            .is_some_and(|dir| USR_PROGRAM_DIRS.contains(&dir))
}

/// Under a directory the system itself owns (`/usr` including `/usr/local`,
/// `/bin`, `/sbin`, `/lib*`).
pub fn is_system_path(path: &str) -> bool {
    SYSTEM_DIRS.iter().any(|dir| path.starts_with(dir))
}

/// The last component.
pub(super) fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

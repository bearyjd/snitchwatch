//! Process path -> application icon name, via installed `.desktop` files.
//!
//! A `.desktop` entry matches a process when its `TryExec`/`Exec` program (the
//! first real token) is the process path, or has the same basename **and the
//! process lives in a standard binary directory** ([`BIN_DIRS`]): a program
//! in `/tmp` or a build directory merely named `firefox` does not borrow
//! Firefox's icon. Flatpak apps are matched by app id, and only for a host
//! path under `/var/lib/flatpak/app/` or `$HOME/.local/share/flatpak/app/`
//! (`.../flatpak/app/<app-id>/...`), against `X-Flatpak=`,
//! `Exec=flatpak run ... <app-id>` or the desktop file stem. A bare `/app/...`
//! path, a `bwrap` process or any other path carries no trustworthy id, so
//! those fall back to [`FALLBACK_ICON`].
//!
//! A `.desktop` file is untrusted input. The scan reads only regular files
//! (a symlink to one is fine) of at most [`MAX_DESKTOP_BYTES`], looks at no
//! more than [`MAX_DESKTOP_FILES`] files, and keeps an `Icon=` only when it is
//! a theme name or an absolute path to an existing file under the data dirs
//! searched; a URL or any other path makes the entry unusable (generic icon).
//!
//! Parsing and matching are pure ([`parse_desktop`], [`AppIconIndex`]); only
//! [`icon_for_path`] touches the host. The index is built once, on a
//! background thread ([`prewarm`] at startup, or the first lookup), never on
//! the UI thread: until it is ready a lookup answers the generic icon without
//! caching it, so rows read before then keep the generic icon until they are
//! read again. Apps installed while the GUI runs show it until restart.

use crate::app_names::clean_display;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once, OnceLock};

/// Generic icon name used when no `.desktop` entry matches.
pub const FALLBACK_ICON: &str = "application-x-executable";

/// Most entries in each per-path result cache; when full it is cleared and
/// refilled, so a long run over many distinct paths cannot grow it unbounded.
const MAX_CACHED_PATHS: usize = 4096;

/// Insert into a cache holding at most `cap` entries (clear-and-refill).
fn insert_capped<V>(cache: &mut HashMap<String, V>, cap: usize, key: &str, value: V) {
    if cache.len() >= cap && !cache.contains_key(key) {
        cache.clear();
    }
    cache.insert(key.to_string(), value);
}

/// Directories whose programs may be matched to a `.desktop` entry by
/// basename alone. The icon is a hint, not the program's identity: a
/// look-alike in one of these can still borrow an icon, which is why the
/// alert also shows the full path under the name. `/app/bin` is not here: it
/// is a path inside a Flatpak sandbox, which names no host program.
const BIN_DIRS: [&str; 6] = [
    "/usr/bin",
    "/usr/sbin",
    "/usr/local/bin",
    "/bin",
    "/sbin",
    "/usr/libexec",
];

/// Host prefixes under which a Flatpak app id is read from the path.
const FLATPAK_SYSTEM_APPS: &str = "/var/lib/flatpak/app/";
const FLATPAK_USER_APPS: &str = ".local/share/flatpak/app/";

/// Most `.desktop` bytes read from one file.
const MAX_DESKTOP_BYTES: u64 = 64 * 1024;
/// Most `.desktop` files looked at in one scan.
const MAX_DESKTOP_FILES: usize = 5000;

/// The fields of one `[Desktop Entry]` that icon matching needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    /// Programs this entry launches (`TryExec`, then `Exec`'s first token).
    programs: Vec<String>,
    icon: String,
    /// Flatpak app id, when the entry declares or implies one.
    flatpak_id: Option<String>,
    /// The unlocalized `Name=`, sanitized. A label only: a desktop file in a
    /// user-writable directory can name any program anything.
    name: Option<String>,
}

/// Split an `Exec=` value into tokens, honouring double quotes and backslash
/// escapes (desktop-entry quoting rules, minus field-code expansion).
fn exec_tokens(exec: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut started = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    started = true;
                }
            }
            '"' => {
                in_quotes = !in_quotes;
                started = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if started {
                    tokens.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        tokens.push(cur);
    }
    tokens
}

/// Decode the Desktop Entry value escapes `\s`, `\n`, `\t`, `\r` and `\\`;
/// any other backslash is kept as written.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The program an `Exec` line runs: first token, skipping an `env` wrapper
/// and `VAR=value` assignments.
fn exec_program(tokens: &[String]) -> Option<&str> {
    let mut it = tokens.iter().map(String::as_str).peekable();
    if it.peek().is_some_and(|t| basename(t) == "env") {
        it.next();
    }
    it.find(|t| !(t.contains('=') && !t.starts_with('/')))
}

/// `Exec=flatpak run [--opts] <app-id> [args]` -> `<app-id>`.
fn flatpak_run_id(tokens: &[String]) -> Option<String> {
    let run = tokens.iter().position(|t| basename(t) == "flatpak")?;
    let mut rest = tokens[run + 1..].iter();
    if rest.next().map(String::as_str) != Some("run") {
        return None;
    }
    rest.find(|t| !t.starts_with('-') && !t.starts_with('%'))
        .filter(|t| looks_like_app_id(t))
        .cloned()
}

fn looks_like_app_id(s: &str) -> bool {
    s.contains('.')
        && !s.contains('/')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// Parse one `.desktop` file. `stem` is the file name without `.desktop`
/// (reverse-DNS stems are Flatpak app ids). Returns `None` for entries that
/// are not applications or carry no icon or program.
pub fn parse_desktop(contents: &str, stem: &str) -> Option<DesktopEntry> {
    let (mut in_entry, mut kind_ok) = (false, true);
    let (mut exec, mut try_exec, mut icon, mut x_flatpak) = (None, None, None, None);
    let mut name: Option<String> = None;
    for line in contents.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Type" => kind_ok = value == "Application",
            "Exec" => exec = Some(value.to_string()),
            "TryExec" => try_exec = Some(value.to_string()),
            "Icon" => icon = Some(value.to_string()),
            "X-Flatpak" => x_flatpak = Some(value.to_string()),
            // Only the exact key (`Name[de]` is a different one). The FIRST
            // `Name=` wins: if it cleans to empty there is no name, and later
            // lines are ignored.
            "Name" if name.is_none() => name = Some(clean_display(&unescape(value))),
            _ => {}
        }
    }
    let icon = icon.filter(|i| !i.is_empty())?;
    if !kind_ok {
        return None;
    }
    let tokens = exec.as_deref().map(exec_tokens).unwrap_or_default();
    let flatpak_id = x_flatpak
        .filter(|id| looks_like_app_id(id))
        .or_else(|| flatpak_run_id(&tokens))
        .or_else(|| {
            looks_like_app_id(stem)
                .then(|| stem.to_string())
                .filter(|_| tokens.iter().any(|t| basename(t) == "flatpak"))
        });
    let mut programs: Vec<String> = try_exec.into_iter().collect();
    // `flatpak run ...` launches flatpak, not the app: never match it by name.
    if let Some(p) = exec_program(&tokens).filter(|p| basename(p) != "flatpak") {
        programs.push(p.to_string());
    }
    if programs.is_empty() && flatpak_id.is_none() {
        return None;
    }
    Some(DesktopEntry {
        programs,
        icon,
        flatpak_id,
        name: name.filter(|n| !n.is_empty()),
    })
}

/// App id from a host-side Flatpak install path
/// (`<apps>/<app-id>/<arch>/<branch>/<commit>/files/...`), where `<apps>` is
/// `/var/lib/flatpak/app` or `<home>/.local/share/flatpak/app`. Any other
/// path, or one with a `..` component, has no id.
pub fn flatpak_app_id_from_path(path: &str, home: Option<&str>) -> Option<String> {
    if path.split('/').any(|part| part == "..") {
        return None;
    }
    let user_apps = home
        .filter(|h| h.starts_with('/'))
        .map(|h| format!("{}/{FLATPAK_USER_APPS}", h.trim_end_matches('/')));
    let rest = path.strip_prefix(FLATPAK_SYSTEM_APPS).or_else(|| {
        user_apps
            .as_deref()
            .and_then(|prefix| path.strip_prefix(prefix))
    })?;
    let id = rest.split('/').next()?;
    // An app id names a directory with content under it.
    (rest.len() > id.len() && looks_like_app_id(id)).then(|| id.to_string())
}

/// Whether `path` sits directly in a standard binary directory.
fn in_standard_bin_dir(path: &str) -> bool {
    path.rsplit_once('/')
        .is_some_and(|(dir, _)| BIN_DIRS.contains(&dir))
}

/// Read at most [`MAX_DESKTOP_BYTES`] of a regular file (following a
/// symlink); `None` for anything else.
fn read_desktop_file(file: &Path) -> Option<String> {
    if !std::fs::metadata(file).ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(file)
        .ok()?
        .take(MAX_DESKTOP_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Whether `icon` may be handed to the UI: an icon-theme name, or an absolute
/// path to an existing regular file under one of `roots` (canonical data dirs;
/// symlinks are resolved first so a link cannot point outside them).
fn icon_is_safe(icon: &str, roots: &[PathBuf]) -> bool {
    if icon.starts_with('/') {
        return std::fs::canonicalize(icon)
            .ok()
            .is_some_and(|real| real.is_file() && roots.iter().any(|root| real.starts_with(root)));
    }
    icon.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
}

/// Parsed desktop entries in search-directory order (earlier wins).
#[derive(Debug, Default)]
pub struct AppIconIndex {
    entries: Vec<DesktopEntry>,
    /// `$HOME`, for the per-user Flatpak install location.
    home: Option<String>,
}

impl AppIconIndex {
    /// Scan each directory (non-recursively, sorted by file name) for
    /// `.desktop` files; see the module docs for what is skipped. Unreadable
    /// or missing directories are skipped. The data dirs are each
    /// directory's parent (`.../share` for `.../share/applications`).
    pub fn scan(dirs: &[PathBuf]) -> Self {
        let roots: Vec<PathBuf> = dirs
            .iter()
            .filter_map(|d| d.parent())
            .filter_map(|d| std::fs::canonicalize(d).ok())
            .collect();
        let mut entries = Vec::new();
        let mut budget = MAX_DESKTOP_FILES;
        for dir in dirs {
            let Ok(read) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut files: Vec<PathBuf> = read
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "desktop"))
                .collect();
            files.sort();
            for file in files {
                if budget == 0 {
                    return Self {
                        entries,
                        home: None,
                    };
                }
                budget -= 1;
                let Some(stem) = file.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if let Some(entry) = read_desktop_file(&file)
                    .and_then(|body| parse_desktop(&body, stem))
                    .filter(|entry| icon_is_safe(&entry.icon, &roots))
                {
                    entries.push(entry);
                }
            }
        }
        Self {
            entries,
            home: None,
        }
    }

    /// The same index, resolving per-user Flatpak paths under `home`.
    #[must_use]
    pub fn with_home(mut self, home: Option<String>) -> Self {
        self.home = home;
        self
    }

    /// Icon for a process path, or [`FALLBACK_ICON`].
    pub fn icon_for(&self, process_path: &str) -> String {
        self.matching_entry(process_path)
            .map_or_else(|| FALLBACK_ICON.to_string(), |e| e.icon.clone())
    }

    /// Display name of the entry [`Self::icon_for`] would pick, if it has one.
    /// A label only: the matching rules are the icon's, so a name is as
    /// unverified as an icon is.
    pub fn name_for(&self, process_path: &str) -> Option<String> {
        self.matching_entry(process_path)
            .and_then(|e| e.name.clone())
    }

    fn matching_entry(&self, process_path: &str) -> Option<&DesktopEntry> {
        if process_path.is_empty() {
            return None;
        }
        if let Some(id) = flatpak_app_id_from_path(process_path, self.home.as_deref()) {
            if let Some(e) = self
                .entries
                .iter()
                .find(|e| e.flatpak_id.as_deref() == Some(id.as_str()))
            {
                return Some(e);
            }
        }
        let base = basename(process_path);
        // A name alone proves nothing for a program outside the bin dirs.
        let by_name = in_standard_bin_dir(process_path);
        let hit = |exact: bool| {
            self.entries.iter().find(|e| {
                e.programs.iter().any(|p| {
                    if exact {
                        p == process_path && process_path.starts_with('/')
                    } else {
                        basename(p) == base
                    }
                })
            })
        };
        hit(true).or_else(|| by_name.then(|| hit(false)).flatten())
    }
}

/// `.desktop` search directories: user data dir, `$XDG_DATA_DIRS`, the
/// system and Flatpak export locations, and the `/run/host` view of those.
pub fn search_dirs(env: impl Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let home = env("HOME").filter(|h| !h.is_empty());
    let mut bases: Vec<PathBuf> = Vec::new();
    // Inside a Flatpak `XDG_DATA_HOME` is the app's private dir, while the
    // host's `~/.local/share` is what the manifest mounts read-only: look in both.
    bases.extend(
        env("XDG_DATA_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from),
    );
    bases.extend(home.as_ref().map(|h| Path::new(h).join(".local/share")));
    let data_dirs = env("XDG_DATA_DIRS")
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    bases.extend(
        data_dirs
            .split(':')
            .filter(|d| !d.is_empty())
            .map(PathBuf::from),
    );
    bases.push(PathBuf::from("/usr/share"));
    bases.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    bases.extend(
        home.as_ref()
            .map(|h| Path::new(h).join(".local/share/flatpak/exports/share")),
    );
    let host: Vec<PathBuf> = [
        "/usr/local/share",
        "/usr/share",
        "/var/lib/flatpak/exports/share",
    ]
    .iter()
    .map(|b| Path::new("/run/host").join(b.trim_start_matches('/')))
    .collect();
    bases.extend(host);
    let mut dirs: Vec<PathBuf> = Vec::new();
    for dir in bases.into_iter().map(|b| b.join("applications")) {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// The host's index, once built (see [`prewarm`]).
static INDEX: OnceLock<AppIconIndex> = OnceLock::new();

/// Callbacks to run once the index is ready, so views that drew the generic
/// icon meanwhile can ask again.
#[derive(Default)]
struct ReadyHooks {
    ready: bool,
    hooks: Vec<Box<dyn FnOnce() + Send>>,
}

impl ReadyHooks {
    /// Run `hook` now if the index is ready, else when it becomes so.
    fn add(&mut self, hook: Box<dyn FnOnce() + Send>) {
        if self.ready {
            hook();
        } else {
            self.hooks.push(hook);
        }
    }

    fn mark_ready(&mut self) {
        self.ready = true;
        for hook in self.hooks.drain(..) {
            hook();
        }
    }
}

static READY_HOOKS: Mutex<ReadyHooks> = Mutex::new(ReadyHooks {
    ready: false,
    hooks: Vec::new(),
});

/// Run `hook` (on whatever thread builds the index: keep it short, e.g.
/// queue to the Qt thread) once the index is ready; now if it already is.
pub fn when_ready(hook: impl FnOnce() + Send + 'static) {
    READY_HOOKS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .add(Box::new(hook));
}

/// Start building the host's index on a background thread, once. Called at
/// startup so it is ready by the time rows are drawn.
pub fn prewarm() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        let spawned = std::thread::Builder::new()
            .name("app-icon-index".into())
            .spawn(|| {
                let env = |k: &str| std::env::var(k).ok();
                let index = AppIconIndex::scan(&search_dirs(env))
                    .with_home(env("HOME").filter(|h| !h.is_empty()));
                let _ = INDEX.set(index);
                READY_HOOKS
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .mark_ready();
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "app icon index not built; generic icons are used");
        }
    });
}

/// Icon name for a process path, using the host's `.desktop` files. Results
/// are cached per path once the index is built; before that (it is built off
/// the UI thread) the generic icon is answered and not cached.
pub fn icon_for_path(process_path: &str) -> String {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let Some(index) = INDEX.get() else {
        prewarm();
        return FALLBACK_ICON.to_string();
    };
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(process_path).cloned()) {
        return hit;
    }
    let icon = index.icon_for(process_path);
    if let Ok(mut c) = cache.lock() {
        insert_capped(&mut c, MAX_CACHED_PATHS, process_path, icon.clone());
    }
    icon
}

/// `.desktop` display name for a process path, from the same index and match
/// rules as [`icon_for_path`]. `None` when nothing matches or while the index
/// is still being built (not cached then); cached per path afterwards. A
/// label only, never proof of identity (see [`crate::app_names`]).
pub fn name_for_path(process_path: &str) -> Option<String> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let Some(index) = INDEX.get() else {
        prewarm();
        return None;
    };
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(process_path).cloned()) {
        return hit;
    }
    let name = index.name_for(process_path);
    if let Ok(mut c) = cache.lock() {
        insert_capped(&mut c, MAX_CACHED_PATHS, process_path, name.clone());
    }
    name
}

#[cfg(test)]
mod tests;

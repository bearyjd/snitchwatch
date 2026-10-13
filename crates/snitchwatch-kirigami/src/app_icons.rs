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

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once, OnceLock};

/// Generic icon name used when no `.desktop` entry matches.
pub const FALLBACK_ICON: &str = "application-x-executable";

/// Directories whose programs may be matched to a `.desktop` entry by
/// basename alone.
const BIN_DIRS: [&str; 7] = [
    "/usr/bin",
    "/usr/sbin",
    "/usr/local/bin",
    "/bin",
    "/sbin",
    "/usr/libexec",
    "/app/bin",
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
        if process_path.is_empty() {
            return FALLBACK_ICON.to_string();
        }
        if let Some(id) = flatpak_app_id_from_path(process_path, self.home.as_deref()) {
            if let Some(e) = self
                .entries
                .iter()
                .find(|e| e.flatpak_id.as_deref() == Some(id.as_str()))
            {
                return e.icon.clone();
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
        hit(true)
            .or_else(|| by_name.then(|| hit(false)).flatten())
            .map_or_else(|| FALLBACK_ICON.to_string(), |e| e.icon.clone())
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
        c.insert(process_path.to_string(), icon.clone());
    }
    icon
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn entry(exec: &str, icon: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName=X\nExec={exec}\nIcon={icon}\n")
    }

    #[test]
    fn exec_tokens_handle_quotes_and_escapes() {
        assert_eq!(
            exec_tokens(r#""/opt/My App/run" --flag "a b" c\ d"#),
            ["/opt/My App/run", "--flag", "a b", "c d"]
        );
    }

    #[test]
    fn exec_program_skips_env_and_assignments() {
        let t = exec_tokens("env FOO=1 BAR=2 /usr/bin/thing %U");
        assert_eq!(exec_program(&t), Some("/usr/bin/thing"));
    }

    #[test]
    fn parses_exec_basename_and_icon() {
        let e = parse_desktop(&entry("firefox %u", "firefox"), "firefox").unwrap();
        assert_eq!(e.programs, ["firefox"]);
        assert_eq!(e.icon, "firefox");
        assert_eq!(e.flatpak_id, None);
    }

    #[test]
    fn ignores_other_groups_and_non_applications() {
        let body = "[Desktop Entry]\nType=Application\nExec=a\nIcon=right\n\
                    [Desktop Action x]\nExec=b\nIcon=wrong\n";
        assert_eq!(parse_desktop(body, "a").unwrap().icon, "right");
        let link = "[Desktop Entry]\nType=Link\nExec=a\nIcon=i\n";
        assert!(parse_desktop(link, "a").is_none());
        assert!(parse_desktop("[Desktop Entry]\nType=Application\nExec=a\n", "a").is_none());
    }

    #[test]
    fn flatpak_run_exec_yields_id_not_flatpak_program() {
        let e = parse_desktop(
            &entry(
                "/usr/bin/flatpak run --branch=stable --command=fx org.mozilla.firefox @@u %u @@",
                "org.mozilla.firefox",
            ),
            "org.mozilla.firefox",
        )
        .unwrap();
        assert_eq!(e.flatpak_id.as_deref(), Some("org.mozilla.firefox"));
        assert!(e.programs.is_empty());
    }

    #[test]
    fn x_flatpak_key_gives_id() {
        let body = "[Desktop Entry]\nType=Application\nExec=thing\nIcon=i\nX-Flatpak=org.a.B\n";
        assert_eq!(
            parse_desktop(body, "other").unwrap().flatpak_id.as_deref(),
            Some("org.a.B")
        );
    }

    #[test]
    fn flatpak_id_from_host_install_path() {
        let p = "/var/lib/flatpak/app/org.mozilla.firefox/x86_64/stable/abc123/files/lib/firefox/firefox";
        assert_eq!(
            flatpak_app_id_from_path(p, None).as_deref(),
            Some("org.mozilla.firefox")
        );
        let u = "/home/u/.local/share/flatpak/app/com.valve.Steam/x86_64/stable/c/files/bin/steam";
        assert_eq!(
            flatpak_app_id_from_path(u, Some("/home/u")).as_deref(),
            Some("com.valve.Steam")
        );
        assert_eq!(flatpak_app_id_from_path("/app/bin/foo", None), None);
        assert_eq!(flatpak_app_id_from_path("/usr/bin/bwrap", None), None);
    }

    #[test]
    fn flatpak_id_only_from_the_real_install_locations() {
        let home = Some("/home/u");
        for p in [
            "/home/u/flatpak/app/org.mozilla.firefox/evil",
            "/tmp/flatpak/app/org.mozilla.firefox/x86_64/stable/c/files/bin/x",
            "/home/other/.local/share/flatpak/app/org.mozilla.firefox/x/y",
            "/var/lib/flatpak/app/org.mozilla.firefox",
            "/var/lib/flatpak/app/org.mozilla.firefox/../../../../tmp/x",
            "/var/lib/flatpak/app/not-an-id/x",
        ] {
            assert_eq!(flatpak_app_id_from_path(p, home), None, "{p}");
        }
        // The per-user location needs a known absolute $HOME.
        let u = "/home/u/.local/share/flatpak/app/com.valve.Steam/x/y";
        assert_eq!(flatpak_app_id_from_path(u, None), None);
        assert_eq!(flatpak_app_id_from_path(u, Some("home/u")), None);
    }

    #[test]
    fn index_matches_absolute_then_basename_then_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "a.desktop",
            &entry("/usr/bin/firefox %u", "firefox-icon"),
        );
        write(dir.path(), "b.desktop", &entry("/opt/x/tool", "tool-icon"));
        write(
            dir.path(),
            "c.desktop",
            &entry("env A=1 syncthing-gtk", "st-icon"),
        );
        write(dir.path(), "notes.txt", "ignored");
        let idx = AppIconIndex::scan(&[dir.path().to_path_buf()]);
        assert_eq!(idx.icon_for("/usr/bin/firefox"), "firefox-icon");
        assert_eq!(idx.icon_for("/usr/libexec/firefox"), "firefox-icon");
        assert_eq!(idx.icon_for("/opt/x/tool"), "tool-icon");
        assert_eq!(idx.icon_for("/usr/bin/syncthing-gtk"), "st-icon");
        assert_eq!(idx.icon_for("/usr/sbin/unknownd"), FALLBACK_ICON);
        assert_eq!(idx.icon_for(""), FALLBACK_ICON);
    }

    #[test]
    fn a_name_alone_matches_only_in_a_standard_bin_dir() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "a.desktop",
            &entry("firefox %u", "firefox-icon"),
        );
        let idx = AppIconIndex::scan(&[dir.path().to_path_buf()]);
        for dir in BIN_DIRS {
            assert_eq!(idx.icon_for(&format!("{dir}/firefox")), "firefox-icon");
        }
        for p in [
            "/tmp/x/firefox",
            "/home/u/Downloads/firefox",
            "/usr/lib64/firefox/firefox",
            "/usr/bin/sub/firefox",
            "firefox",
        ] {
            assert_eq!(idx.icon_for(p), FALLBACK_ICON, "{p}");
        }
    }

    #[test]
    fn a_made_up_flatpak_path_gets_the_generic_icon() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "org.mozilla.firefox.desktop",
            &entry("/usr/bin/flatpak run org.mozilla.firefox", "ff-icon"),
        );
        let idx =
            AppIconIndex::scan(&[dir.path().to_path_buf()]).with_home(Some("/home/u".to_string()));
        assert_eq!(
            idx.icon_for("/home/u/flatpak/app/org.mozilla.firefox/evil"),
            FALLBACK_ICON
        );
        assert_eq!(
            idx.icon_for("/tmp/flatpak/app/org.mozilla.firefox/x/y/files/bin/f"),
            FALLBACK_ICON
        );
        assert_eq!(
            idx.icon_for("/home/u/.local/share/flatpak/app/org.mozilla.firefox/x/y/files/bin/f"),
            "ff-icon"
        );
    }

    #[test]
    fn scan_skips_non_files_and_oversized_files_reads_symlinked_files() {
        let (apps, other) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // A directory named like a desktop file.
        std::fs::create_dir(apps.path().join("dir.desktop")).unwrap();
        // A symlink to a regular file is fine.
        write(
            other.path(),
            "real.desktop",
            &entry("/usr/bin/linked", "linked-icon"),
        );
        std::os::unix::fs::symlink(
            other.path().join("real.desktop"),
            apps.path().join("link.desktop"),
        )
        .unwrap();
        // A dangling symlink is skipped.
        std::os::unix::fs::symlink(
            other.path().join("missing.desktop"),
            apps.path().join("dangling.desktop"),
        )
        .unwrap();
        // The program line sits past the read cap, so the entry is unusable.
        let mut big = "[Desktop Entry]\nType=Application\nIcon=big-icon\n".to_string();
        big.push_str(&"# padding\n".repeat(MAX_DESKTOP_BYTES as usize / 8));
        big.push_str("Exec=/usr/bin/big\n");
        write(apps.path(), "big.desktop", &big);
        let idx = AppIconIndex::scan(&[apps.path().to_path_buf()]);
        assert_eq!(idx.icon_for("/usr/bin/linked"), "linked-icon");
        assert_eq!(idx.icon_for("/usr/bin/big"), FALLBACK_ICON);
    }

    #[test]
    fn only_theme_names_and_files_under_the_data_dirs_are_icons() {
        let share = tempfile::tempdir().unwrap();
        let apps = share.path().join("applications");
        std::fs::create_dir(&apps).unwrap();
        let inside = share.path().join("pixmaps-app.png");
        std::fs::write(&inside, b"png").unwrap();
        let outside = tempfile::tempdir().unwrap();
        let elsewhere = outside.path().join("evil.png");
        std::fs::write(&elsewhere, b"png").unwrap();
        // A link inside the data dir that points outside it.
        let link = share.path().join("link.png");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        let cases = [
            ("theme-icon", "/usr/bin/a", true),
            ("org.app.Name", "/usr/bin/b", true),
            (inside.to_str().unwrap(), "/usr/bin/c", true),
            (elsewhere.to_str().unwrap(), "/usr/bin/d", false),
            (link.to_str().unwrap(), "/usr/bin/e", false),
            ("/nonexistent/under/nothing.png", "/usr/bin/f", false),
            ("https://example.com/i.png", "/usr/bin/g", false),
            ("file:///etc/passwd", "/usr/bin/h", false),
            ("relative/icon.png", "/usr/bin/i", false),
            ("../x", "/usr/bin/j", false),
        ];
        for (n, (icon, program, _)) in cases.iter().enumerate() {
            write(&apps, &format!("{n}.desktop"), &entry(program, icon));
        }
        let idx = AppIconIndex::scan(&[apps]);
        for (icon, program, kept) in cases {
            let got = idx.icon_for(program);
            if kept {
                assert_eq!(got, icon, "{icon}");
            } else {
                assert_eq!(got, FALLBACK_ICON, "{icon} must be rejected");
            }
        }
    }

    #[test]
    fn earlier_directory_wins() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write(a.path(), "x.desktop", &entry("prog", "user-icon"));
        write(b.path(), "x.desktop", &entry("prog", "system-icon"));
        let idx = AppIconIndex::scan(&[a.path().into(), b.path().into()]);
        assert_eq!(idx.icon_for("/usr/bin/prog"), "user-icon");
    }

    #[test]
    fn flatpak_install_path_matches_by_app_id_only() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "org.mozilla.firefox.desktop",
            &entry(
                "/usr/bin/flatpak run org.mozilla.firefox",
                "org.mozilla.firefox",
            ),
        );
        let idx = AppIconIndex::scan(&[dir.path().to_path_buf()]);
        let p = "/var/lib/flatpak/app/org.mozilla.firefox/x86_64/stable/h/files/bin/firefox";
        assert_eq!(idx.icon_for(p), "org.mozilla.firefox");
        // No derivable id: generic icon, and `flatpak` itself never matches.
        assert_eq!(idx.icon_for("/app/bin/firefox"), FALLBACK_ICON);
        assert_eq!(idx.icon_for("/usr/bin/flatpak"), FALLBACK_ICON);
        assert_eq!(idx.icon_for("/usr/bin/bwrap"), FALLBACK_ICON);
    }

    #[test]
    fn search_dirs_orders_user_then_xdg_then_flatpak_then_host() {
        let env = |k: &str| match k {
            "HOME" => Some("/home/u".to_string()),
            "XDG_DATA_DIRS" => Some("/opt/share:/usr/share".to_string()),
            _ => None,
        };
        let dirs = search_dirs(env);
        let s: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        assert_eq!(s[0], "/home/u/.local/share/applications");
        assert_eq!(s[1], "/opt/share/applications");
        assert!(s.contains(&"/var/lib/flatpak/exports/share/applications".to_string()));
        assert!(s.contains(&"/home/u/.local/share/flatpak/exports/share/applications".to_string()));
        assert!(s.contains(&"/run/host/usr/share/applications".to_string()));
        assert_eq!(
            s.iter().filter(|d| *d == "/usr/share/applications").count(),
            1
        );
    }

    #[test]
    fn xdg_data_home_comes_first_then_the_home_data_dir() {
        let env = |k: &str| match k {
            "HOME" => Some("/home/u".to_string()),
            "XDG_DATA_HOME" => Some("/data".to_string()),
            _ => None,
        };
        let dirs = search_dirs(env);
        assert_eq!(dirs[0], PathBuf::from("/data/applications"));
        assert_eq!(dirs[1], PathBuf::from("/home/u/.local/share/applications"));
    }
}

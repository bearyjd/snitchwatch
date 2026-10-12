//! Process path -> application icon name, via installed `.desktop` files.
//!
//! A `.desktop` entry matches a process when its `TryExec`/`Exec` program (the
//! first real token) is the process path, or has the same basename. Flatpak
//! apps are matched by app id, and only when the id is derivable: a host-side
//! path under `.../flatpak/app/<app-id>/...`, matched against `X-Flatpak=`,
//! `Exec=flatpak run ... <app-id>` or the desktop file stem. A bare `/app/...`
//! path or a `bwrap` process carries no id, so those fall back to
//! [`FALLBACK_ICON`].
//!
//! Parsing and matching are pure ([`parse_desktop`], [`AppIconIndex`]); only
//! [`icon_for_path`] touches the host (search dirs, a lazily built index and a
//! per-path cache). The index is built once per process, so apps installed
//! while the GUI runs show the generic icon until restart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Generic icon name used when no `.desktop` entry matches.
pub const FALLBACK_ICON: &str = "application-x-executable";

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
/// (`.../flatpak/app/<app-id>/<arch>/<branch>/<commit>/files/...`).
pub fn flatpak_app_id_from_path(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    parts
        .windows(3)
        .find(|w| w[0] == "flatpak" && w[1] == "app" && looks_like_app_id(w[2]))
        .map(|w| w[2].to_string())
}

/// Parsed desktop entries in search-directory order (earlier wins).
#[derive(Debug, Default)]
pub struct AppIconIndex {
    entries: Vec<DesktopEntry>,
}

impl AppIconIndex {
    /// Scan each directory (non-recursively, sorted by file name) for
    /// `.desktop` files. Unreadable or missing directories are skipped.
    pub fn scan(dirs: &[PathBuf]) -> Self {
        let mut entries = Vec::new();
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
                let Some(stem) = file.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if let Some(entry) = std::fs::read_to_string(&file)
                    .ok()
                    .and_then(|body| parse_desktop(&body, stem))
                {
                    entries.push(entry);
                }
            }
        }
        Self { entries }
    }

    /// Icon for a process path, or [`FALLBACK_ICON`].
    pub fn icon_for(&self, process_path: &str) -> String {
        if process_path.is_empty() {
            return FALLBACK_ICON.to_string();
        }
        if let Some(id) = flatpak_app_id_from_path(process_path) {
            if let Some(e) = self
                .entries
                .iter()
                .find(|e| e.flatpak_id.as_deref() == Some(id.as_str()))
            {
                return e.icon.clone();
            }
        }
        let base = basename(process_path);
        let hit = |exact: bool| {
            self.entries.iter().find(|e| {
                e.programs.iter().any(|p| {
                    if exact {
                        p == process_path
                    } else {
                        basename(p) == base
                    }
                })
            })
        };
        hit(true)
            .or_else(|| hit(false))
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

/// Icon name for a process path, using the host's `.desktop` files. Results
/// are cached per path; the empty string (no path known) is never cached
/// differently from any other input.
pub fn icon_for_path(process_path: &str) -> String {
    static INDEX: OnceLock<AppIconIndex> = OnceLock::new();
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(process_path).cloned()) {
        return hit;
    }
    let index = INDEX.get_or_init(|| AppIconIndex::scan(&search_dirs(|k| std::env::var(k).ok())));
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
            flatpak_app_id_from_path(p).as_deref(),
            Some("org.mozilla.firefox")
        );
        let u = "/home/u/.local/share/flatpak/app/com.valve.Steam/x86_64/stable/c/files/bin/steam";
        assert_eq!(
            flatpak_app_id_from_path(u).as_deref(),
            Some("com.valve.Steam")
        );
        assert_eq!(flatpak_app_id_from_path("/app/bin/foo"), None);
        assert_eq!(flatpak_app_id_from_path("/usr/bin/bwrap"), None);
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
        assert_eq!(idx.icon_for("/usr/lib64/firefox/firefox"), "firefox-icon");
        assert_eq!(idx.icon_for("/opt/x/tool"), "tool-icon");
        assert_eq!(idx.icon_for("/usr/bin/syncthing-gtk"), "st-icon");
        assert_eq!(idx.icon_for("/usr/sbin/unknownd"), FALLBACK_ICON);
        assert_eq!(idx.icon_for(""), FALLBACK_ICON);
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

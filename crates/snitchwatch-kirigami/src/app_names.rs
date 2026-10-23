//! Process path -> display name, and how far that name can be trusted.
//!
//! GUI side only, pure (no I/O): the caller looks the `.desktop` name up
//! ([`crate::app_icons::AppIconIndex::name_for`]) and passes it in. A program
//! controls its own path, and a user-writable desktop-file directory can name
//! any binary, so no label here is proof of identity: [`NameSource`] says
//! where a label came from and the UI must show the real path beside any
//! label that [`needs_marker`](ProgramName::needs_marker).

/// Longest display name, in characters.
const MAX_NAME_CHARS: usize = 80;
/// Label used when sanitizing leaves nothing.
const UNNAMED: &str = "<unnamed>";
/// Most consecutive combining marks kept after one base character.
const MAX_COMBINING: usize = 2;

/// Path components that never name an app in `<app>/versions/<semver>`.
const NOT_AN_APP: [&str; 8] = [
    "bin", "sbin", "lib", "lib64", "usr", "opt", "local", "share",
];
/// Longest `<app>` component accepted by the versions heuristic.
const MAX_APP_CHARS: usize = 40;
/// Basename suffixes that mark a CPU-feature build of the same program.
const CPU_SUFFIXES: [&str; 5] = [".avx", ".avx2", ".avx512", ".sse2", ".sse4"];

/// Where a label came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameSource {
    /// A `.desktop` file's `Name=`.
    DesktopFile,
    /// Guessed from the shape of the path.
    Heuristic,
    /// The raw process name.
    Raw,
}

/// A display label and its provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramName {
    pub label: String,
    pub source: NameSource,
    /// A `Raw` label that differs from the raw process name (sanitized,
    /// capped or replaced by `<unnamed>`). Always false for other sources.
    pub altered: bool,
}

impl ProgramName {
    /// Whether the UI must flag the label as unverified: it came from a
    /// forgeable source, or it is not the raw file name.
    pub fn needs_marker(&self) -> bool {
        self.source != NameSource::Raw || self.altered
    }
}

/// Characters removed from display names: control, format, invisible, filler
/// and variation-selector characters (explicit ranges; no Unicode tables).
pub(crate) fn is_stripped(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{AD}' | '\u{34F}' | '\u{600}'..='\u{605}' | '\u{61C}' | '\u{6DD}' | '\u{70F}'
            | '\u{115F}' | '\u{1160}' | '\u{180E}' | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}'
            | '\u{FFA0}' | '\u{2800}' | '\u{17B4}' | '\u{17B5}' | '\u{890}'
            | '\u{891}' | '\u{8E2}' | '\u{FFF9}'..='\u{FFFB}' | '\u{E0000}'..='\u{E007F}'
            | '\u{E0100}'..='\u{E01EF}')
}

fn is_combining(c: char) -> bool {
    matches!(c,
        '\u{300}'..='\u{36F}' | '\u{1AB0}'..='\u{1AFF}' | '\u{1DC0}'..='\u{1DFF}'
        | '\u{20D0}'..='\u{20FF}' | '\u{FE20}'..='\u{FE2F}')
}

/// Turn every whitespace character (controls like `\n` included) into a
/// space, strip the other unsafe characters, collapse space runs, keep
/// at most [`MAX_COMBINING`] consecutive combining marks, trim, cap at
/// [`MAX_NAME_CHARS`] characters and trim again. May return an empty string.
pub(crate) fn clean_display(s: &str) -> String {
    let mut out = String::new();
    let mut marks = 0;
    for c in s.chars() {
        // Whitespace first: tab, newline and U+2028 separate words, so they
        // become a space rather than being deleted and fusing the words.
        let c = if c.is_whitespace() {
            ' '
        } else if is_stripped(c) {
            continue;
        } else {
            c
        };
        marks = if is_combining(c) { marks + 1 } else { 0 };
        if marks > MAX_COMBINING || (c == ' ' && out.ends_with(' ')) {
            continue;
        }
        out.push(c);
    }
    let capped: String = out.trim().chars().take(MAX_NAME_CHARS).collect();
    capped.trim().to_string()
}

/// [`clean_display`], or `<unnamed>` when nothing is left.
pub fn sanitize_display(s: &str) -> String {
    let cleaned = clean_display(s);
    if cleaned.is_empty() {
        UNNAMED.to_string()
    } else {
        cleaned
    }
}

/// Name for a connection's program: the caller's `.desktop` name, else a path
/// heuristic, else the sanitized raw process name.
pub fn friendly_name(
    process_path: Option<&str>,
    raw: &str,
    desktop_name: Option<&str>,
) -> ProgramName {
    let derived = |label: String, source| ProgramName {
        label,
        source,
        altered: false,
    };
    // A desktop name that cleans to nothing is no name: fall through.
    if let Some(label) = desktop_name.map(clean_display).filter(|l| !l.is_empty()) {
        return derived(label, NameSource::DesktopFile);
    }
    if let Some(label) = process_path.and_then(heuristic_label) {
        return derived(sanitize_display(&label), NameSource::Heuristic);
    }
    let label = sanitize_display(raw);
    ProgramName {
        altered: label != raw,
        label,
        source: NameSource::Raw,
    }
}

fn heuristic_label(path: &str) -> Option<String> {
    if path.chars().any(char::is_control) {
        return None;
    }
    versions_label(path).or_else(|| cpu_suffix_label(path))
}

/// `<...>/<app>/versions/<semver>` -> `<app> <semver>`.
fn versions_label(path: &str) -> Option<String> {
    let (app, version) = versions_parts(path)?;
    Some(format!("{app} {version}"))
}

/// `<app>` and `<semver>` of a `<...>/<app>/versions/<semver>` path.
fn versions_parts(path: &str) -> Option<(&str, &str)> {
    let mut parts = path.rsplit('/');
    let (version, dir, app) = (parts.next()?, parts.next()?, parts.next()?);
    (dir == "versions" && is_semver_like(version) && is_app_component(app))
        .then_some((app, version))
}

/// A path the versions heuristic matches, split: `<dir>/<app>/versions/<version>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VersionedBinary<'a> {
    /// Everything before `/<app>/versions/<version>` (empty for a file at
    /// `/<app>/versions/<version>`).
    pub dir: &'a str,
    pub app: &'a str,
    pub version: &'a str,
}

/// Splits `path` when the versions heuristic matches it and it is absolute,
/// without control characters (as [`friendly_name`] requires). A suggestion
/// shape only, never proof of what a program is.
pub(crate) fn versioned_binary(path: &str) -> Option<VersionedBinary<'_>> {
    if path.chars().any(char::is_control) {
        return None;
    }
    let (app, version) = versions_parts(path)?;
    let tail_len = "/".len() + app.len() + "/versions/".len() + version.len();
    let dir = path.get(..path.len().checked_sub(tail_len)?)?;
    path.starts_with('/')
        .then_some(VersionedBinary { dir, app, version })
}

fn is_app_component(app: &str) -> bool {
    (1..=MAX_APP_CHARS).contains(&app.chars().count())
        && app != "."
        && app != ".."
        && !NOT_AN_APP.contains(&app)
        && app
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// 2 to 4 dot-separated numbers, then an optional `-`/`+` suffix of
/// `[A-Za-z0-9.]`.
fn is_semver_like(s: &str) -> bool {
    let (core, suffix) = match s.find(['-', '+']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let numbers: Vec<&str> = core.split('.').collect();
    (2..=4).contains(&numbers.len())
        && numbers
            .iter()
            .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        && suffix.is_none_or(|x| {
            !x.is_empty() && x.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
        })
}

/// `pasta.avx2` -> `pasta`.
fn cpu_suffix_label(path: &str) -> Option<String> {
    let base = path.rsplit('/').next()?;
    let lower = base.to_ascii_lowercase();
    let suffix = CPU_SUFFIXES.iter().find(|s| lower.ends_with(**s))?;
    // ASCII suffix, so the byte offset is a char boundary in `base` too.
    let stem = &base[..base.len() - suffix.len()];
    (!stem.is_empty()).then(|| stem.to_string())
}

#[cfg(test)]
mod tests;

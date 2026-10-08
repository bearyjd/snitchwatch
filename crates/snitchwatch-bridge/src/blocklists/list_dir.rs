//! The directories opensnitchd reads a blocklist's hosts from (issue #45 PR B).
//!
//! # Path contract
//!
//! This is what a confined opensnitchd must be able to read, and all a
//! confinement policy has to allow for Snitchwatch's `lists.*` rules:
//!
//! - **System bridge only:** `/var/lib/snitchwatch/blocklists` (the state
//!   directory is checked to be `snitchwatch:snitchwatch`, mode 0700). A
//!   per-user bridge writes no list file and installs no rule: root would
//!   read files any of the user's processes could replace.
//! - **Pinned root:** the bridge sends a `lists` rule only for `data` that
//!   is exactly `<root>/<list>/<kind>` under the one root its sink pinned,
//!   and the root path is plain UTF-8 without `*?[]\` (the daemon globs
//!   `<data>/*.*`).
//! - **Per list:** `<root>/<list>/domains/domains.list` and
//!   `<root>/<list>/ips/ips.list`. `<list>` matches
//!   `^[A-Za-z0-9_-]{1,81}$`, or `^[A-Za-z0-9_-]{1,40}\.[0-9a-f]{16}$` for
//!   an id that needed hashing ([`IdComponent`]). Every directory is a real
//!   directory (never a symlink) owned by the bridge's user, mode 0700;
//!   every list file a regular file, mode 0600. The only other entry is a
//!   transient hidden `.<file>.tmp`, renamed over the list file.
//! - **Lines:** `0.0.0.0 <host>\n` (`<host>`: `[a-z0-9.-]`, at most 253
//!   bytes) in `domains.list`; `<IPv4 dotted quad>\n` in `ips.list`, never
//!   a loopback, `0.0.0.0/8`, private, link-local, CGNAT, multicast or
//!   reserved address ([`is_blockable_ip`]).
//! - **Caps:** at most [`MAX_LIST_LINES`] lines and [`MAX_LIST_FILE_BYTES`]
//!   bytes per file; at most `AGGREGATE_MAX_HOSTS` (2,000,000) hosts summed
//!   over every installed list (later subscriptions past it get no files and
//!   no rule); the bridge accepts at most `MAX_SUBSCRIPTIONS` (32)
//!   subscriptions. An unchanged file is never rewritten.
//! - **Rules:** `z00-blocklist:<list>:domains` with operator
//!   `{"type":"lists","operand":"lists.domains","data":"<root>/<list>/domains","sensitive":false}`
//!   and `z00-blocklist:<list>:ips` with `lists.ips` and `…/ips`; `data` is
//!   absolute with no trailing slash, action `deny`, duration `always`,
//!   `precedence` false (see [`crate::blocklists::materializer`]).
//!
//! The daemon runs as root and reads these 0700 directories through its DAC
//! override (`CAP_DAC_READ_SEARCH`/`CAP_DAC_OVERRIDE`); a unit that drops
//! those, or SELinux, makes it load 0 entries without any error back to the
//! bridge.
//!
//! # Not following attacker-influenced paths
//!
//! Every directory is checked with `lstat` (a symlink or non-directory is
//! refused, ownership must be the bridge's user) before anything is written
//! under it, and list files are written as a new temp file
//! (`O_CREAT|O_EXCL|O_NOFOLLOW`) renamed over the old one, so a planted link
//! is replaced, never written through. Only a process running as the
//! bridge's own user (or root) could race these checks, because the root is
//! 0700.

use std::fs;
use std::io::{self, BufWriter, Write};
use std::net::Ipv4Addr;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::blocklists::fetcher::MAX_BODY_BYTES;
use crate::blocklists::format::MAX_ENTRIES;
use crate::blocklists::materializer::ListKind;

/// The directory under the state directory that holds every list.
pub const LISTS_DIR_NAME: &str = "blocklists";
/// Longest list directory name (`derive_id`'s 64-character stem + `-` + 16
/// hex).
pub const MAX_ID_COMPONENT_BYTES: usize = 81;
/// Most lines in one list file: PR A's per-list entry cap.
pub const MAX_LIST_LINES: usize = MAX_ENTRIES;
/// Most bytes in one list file: every host came from a body of at most
/// `MAX_BODY_BYTES`, plus at most 9 bytes of framing (`0.0.0.0 `, `\n`) for
/// each of at most `MAX_ENTRIES` lines.
pub const MAX_LIST_FILE_BYTES: u64 = MAX_BODY_BYTES + 9 * MAX_ENTRIES as u64;

const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
/// Readable part kept in a hashed [`IdComponent`].
const HASHED_STEM_CHARS: usize = 40;
const LONGEST_HOST: usize = 253;

/// A subscription id as one safe path component and rule-name segment.
///
/// An id that is already `[A-Za-z0-9_-]{1,81}` (every id `derive_id` makes)
/// is used as is. Anything else (an id read from a database something else
/// wrote) becomes `<cleaned stem>.<16 hex of SHA-256(id)>`; the dot never
/// occurs in a plain id, so the mapping stays one-to-one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IdComponent(String);

impl IdComponent {
    pub fn from_id(id: &str) -> Self {
        if is_plain(id) {
            return Self(id.to_string());
        }
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(id.as_bytes());
        let hash: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        let mut stem: String = id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(HASHED_STEM_CHARS)
            .collect();
        if stem.is_empty() {
            stem.push_str("list");
        }
        Self(format!("{stem}.{hash}"))
    }

    /// A directory or rule-name segment that [`from_id`](Self::from_id)
    /// could have produced, or `None`.
    pub fn parse(name: &str) -> Option<Self> {
        if is_plain(name) {
            return Some(Self(name.to_string()));
        }
        let (stem, hash) = name.split_once('.')?;
        let hashed = is_plain(stem)
            && stem.len() <= HASHED_STEM_CHARS
            && hash.len() == 16
            && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        hashed.then(|| Self(name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for IdComponent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_plain(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_COMPONENT_BYTES
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A list's hosts split by the rule that can match them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ListEntries {
    /// Lowercase host names `lists.domains` can match.
    pub domains: Vec<String>,
    /// IPv4 addresses in `DstIP.String()` form, for `lists.ips`.
    pub ips: Vec<String>,
}

impl ListEntries {
    pub fn get(&self, kind: ListKind) -> &[String] {
        match kind {
            ListKind::Domains => &self.domains,
            ListKind::Ips => &self.ips,
        }
    }
}

/// Split downloaded hosts by kind. An IPv4 literal goes to `ips` only if
/// [`is_blockable_ip`]; anything that isn't a writable host name is
/// dropped.
pub fn classify(hosts: Vec<String>) -> ListEntries {
    let mut entries = ListEntries::default();
    for host in hosts {
        match host.parse::<Ipv4Addr>() {
            Ok(ip) if is_blockable_ip(ip) => entries.ips.push(ip.to_string()),
            Ok(_) => {}
            Err(_) if is_list_host(&host) => entries.domains.push(host),
            Err(_) => {}
        }
    }
    entries
}

/// A list "wins" over every allow, so a hostile one must not cut the
/// machine off from itself or its own networks (the gateway, local DNS):
/// loopback, `0.0.0.0/8`, private (RFC 1918), link-local, CGNAT
/// (`100.64.0.0/10`), multicast and reserved (`240.0.0.0/4`, broadcast)
/// addresses are never blocked.
pub fn is_blockable_ip(ip: Ipv4Addr) -> bool {
    let [first, second, ..] = ip.octets();
    let cgnat = first == 100 && (64..128).contains(&second);
    !(ip.is_loopback()
        || first == 0
        || ip.is_private()
        || ip.is_link_local()
        || cgnat
        || ip.is_multicast()
        || first >= 240)
}

/// A host a `0.0.0.0 <host>` line can carry and the daemon keeps: lowercase
/// (it lowercases `DstHost` before the lookup), dotted like every host the
/// parser accepts, no whitespace, no leading dot, and none of the names
/// `filterDomains` drops.
fn is_list_host(host: &str) -> bool {
    host.len() <= LONGEST_HOST
        && host.contains('.')
        && !host.starts_with('.')
        && host != "localhost.localdomain"
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
}

/// `<state>/blocklists`, owned by the bridge's user, mode 0700.
#[derive(Debug, Clone)]
pub struct ListDir {
    root: PathBuf,
    owner: u32,
    max_lines: usize,
    max_bytes: u64,
}

impl ListDir {
    /// Create (0700) or check `<state>/blocklists`. `state` must already be
    /// canonical, as `resolve_storage` makes it; nothing here canonicalizes,
    /// which would follow links.
    ///
    /// The daemon reads `<data>/*.*` as a glob, so a state path that isn't
    /// UTF-8 or holds a glob metacharacter (`*?[]\`; an unclosed `[` can
    /// hang the daemon's rule loader) is refused.
    pub fn open(state: &Path) -> io::Result<Self> {
        let glob_safe = state.is_absolute()
            && state
                .to_str()
                .is_some_and(|path| !path.contains(['*', '?', '[', ']', '\\']));
        if !glob_safe {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the state directory's path isn't plain UTF-8 without *, ?, [, ] or \\",
            ));
        }
        let dir = Self {
            root: state.join(LISTS_DIR_NAME),
            owner: effective_uid(),
            max_lines: MAX_LIST_LINES,
            max_bytes: MAX_LIST_FILE_BYTES,
        };
        dir.ensure_private_dir(&dir.root)?;
        Ok(dir)
    }

    /// Lower caps, for tests.
    pub fn with_caps(mut self, max_lines: usize, max_bytes: u64) -> Self {
        self.max_lines = max_lines;
        self.max_bytes = max_bytes;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/<list>`: everything one subscription owns.
    pub fn list_dir(&self, list: &IdComponent) -> PathBuf {
        self.root.join(list.as_str())
    }

    /// `<root>/<list>/<kind>`: the `data` of the kind's rule.
    pub fn kind_dir(&self, list: &IdComponent, kind: ListKind) -> PathBuf {
        self.list_dir(list).join(kind.dir_name())
    }

    /// Replace the kind's list file with `entries`: a new 0600 temp file,
    /// `fsync`, `rename`, then `fsync` of the directory. Fails, leaving the
    /// previous file, over a cap or for an entry the line format can't hold.
    ///
    /// A file that already holds exactly these lines (same length and
    /// SHA-256, a regular 0600 file of ours) is left alone: the daemon
    /// re-reads every list whenever any file's mtime changes. Returns
    /// whether the file was written.
    pub fn write_list(
        &self,
        list: &IdComponent,
        kind: ListKind,
        entries: &[String],
    ) -> io::Result<bool> {
        if entries.len() > self.max_lines {
            return Err(too_large(format!(
                "{} entries; the limit is {}",
                entries.len(),
                self.max_lines
            )));
        }
        let dir = self.kind_dir(list, kind);
        for path in [&self.root, &self.list_dir(list), &dir] {
            self.ensure_private_dir(path)?;
        }
        let mut wanted = Digest::default();
        self.write_lines(&mut wanted, kind, entries)?;
        if self.digest_of(&dir.join(kind.file_name())) == Some(wanted) {
            return Ok(false);
        }
        let temp = dir.join(format!(".{}.tmp", kind.file_name()));
        match fs::remove_file(&temp) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temp)?;
        let written = file
            .set_permissions(fs::Permissions::from_mode(FILE_MODE))
            .and_then(|()| self.write_lines(&file, kind, entries))
            .and_then(|()| file.sync_all());
        if let Err(e) = written {
            let _ = fs::remove_file(&temp);
            return Err(e);
        }
        fs::rename(&temp, dir.join(kind.file_name()))?;
        fs::File::open(&dir)?.sync_all()?;
        Ok(true)
    }

    /// Length and SHA-256 of an existing list file, if it is a regular
    /// 0600 file of ours (opened without following a link).
    fn digest_of(&self, path: &Path) -> Option<Digest> {
        let mut file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .ok()?;
        let meta = file.metadata().ok()?;
        let ours = meta.is_file() && meta.uid() == self.owner && meta.mode() & 0o7777 == FILE_MODE;
        if !ours || meta.len() > self.max_bytes {
            return None;
        }
        let mut digest = Digest::default();
        io::copy(&mut file, &mut digest).ok()?;
        Some(digest)
    }

    fn write_lines(&self, out: impl Write, kind: ListKind, entries: &[String]) -> io::Result<()> {
        let mut out = BufWriter::new(out);
        let mut bytes = 0u64;
        for entry in entries {
            let line = match kind {
                ListKind::Domains if is_list_host(entry) => format!("0.0.0.0 {entry}\n"),
                ListKind::Ips
                    if entry
                        .parse::<Ipv4Addr>()
                        .is_ok_and(|ip| ip.to_string() == *entry && is_blockable_ip(ip)) =>
                {
                    format!("{entry}\n")
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "an entry doesn't fit the list format",
                    ))
                }
            };
            bytes += line.len() as u64;
            if bytes > self.max_bytes {
                return Err(too_large(format!("over {} bytes", self.max_bytes)));
            }
            out.write_all(line.as_bytes())?;
        }
        out.flush()
    }

    /// Whether the kind's list file is there, as a regular file under real
    /// directories.
    pub fn has_list(&self, list: &IdComponent, kind: ListKind) -> bool {
        let dir = self.kind_dir(list, kind);
        [&self.root, &self.list_dir(list), &dir]
            .into_iter()
            .all(|path| self.check_private_dir(path).is_ok())
            && fs::symlink_metadata(dir.join(kind.file_name())).is_ok_and(|m| m.is_file())
    }

    /// Remove one kind's directory (a link is removed, not followed).
    pub fn remove_kind(&self, list: &IdComponent, kind: ListKind) -> io::Result<()> {
        self.check_private_dir(&self.root)?;
        if fs::symlink_metadata(self.list_dir(list)).is_ok_and(|m| m.is_dir()) {
            remove_entry(&self.kind_dir(list, kind))?;
        }
        Ok(())
    }

    /// Remove everything one subscription owns.
    pub fn remove_list(&self, list: &IdComponent) -> io::Result<()> {
        self.check_private_dir(&self.root)?;
        remove_entry(&self.list_dir(list))
    }

    /// Every entry of the root that names a list, in order.
    pub fn lists(&self) -> io::Result<Vec<IdComponent>> {
        self.check_private_dir(&self.root)?;
        let mut lists: Vec<IdComponent> = fs::read_dir(&self.root)?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| IdComponent::parse(entry.file_name().to_str()?))
            .collect();
        lists.sort();
        Ok(lists)
    }

    /// Create `path` (0700) if missing, then [`check_private_dir`] it and
    /// force its mode to 0700.
    fn ensure_private_dir(&self, path: &Path) -> io::Result<()> {
        match fs::DirBuilder::new().mode(DIR_MODE).create(path) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
            _ => {}
        }
        let meta = self.check_private_dir(path)?;
        if meta.mode() & 0o7777 != DIR_MODE {
            fs::set_permissions(path, fs::Permissions::from_mode(DIR_MODE))?;
        }
        Ok(())
    }

    /// `lstat` `path`: a real directory owned by the bridge's user.
    fn check_private_dir(&self, path: &Path) -> io::Result<fs::Metadata> {
        let meta = fs::symlink_metadata(path)?;
        let problem = if meta.file_type().is_symlink() {
            "is a symbolic link"
        } else if !meta.is_dir() {
            "is not a directory"
        } else if meta.uid() != self.owner {
            "is owned by another user"
        } else {
            return Ok(meta);
        };
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} {problem}", path.display()),
        ))
    }
}

/// Running length and SHA-256 of bytes written to it.
#[derive(Default, Clone)]
struct Digest {
    hasher: sha2::Sha256,
    len: u64,
}

impl PartialEq for Digest {
    fn eq(&self, other: &Self) -> bool {
        use sha2::Digest as _;
        self.len == other.len && self.hasher.clone().finalize() == other.hasher.clone().finalize()
    }
}

impl Write for Digest {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        use sha2::Digest as _;
        self.hasher.update(buf);
        self.len += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Remove a directory tree, file or link at `path` without following a
/// link; a missing entry is fine.
fn remove_entry(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
    }
}

fn too_large(what: String) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("list too large: {what}"),
    )
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, touches no memory and always
    // succeeds.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
#[path = "list_dir_tests.rs"]
mod tests;

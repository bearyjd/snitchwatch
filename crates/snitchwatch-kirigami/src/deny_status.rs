//! Display-only deny-mode status, read from the tower helper's snapshot
//! (`docs/superpowers/plans/2026-10-27-deny-status-display.md`, contract
//! bazzite-tower #138). The file is untrusted input: size-capped, regular
//! files only, never followed through a symlink, unknown fields ignored, and
//! anything that doesn't check out is shown as unknown. Nothing here changes
//! the host; the GUI only shows the `ujust` commands to copy.

use std::io::Read;
use std::path::Path;

use serde::Deserialize;

/// Where the tower helper publishes the snapshot.
pub const STATUS_PATH: &str = "/run/snitchwatch/deny-status.json";
/// This boot's id, compared with the snapshot's `bootId`.
pub const BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";
/// Larger files are refused unread past this many bytes.
pub const MAX_BYTES: u64 = 16 * 1024;
const MAX_BOOT_ID_BYTES: u64 = 128;
/// The helper's longest trial (`on` takes 1 to 60 minutes).
const MAX_TRIAL_MS: i64 = 60 * 60_000;
/// Clock skew tolerated between the helper's write and this clock, and on
/// top of the longest trial.
const SKEW_MS: i64 = 2 * 60_000;

/// The commands the page offers to copy, in display order.
pub const COMMANDS: [&str; 4] = [
    "ujust snitchwatch-deny status",
    "ujust snitchwatch-deny on 10",
    "ujust snitchwatch-deny confirm",
    "ujust snitchwatch-deny off",
];

/// Why the status is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownReason {
    Missing,
    NotRegular,
    TooLarge,
    Unreadable,
    Unparseable,
    UnknownSchema,
    NoBootId,
    StaleBoot,
    /// A trial deadline the helper couldn't have set (its trials last 1 to 60
    /// minutes from `updatedAt`).
    ImplausibleDeadline,
    /// `updatedAt` is later than this clock: one of the two clocks is wrong.
    ClockBehind,
}

/// What the snapshot says, judged against this boot and the clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyState {
    Unknown(UnknownReason),
    Allow,
    /// A deny trial that reverts at `expires_ms` unless confirmed.
    Trial {
        expires_ms: i64,
        remaining_ms: i64,
    },
    /// A trial past its deadline that the snapshot still shows as pending.
    RevertOverdue {
        expires_ms: Option<i64>,
    },
    DenyConfirmed,
    DenyUnconfirmed,
    Other,
}

/// The display model: the state, when the snapshot was written, and why deny
/// can't be enabled (empty when it can, or when unknown).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyView {
    pub state: DenyState,
    pub updated_ms: Option<i64>,
    pub blockers: Vec<String>,
}

impl DenyView {
    fn unknown(reason: UnknownReason) -> Self {
        Self {
            state: DenyState::Unknown(reason),
            updated_ms: None,
            blockers: Vec::new(),
        }
    }
}

/// The schema-1 fields. Unknown fields are ignored; every field must be
/// present (`deserialize_with` keeps serde from defaulting a missing `Option`
/// to `None`), and a mistyped one fails the parse.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    schema: u32,
    #[allow(dead_code)]
    profile: String,
    default_action: String,
    opt_in: bool,
    pending: bool,
    #[serde(deserialize_with = "Option::deserialize")]
    expires_at: Option<String>,
    can_enable: bool,
    blockers: Vec<String>,
    expired: bool,
    updated_at: String,
    #[serde(deserialize_with = "Option::deserialize")]
    boot_id: Option<String>,
}

/// Opens `path` without following a symlink or blocking on a FIFO, and reads
/// at most `cap` bytes of a regular file.
fn read_regular(path: &Path, cap: u64) -> Result<Vec<u8>, UnknownReason> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| match error.raw_os_error() {
            Some(libc::ENOENT) | Some(libc::ENOTDIR) => UnknownReason::Missing,
            Some(libc::ELOOP) => UnknownReason::NotRegular,
            _ => UnknownReason::Unreadable,
        })?;
    let metadata = file.metadata().map_err(|_| UnknownReason::Unreadable)?;
    if !metadata.is_file() {
        return Err(UnknownReason::NotRegular);
    }
    if metadata.len() > cap {
        return Err(UnknownReason::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UnknownReason::Unreadable)?;
    if bytes.len() as u64 > cap {
        return Err(UnknownReason::TooLarge);
    }
    Ok(bytes)
}

/// This boot's id, or `None` when it can't be read.
pub fn read_boot_id(path: &Path) -> Option<String> {
    let bytes = read_regular_proc(path)?;
    let text = String::from_utf8(bytes).ok()?;
    let id = text.trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// `/proc` files report a zero size, so only the cap applies there.
fn read_regular_proc(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_BOOT_ID_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_BOOT_ID_BYTES).then_some(bytes)
}

/// Reads the snapshot at `path` and judges it against `boot_id` and `now_ms`.
pub fn load(path: &Path, boot_id: Option<&str>, now_ms: i64) -> DenyView {
    match read_regular(path, MAX_BYTES) {
        Ok(bytes) => evaluate(&bytes, boot_id, now_ms),
        Err(reason) => DenyView::unknown(reason),
    }
}

/// Judges snapshot `bytes` against this boot's id and the clock.
pub fn evaluate(bytes: &[u8], boot_id: Option<&str>, now_ms: i64) -> DenyView {
    let value: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(_) => return DenyView::unknown(UnknownReason::Unparseable),
    };
    // The schema decides how to read the rest, so check it first.
    if value.get("schema").and_then(serde_json::Value::as_u64) != Some(1) {
        return DenyView::unknown(if value.get("schema").is_some() {
            UnknownReason::UnknownSchema
        } else {
            UnknownReason::Unparseable
        });
    }
    let Ok(snapshot) = serde_json::from_value::<Snapshot>(value) else {
        return DenyView::unknown(UnknownReason::Unparseable);
    };
    debug_assert_eq!(snapshot.schema, 1);
    let Some(current) = boot_id else {
        return DenyView::unknown(UnknownReason::NoBootId);
    };
    if snapshot.boot_id.as_deref() != Some(current) {
        return DenyView::unknown(UnknownReason::StaleBoot);
    }
    let Some(updated_ms) = parse_utc(&snapshot.updated_at) else {
        return DenyView::unknown(UnknownReason::Unparseable);
    };
    let expires_ms = match snapshot.expires_at.as_deref() {
        Some(text) => match parse_utc(text) {
            Some(ms) => Some(ms),
            None => return DenyView::unknown(UnknownReason::Unparseable),
        },
        None => None,
    };
    if updated_ms > now_ms + SKEW_MS {
        return DenyView::unknown(UnknownReason::ClockBehind);
    }
    if let (true, Some(expires_ms)) = (snapshot.pending, expires_ms) {
        if !plausible_deadline(expires_ms, updated_ms, snapshot.expired) {
            return DenyView::unknown(UnknownReason::ImplausibleDeadline);
        }
    }
    let state = if snapshot.pending {
        match expires_ms {
            Some(expires_ms) if !snapshot.expired && now_ms < expires_ms => DenyState::Trial {
                expires_ms,
                remaining_ms: expires_ms - now_ms,
            },
            _ => DenyState::RevertOverdue { expires_ms },
        }
    } else {
        match snapshot.default_action.as_str() {
            "allow" => DenyState::Allow,
            "deny" if snapshot.opt_in => DenyState::DenyConfirmed,
            "deny" => DenyState::DenyUnconfirmed,
            _ => DenyState::Other,
        }
    };
    let blockers = if snapshot.can_enable {
        Vec::new()
    } else {
        snapshot.blockers
    };
    DenyView {
        state,
        updated_ms: Some(updated_ms),
        blockers,
    }
}

/// Whether a pending trial's deadline is one the helper could have written.
/// It sets `expiresAt` 1 to 60 minutes after the trial starts, and every
/// later write (`updatedAt`) is at or after that start, so the deadline is
/// never more than 60 minutes (plus skew) past `updatedAt`. It computes
/// `expired` at write time as now > `expiresAt`, so an expired snapshot was
/// written after its deadline (a revert that failed, then republished), and a
/// live one before it.
fn plausible_deadline(expires_ms: i64, updated_ms: i64, expired: bool) -> bool {
    let ahead = expires_ms - updated_ms;
    ahead <= MAX_TRIAL_MS + SKEW_MS && if expired { ahead <= SKEW_MS } else { ahead > 0 }
}

/// `YYYY-MM-DDTHH:MM:SSZ` (the helper's `TIME_FORMAT`, whole seconds, always
/// UTC "Z") as Unix milliseconds.
pub fn parse_utc(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = text.get(range)?;
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(((days * 24 + hour) * 60 + minute) * 60_000 + second * 1000)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// "M min S s" for a positive remaining time, rounded up to the second.
pub fn remaining_text(remaining_ms: i64) -> String {
    let seconds = (remaining_ms.max(0) + 999) / 1000;
    format!("{} min {} s", seconds / 60, seconds % 60)
}

/// What the page shows: a state token for QML, fixed sentences, and the
/// blocker text (helper output, shown as plain text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyTexts {
    pub state: &'static str,
    pub headline: &'static str,
    pub detail: String,
    pub as_of: String,
    pub blockers: String,
}

fn unknown_detail(reason: UnknownReason) -> &'static str {
    match reason {
        UnknownReason::Missing => {
            "No status from the tower helper: it isn't installed, or hasn't published since this boot."
        }
        UnknownReason::NotRegular => "The status path isn't a regular file, so it isn't read.",
        UnknownReason::TooLarge => "The status file is too large to trust.",
        UnknownReason::Unreadable => "The status file couldn't be read.",
        UnknownReason::Unparseable => "The status file isn't valid status data.",
        UnknownReason::UnknownSchema => "The status file uses a format this version doesn't know.",
        UnknownReason::NoBootId => "This boot's id couldn't be read, so the status can't be checked.",
        UnknownReason::StaleBoot => "The status file is from an earlier boot.",
        UnknownReason::ImplausibleDeadline => {
            "The status file's trial deadline isn't one the helper could have set."
        }
        UnknownReason::ClockBehind => {
            "The status file is dated after this clock's time, so it can't be timed."
        }
    }
}

/// The page's text for `view` at `now_ms`; `local_time` formats a Unix-ms
/// instant as local time.
pub fn texts(view: &DenyView, local_time: impl Fn(i64) -> String) -> DenyTexts {
    let (state, headline, detail) = match &view.state {
        DenyState::Unknown(reason) => (
            "unknown",
            "Deny mode: unknown",
            unknown_detail(*reason).to_string(),
        ),
        DenyState::Allow => ("allow", "Deny mode: off (allow)", String::new()),
        DenyState::Trial {
            expires_ms,
            remaining_ms,
        } => (
            "trial",
            "Deny mode: trial",
            format!(
                "Reverts to allow in {} (at {}) unless confirmed.",
                remaining_text(*remaining_ms),
                local_time(*expires_ms)
            ),
        ),
        DenyState::RevertOverdue { expires_ms } => (
            "overdue",
            "Deny mode: revert overdue",
            match expires_ms {
                Some(ms) => format!(
                    "The trial should have reverted at {}, but the status still shows it \
                     pending. Check with: ujust snitchwatch-deny status",
                    local_time(*ms)
                ),
                None => "The trial has no valid deadline, but the status still shows it \
                         pending. Check with: ujust snitchwatch-deny status"
                    .to_string(),
            },
        ),
        DenyState::DenyConfirmed => ("confirmed", "Deny mode: on (confirmed)", String::new()),
        DenyState::DenyUnconfirmed => {
            ("unconfirmed", "Deny mode: on, not confirmed", String::new())
        }
        DenyState::Other => (
            "other",
            "Deny mode: unrecognized default action",
            String::new(),
        ),
    };
    DenyTexts {
        state,
        headline,
        detail,
        as_of: view
            .updated_ms
            .map(|ms| format!("as of {}", local_time(ms)))
            .unwrap_or_default(),
        blockers: if view.blockers.is_empty() {
            String::new()
        } else {
            format!("Deny can't be enabled now:\n{}", view.blockers.join("\n"))
        },
    }
}

#[cfg(test)]
#[path = "deny_status_tests.rs"]
mod tests;

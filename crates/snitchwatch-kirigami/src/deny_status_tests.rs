//! Fixture files only (`tests/fixtures/deny-status/`, plus temp files for the
//! size, symlink and directory cases). The real `/run` path and the tower
//! helper are never touched.
use super::*;
use std::path::PathBuf;

const BOOT: &str = "0b1f5a2c-6d7e-4f80-9a1b-2c3d4e5f6a7b";
/// 2026-10-26T12:00:00Z, the fixtures' `updatedAt`.
const UPDATED_MS: i64 = 1_793_016_000_000;
const MINUTE_MS: i64 = 60_000;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/deny-status/"
    ))
    .join(name)
}

fn load_fixture(name: &str, now_ms: i64) -> DenyView {
    load(&fixture(name), Some(BOOT), now_ms)
}

#[test]
fn the_fixture_times_parse() {
    assert_eq!(parse_utc("2026-10-26T12:00:00Z"), Some(UPDATED_MS));
    assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_utc("2024-02-29T23:59:59Z"), Some(1_709_251_199_000));
    for bad in [
        "",
        "2026-10-26 12:00:00Z",
        "2026-10-26T12:00:00",
        "2026-10-26T12:00:00+00:00",
        "2026-13-01T00:00:00Z",
        "2025-02-29T00:00:00Z",
        "2026-10-26T24:00:00Z",
        "2026-10-26T12:60:00Z",
        "+026-10-26T12:00:00Z",
        "2026-10-26T12:00:0aZ",
    ] {
        assert_eq!(parse_utc(bad), None, "{bad}");
    }
}

#[test]
fn the_boot_id_fixture_reads_trimmed() {
    assert_eq!(read_boot_id(&fixture("boot_id")).as_deref(), Some(BOOT));
    assert_eq!(read_boot_id(&fixture("absent")), None);
}

#[test]
fn allow_is_shown_with_its_time() {
    let view = load_fixture("allow.json", UPDATED_MS + MINUTE_MS);
    assert_eq!(view.state, DenyState::Allow);
    assert_eq!(view.updated_ms, Some(UPDATED_MS));
    assert!(view.blockers.is_empty());
}

#[test]
fn a_trial_counts_down_from_expires_at() {
    let view = load_fixture("trial.json", UPDATED_MS + 2 * MINUTE_MS);
    assert_eq!(
        view.state,
        DenyState::Trial {
            expires_ms: UPDATED_MS + 5 * MINUTE_MS,
            remaining_ms: 3 * MINUTE_MS,
        }
    );
    assert_eq!(remaining_text(3 * MINUTE_MS), "3 min 0 s");
    assert_eq!(remaining_text(61_001), "1 min 2 s");
}

/// The snapshot still says pending, but the clock is past `expiresAt`: the
/// revert should have happened and the file wasn't rewritten.
#[test]
fn a_trial_past_its_deadline_is_a_revert_overdue() {
    for now in [UPDATED_MS + 5 * MINUTE_MS, UPDATED_MS + 90 * MINUTE_MS] {
        assert_eq!(
            load_fixture("trial.json", now).state,
            DenyState::RevertOverdue {
                expires_ms: Some(UPDATED_MS + 5 * MINUTE_MS)
            }
        );
    }
}

/// The helper's revert failed and a later `publish-status` rewrote the file
/// after the deadline: `pending: true`, `expired: true`, `updatedAt` after
/// `expiresAt` (helper source, bazzite-tower #138: `expired` is computed at
/// write time as now > expiresAt). That is a genuine overdue revert.
#[test]
fn a_snapshot_rewritten_after_the_deadline_is_a_revert_overdue() {
    let text = std::fs::read_to_string(fixture("trial.json"))
        .unwrap()
        .replace("\"expired\":false", "\"expired\":true")
        .replace(
            "\"updatedAt\":\"2026-10-26T12:00:00Z\"",
            "\"updatedAt\":\"2026-10-26T12:06:00Z\"",
        );
    for now in [UPDATED_MS + 6 * MINUTE_MS, UPDATED_MS + 600 * MINUTE_MS] {
        assert_eq!(
            evaluate(text.as_bytes(), Some(BOOT), now).state,
            DenyState::RevertOverdue {
                expires_ms: Some(UPDATED_MS + 5 * MINUTE_MS)
            },
            "now {now}"
        );
    }
    // Days later, too: the helper may republish long after the deadline.
    let days = text.replace("2026-10-26T12:06:00Z", "2026-10-29T08:00:00Z");
    assert!(matches!(
        evaluate(
            days.as_bytes(),
            Some(BOOT),
            UPDATED_MS + 5 * 1440 * MINUTE_MS
        )
        .state,
        DenyState::RevertOverdue { .. }
    ));
}

/// `expired: true` written before the deadline, or `expired: false` written
/// after it, contradicts how the helper computes the flag.
#[test]
fn an_expired_flag_that_contradicts_its_times_is_unknown() {
    let trial = std::fs::read_to_string(fixture("trial.json")).unwrap();
    let early = trial.replace("\"expired\":false", "\"expired\":true");
    assert_eq!(
        evaluate(early.as_bytes(), Some(BOOT), UPDATED_MS + 10 * MINUTE_MS).state,
        DenyState::Unknown(UnknownReason::ImplausibleDeadline)
    );
    let late = trial.replace(
        "\"updatedAt\":\"2026-10-26T12:00:00Z\"",
        "\"updatedAt\":\"2026-10-26T12:06:00Z\"",
    );
    assert_eq!(
        evaluate(late.as_bytes(), Some(BOOT), UPDATED_MS + 10 * MINUTE_MS).state,
        DenyState::Unknown(UnknownReason::ImplausibleDeadline)
    );
}

#[test]
fn confirmed_and_unconfirmed_deny_differ() {
    assert_eq!(
        load_fixture("confirmed.json", UPDATED_MS).state,
        DenyState::DenyConfirmed
    );
    let text = std::fs::read_to_string(fixture("confirmed.json"))
        .unwrap()
        .replace("\"optIn\":true", "\"optIn\":false");
    assert_eq!(
        evaluate(text.as_bytes(), Some(BOOT), UPDATED_MS).state,
        DenyState::DenyUnconfirmed
    );
    let other = text.replace("\"defaultAction\":\"deny\"", "\"defaultAction\":\"other\"");
    assert_eq!(
        evaluate(other.as_bytes(), Some(BOOT), UPDATED_MS).state,
        DenyState::Other
    );
}

#[test]
fn blockers_are_kept_verbatim_and_unknown_fields_ignored() {
    let view = load_fixture("blocked.json", UPDATED_MS);
    assert_eq!(view.state, DenyState::Allow);
    assert_eq!(
        view.blockers,
        ["<b>legacy TCP profile</b>: Server.Address 127.0.0.1:50051"]
    );
}

#[test]
fn every_unknown_case_is_unknown_with_its_reason() {
    let cases = [
        ("absent.json", UnknownReason::Missing),
        ("stale-boot.json", UnknownReason::StaleBoot),
        ("garbage.bin", UnknownReason::Unparseable),
        ("schema-2.json", UnknownReason::UnknownSchema),
        ("missing-field.json", UnknownReason::Unparseable),
    ];
    for (name, reason) in cases {
        let view = load_fixture(name, UPDATED_MS);
        assert_eq!(view.state, DenyState::Unknown(reason), "{name}");
        assert_eq!(view.updated_ms, None, "{name}");
        assert!(view.blockers.is_empty(), "{name}");
    }
}

#[test]
fn without_this_boots_id_nothing_is_trusted() {
    assert_eq!(
        load(&fixture("allow.json"), None, UPDATED_MS).state,
        DenyState::Unknown(UnknownReason::NoBootId)
    );
    let null_boot = std::fs::read_to_string(fixture("allow.json"))
        .unwrap()
        .replace(&format!("\"bootId\":\"{BOOT}\""), "\"bootId\":null");
    assert_eq!(
        evaluate(null_boot.as_bytes(), Some(BOOT), UPDATED_MS).state,
        DenyState::Unknown(UnknownReason::StaleBoot)
    );
}

#[test]
fn mistyped_fields_and_bad_times_are_unparseable() {
    let allow = std::fs::read_to_string(fixture("allow.json")).unwrap();
    for (from, to) in [
        ("\"pending\":false", "\"pending\":\"no\""),
        ("\"blockers\":[]", "\"blockers\":[1]"),
        (
            "\"updatedAt\":\"2026-10-26T12:00:00Z\"",
            "\"updatedAt\":\"yesterday\"",
        ),
        ("\"expiresAt\":null", "\"expiresAt\":\"soon\""),
        ("\"schema\":1", "\"schema\":\"1\""),
    ] {
        let text = allow.replace(from, to);
        assert_ne!(text, allow, "{from}");
        assert!(
            matches!(
                evaluate(text.as_bytes(), Some(BOOT), UPDATED_MS).state,
                DenyState::Unknown(UnknownReason::Unparseable | UnknownReason::UnknownSchema)
            ),
            "{to}"
        );
    }
}

#[test]
fn an_oversize_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deny-status.json");
    let allow = std::fs::read_to_string(fixture("allow.json")).unwrap();
    let padded = format!(
        "{}{}",
        allow.trim_end(),
        " ".repeat(usize::try_from(MAX_BYTES).unwrap())
    );
    std::fs::write(&path, padded).unwrap();
    assert_eq!(
        load(&path, Some(BOOT), UPDATED_MS).state,
        DenyState::Unknown(UnknownReason::TooLarge)
    );
    // Exactly at the cap still reads.
    let exact = format!(
        "{}{}",
        allow.trim_end(),
        " ".repeat(usize::try_from(MAX_BYTES).unwrap() - allow.trim_end().len())
    );
    std::fs::write(&path, exact).unwrap();
    assert_eq!(load(&path, Some(BOOT), UPDATED_MS).state, DenyState::Allow);
}

#[test]
fn symlinks_and_directories_are_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("deny-status.json");
    std::os::unix::fs::symlink(fixture("allow.json"), &link).unwrap();
    assert_eq!(
        load(&link, Some(BOOT), UPDATED_MS).state,
        DenyState::Unknown(UnknownReason::NotRegular)
    );
    assert_eq!(
        load(dir.path(), Some(BOOT), UPDATED_MS).state,
        DenyState::Unknown(UnknownReason::NotRegular)
    );
}

#[test]
fn the_commands_are_the_contracts() {
    assert_eq!(
        COMMANDS,
        [
            "ujust snitchwatch-deny status",
            "ujust snitchwatch-deny on 10",
            "ujust snitchwatch-deny confirm",
            "ujust snitchwatch-deny off",
        ]
    );
}

fn utc_hh_mm(ms: i64) -> String {
    let minutes = ms.div_euclid(MINUTE_MS);
    format!("{:02}:{:02}", minutes / 60 % 24, minutes % 60)
}

#[test]
fn every_state_has_its_fixed_text() {
    let trial = texts(
        &load_fixture("trial.json", UPDATED_MS + 2 * MINUTE_MS),
        utc_hh_mm,
    );
    assert_eq!(trial.state, "trial");
    assert_eq!(
        trial.detail,
        "Reverts to allow in 3 min 0 s (at 12:05) unless confirmed."
    );
    assert_eq!(trial.as_of, "as of 12:00");
    let overdue = texts(
        &load_fixture("trial.json", UPDATED_MS + 9 * MINUTE_MS),
        utc_hh_mm,
    );
    assert_eq!(overdue.state, "overdue");
    assert_eq!(overdue.headline, "Deny mode: revert overdue");
    assert!(overdue.detail.contains("should have reverted at 12:05"));
    let stale = texts(&load_fixture("stale-boot.json", UPDATED_MS), utc_hh_mm);
    assert_eq!(
        (stale.state, stale.headline, stale.as_of.as_str()),
        ("unknown", "Deny mode: unknown", "")
    );
    assert_eq!(stale.detail, "The status file is from an earlier boot.");
    for (name, state) in [
        ("allow.json", "allow"),
        ("confirmed.json", "confirmed"),
        ("blocked.json", "allow"),
        ("absent.json", "unknown"),
        ("garbage.bin", "unknown"),
    ] {
        assert_eq!(
            texts(&load_fixture(name, UPDATED_MS), utc_hh_mm).state,
            state,
            "{name}"
        );
    }
}

#[test]
fn blockers_show_only_when_deny_cant_be_enabled() {
    let blocked = texts(&load_fixture("blocked.json", UPDATED_MS), utc_hh_mm);
    assert_eq!(
        blocked.blockers,
        "Deny can't be enabled now:\n<b>legacy TCP profile</b>: Server.Address 127.0.0.1:50051"
    );
    assert_eq!(
        texts(&load_fixture("allow.json", UPDATED_MS), utc_hh_mm).blockers,
        ""
    );
}

/// Headlines and unknown details are fixed text: no snapshot content.
#[test]
fn only_the_blockers_carry_snapshot_text() {
    let blocked = texts(&load_fixture("blocked.json", UPDATED_MS), utc_hh_mm);
    for fixed in [
        blocked.headline,
        blocked.detail.as_str(),
        blocked.as_of.as_str(),
    ] {
        assert!(
            !fixed.contains('<') && !fixed.contains("Server.Address"),
            "{fixed}"
        );
    }
}

/// The helper's trials last 1 to 60 minutes; anything else can't be a real
/// trial, so it's unknown, never a known countdown.
#[test]
fn an_impossible_trial_deadline_is_unknown() {
    let trial = std::fs::read_to_string(fixture("trial.json")).unwrap();
    for expires in [
        "9999-12-31T23:59:59Z",
        "2026-10-26T13:05:00Z",
        "2026-10-26T12:00:00Z",
        "2026-10-26T11:55:00Z",
    ] {
        let text = trial.replace("2026-10-26T12:05:00Z", expires);
        assert_eq!(
            evaluate(text.as_bytes(), Some(BOOT), UPDATED_MS).state,
            DenyState::Unknown(UnknownReason::ImplausibleDeadline),
            "{expires}"
        );
    }
    // 60 minutes plus the slack is still a trial.
    let longest = trial.replace("2026-10-26T12:05:00Z", "2026-10-26T13:01:00Z");
    assert!(matches!(
        evaluate(longest.as_bytes(), Some(BOOT), UPDATED_MS).state,
        DenyState::Trial { .. }
    ));
}

/// A snapshot written "after now" means this clock went back (or the file's
/// did): nothing in it can be timed, so it's unknown.
#[test]
fn a_snapshot_from_the_future_is_unknown() {
    for name in ["allow.json", "trial.json", "confirmed.json"] {
        assert_eq!(
            load_fixture(name, UPDATED_MS - 10 * MINUTE_MS).state,
            DenyState::Unknown(UnknownReason::ClockBehind),
            "{name}"
        );
    }
    // A small skew is tolerated.
    assert_eq!(
        load_fixture("allow.json", UPDATED_MS - 30_000).state,
        DenyState::Allow
    );
}

/// Every field must be present; `expiresAt` and `bootId` may be null but not
/// missing.
#[test]
fn every_field_must_be_present_even_the_nullable_ones() {
    let allow = std::fs::read_to_string(fixture("allow.json")).unwrap();
    for field in [
        "\"expiresAt\":null,",
        "\"profile\":\"system\",",
        "\"expired\":false,",
        "\"canEnable\":true,",
    ] {
        let text = allow.replace(field, "");
        assert_ne!(text, allow, "{field}");
        assert_eq!(
            evaluate(text.as_bytes(), Some(BOOT), UPDATED_MS).state,
            DenyState::Unknown(UnknownReason::Unparseable),
            "missing {field}"
        );
    }
    let no_boot = allow.replace(&format!(",\"bootId\":\"{BOOT}\""), "");
    assert_ne!(no_boot, allow);
    assert_eq!(
        evaluate(no_boot.as_bytes(), Some(BOOT), UPDATED_MS).state,
        DenyState::Unknown(UnknownReason::Unparseable)
    );
    // Present and null is fine for expiresAt (allow.json has it so).
    assert_eq!(
        evaluate(allow.as_bytes(), Some(BOOT), UPDATED_MS).state,
        DenyState::Allow
    );
}

#[test]
fn the_new_unknown_reasons_have_fixed_text() {
    let trial = std::fs::read_to_string(fixture("trial.json"))
        .unwrap()
        .replace("2026-10-26T12:05:00Z", "9999-12-31T23:59:59Z");
    let far = texts(
        &evaluate(trial.as_bytes(), Some(BOOT), UPDATED_MS),
        utc_hh_mm,
    );
    assert_eq!(
        far.detail,
        "The status file's trial deadline isn't one the helper could have set."
    );
    let ahead = texts(
        &load_fixture("allow.json", UPDATED_MS - 10 * MINUTE_MS),
        utc_hh_mm,
    );
    assert_eq!(
        ahead.detail,
        "The status file is dated after this clock's time, so it can't be timed."
    );
}

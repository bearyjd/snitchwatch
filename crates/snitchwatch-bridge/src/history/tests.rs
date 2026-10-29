//! PR 1 tests: location, directory and file permissions, schema version,
//! settings file, disabled-creates-nothing. Field caps are in
//! `caps_tests.rs`.

use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use super::open::{check_facts, DirFacts};
use super::test_support::{state_dir, tempdir};
use super::*;

const ME: u32 = 1000;

fn euid() -> u32 {
    open::effective_uid()
}

fn consent() -> Consent {
    Consent {
        text_id: "optin-2026-10-28.1".into(),
        accepted_ms: 1_793_000_000_000,
    }
}

fn location(state: &Path) -> Location {
    locate(Mode::User, Some(state)).unwrap()
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

// ---- location -------------------------------------------------------

#[test]
fn the_per_user_bridge_uses_its_state_dir() {
    let at = locate(
        Mode::User,
        Some(Path::new("/home/u/.local/state/snitchwatch")),
    )
    .unwrap();
    assert_eq!(
        at.dir(),
        Path::new("/home/u/.local/state/snitchwatch/history")
    );
    assert_eq!(at.db_path().file_name().unwrap(), "history.db");
}

#[test]
fn the_system_bridge_uses_var_lib_snitchwatch_and_nothing_else() {
    let at = locate(Mode::System, Some(Path::new("/var/lib/snitchwatch"))).unwrap();
    assert_eq!(at.dir(), Path::new("/var/lib/snitchwatch/history"));
    for other in ["/home/u/.local/state/snitchwatch", "/var/lib/other", "/tmp"] {
        assert!(
            locate(Mode::System, Some(Path::new(other))).is_err(),
            "{other}"
        );
    }
}

#[test]
fn no_state_dir_or_a_relative_one_is_unavailable() {
    assert!(locate(Mode::User, None).is_err());
    assert!(locate(Mode::System, None).is_err());
    assert!(locate(Mode::User, Some(Path::new("state"))).is_err());
}

// ---- directory facts ------------------------------------------------

fn dir_facts(uid: u32, mode: u32) -> DirFacts {
    DirFacts {
        is_dir: true,
        uid,
        mode,
    }
}

#[test]
fn only_an_exactly_0700_directory_of_ours_passes() {
    assert!(check_facts(&dir_facts(ME, 0o40700), ME).is_ok());
    for mode in [
        0o40755, 0o40750, 0o40701, 0o40777, 0o40500, 0o42700, 0o41700, 0o44700,
    ] {
        assert!(check_facts(&dir_facts(ME, mode), ME).is_err(), "{mode:o}");
    }
}

#[test]
fn a_directory_of_another_owner_is_refused() {
    assert!(check_facts(&dir_facts(0, 0o40700), ME).is_err());
    assert!(check_facts(&dir_facts(ME + 1, 0o40700), ME).is_err());
}

#[test]
fn a_non_directory_is_refused() {
    let facts = DirFacts {
        is_dir: false,
        uid: ME,
        mode: 0o100700,
    };
    assert!(check_facts(&facts, ME).is_err());
}

// ---- directory on the real filesystem -------------------------------

fn unavailable(state: State) -> String {
    match state {
        State::Unavailable(reason) => reason.0,
        other => panic!("expected unavailable, got {other:?}"),
    }
}

#[test]
fn a_0755_history_directory_makes_history_unavailable() {
    let root = tempdir();
    let state = state_dir(&root);
    let at = location(&state);
    std::fs::create_dir(at.dir()).unwrap();
    std::fs::set_permissions(at.dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let reason = unavailable(load_state(&at));
    assert!(reason.contains("755"), "{reason}");
    assert!(open_store(&at).is_err());
    assert!(!at.db_path().exists());
}

#[test]
fn a_history_directory_of_another_owner_makes_history_unavailable() {
    let root = tempdir();
    let state = state_dir(&root);
    let at = location(&state);
    std::fs::create_dir(at.dir()).unwrap();
    std::fs::set_permissions(at.dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
    // The bridge plays a different user than the directory's owner.
    unavailable(load_state_as(&at, euid() + 1));
    assert!(matches!(load_state_as(&at, euid()), State::Off));
}

#[test]
fn a_symlinked_history_directory_makes_history_unavailable() {
    let root = tempdir();
    let state = state_dir(&root);
    let elsewhere = state.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o700)).unwrap();
    let at = location(&state);
    symlink(&elsewhere, at.dir()).unwrap();
    unavailable(load_state(&at));
    assert!(enable(&at, consent()).is_err());
    assert!(
        names(&elsewhere).is_empty(),
        "nothing was written through the link"
    );
}

#[test]
fn a_file_named_history_makes_history_unavailable() {
    let root = tempdir();
    let state = state_dir(&root);
    let at = location(&state);
    std::fs::write(at.dir(), b"x").unwrap();
    unavailable(load_state(&at));
    assert!(enable(&at, consent()).is_err());
}

#[test]
fn unavailable_never_falls_back_to_another_path() {
    let root = tempdir();
    let state = state_dir(&root);
    let at = location(&state);
    std::fs::create_dir(at.dir()).unwrap();
    std::fs::set_permissions(at.dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = open_store(&at);
    let _ = enable(&at, consent());
    assert_eq!(names(&state), ["history"]);
    assert!(names(at.dir()).is_empty());
    assert_eq!(names(root.path()), ["state"]);
}

// ---- disabled creates nothing ---------------------------------------

#[test]
fn history_that_was_never_enabled_creates_nothing() {
    let root = tempdir();
    let state = state_dir(&root);
    let at = location(&state);
    assert_eq!(load_state(&at), State::Off);
    assert!(open_store(&at).is_err());
    assert_eq!(load_state(&at), State::Off);
    assert!(names(&state).is_empty(), "no history/ and no settings file");
}

#[test]
fn disabled_history_with_a_settings_file_opens_no_database() {
    let root = tempdir();
    let state = state_dir(&root);
    let at = location(&state);
    let enabled = enable(&at, consent()).unwrap();
    settings::save(
        &at,
        &Settings {
            enabled: false,
            ..enabled
        },
    )
    .unwrap();
    assert_eq!(load_state(&at), State::Off);
    assert!(open_store(&at).is_err());
    assert!(!at.db_path().exists());
}

// ---- enable and the settings file -----------------------------------

#[test]
fn enabling_creates_a_0700_directory_and_a_0600_settings_file() {
    let root = tempdir();
    let state = state_dir(&root);
    let at = location(&state);
    let saved = enable(&at, consent()).unwrap();
    assert_eq!(mode_of(at.dir()), 0o700);
    assert_eq!(mode_of(&at.settings_path()), 0o600);
    assert_eq!(names(at.dir()), ["settings.json"]);
    assert_eq!(saved.retention_days, 30);
    assert_eq!(saved.budget_mib, 64);
    assert_eq!(load_state(&at), State::Enabled(saved));
}

#[test]
fn a_restart_reloads_enabled_state_retention_and_consent() {
    let root = tempdir();
    let state = state_dir(&root);
    let at = location(&state);
    let enabled = enable(&at, consent()).unwrap();
    let changed = Settings {
        retention_days: 90,
        budget_mib: 256,
        ..enabled
    };
    settings::save(&at, &changed).unwrap();
    // A new Location from the same state directory is a restarted bridge.
    let State::Enabled(reloaded) = load_state(&location(&state)) else {
        panic!("not enabled after restart");
    };
    assert_eq!(reloaded, changed);
    assert_eq!(reloaded.consent, Some(consent()));
}

#[test]
fn a_settings_file_that_enables_without_consent_is_not_trusted() {
    let root = tempdir();
    let state = state_dir(&root);
    let at = location(&state);
    enable(&at, consent()).unwrap();
    let forged =
        br#"{"version":1,"enabled":true,"retention_days":30,"budget_mib":64,"consent":null}"#;
    crate::state_file::write(&at.settings_path(), forged).unwrap();
    unavailable(load_state(&at));
    assert!(open_store(&at).is_err());
}

#[test]
fn settings_with_unknown_fields_or_values_are_not_trusted() {
    let root = tempdir();
    let state = state_dir(&root);
    let at = location(&state);
    enable(&at, consent()).unwrap();
    let consent_json = r#""consent":{"text_id":"t","accepted_ms":1}"#;
    for body in [
        format!(
            r#"{{"version":1,"enabled":true,"retention_days":30,"budget_mib":64,{consent_json},"gui_enabled":true}}"#
        ),
        format!(
            r#"{{"version":1,"enabled":true,"retention_days":31,"budget_mib":64,{consent_json}}}"#
        ),
        format!(
            r#"{{"version":1,"enabled":true,"retention_days":30,"budget_mib":65,{consent_json}}}"#
        ),
        format!(
            r#"{{"version":2,"enabled":true,"retention_days":30,"budget_mib":64,{consent_json}}}"#
        ),
        "not json".into(),
    ] {
        crate::state_file::write(&at.settings_path(), body.as_bytes()).unwrap();
        unavailable(load_state(&at));
    }
}

#[test]
fn an_oversized_settings_file_is_refused() {
    let root = tempdir();
    let state = state_dir(&root);
    let at = location(&state);
    enable(&at, consent()).unwrap();
    // Valid JSON (padded with whitespace), so only the size rule can refuse it.
    let mut big = settings::encode(&Settings::default()).unwrap();
    big.resize(settings::SETTINGS_MAX_BYTES as usize + 1, b' ');
    crate::state_file::write(&at.settings_path(), &big).unwrap();
    let reason = unavailable(load_state(&at));
    assert!(reason.contains("too large"), "{reason}");
}

#[test]
fn the_largest_valid_settings_fit_the_reserved_footprint() {
    let worst = Settings {
        enabled: true,
        retention_days: 90,
        budget_mib: 256,
        consent: Some(Consent {
            text_id: "a".repeat(settings::TEXT_ID_MAX),
            accepted_ms: i64::MIN,
        }),
    };
    let bytes = settings::encode(&worst).unwrap();
    assert!(
        bytes.len() as u64 <= settings::SETTINGS_MAX_BYTES,
        "{}",
        bytes.len()
    );
}

#[test]
fn consent_text_ids_cannot_escape_or_overflow() {
    let root = tempdir();
    let state = state_dir(&root);
    let at = location(&state);
    for text_id in [
        "a".repeat(settings::TEXT_ID_MAX + 1),
        "\u{0}\u{0}\u{0}\u{0}".into(),
        "with space".into(),
        "quote\"".into(),
        String::new(),
    ] {
        let bad = Consent {
            text_id,
            accepted_ms: 1,
        };
        assert!(enable(&at, bad).is_err());
    }
    // A refusal on validation creates nothing either.
    assert!(names(&state).is_empty());
}

#[test]
fn settings_written_at_every_cap_leave_no_stray_file() {
    let root = tempdir();
    let state = state_dir(&root);
    let at = location(&state);
    let mut worst = enable(&at, consent()).unwrap();
    worst.retention_days = 90;
    worst.budget_mib = 256;
    worst.consent = Some(Consent {
        text_id: "z".repeat(settings::TEXT_ID_MAX),
        accepted_ms: i64::MAX,
    });
    for _ in 0..3 {
        settings::save(&at, &worst).unwrap();
    }
    assert_eq!(names(at.dir()), ["settings.json"]);
    let size = std::fs::metadata(at.settings_path()).unwrap().len();
    assert!(size <= settings::SETTINGS_MAX_BYTES);
}

#[test]
fn a_failed_settings_write_keeps_the_old_file_and_leaves_no_temp() {
    // The old file stays in place; the failure comes before the rename (the
    // directory can't be written to).
    let root = tempdir();
    let state = state_dir(&root);
    let at = location(&state);
    let saved = enable(&at, consent()).unwrap();
    let before = std::fs::read(at.settings_path()).unwrap();
    std::fs::set_permissions(at.dir(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let changed = Settings {
        retention_days: 90,
        ..saved.clone()
    };
    let failed = settings::save(&at, &changed).is_err();
    std::fs::set_permissions(at.dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
    if euid() == 0 {
        return; // root ignores directory permissions
    }
    assert!(failed);
    assert_eq!(std::fs::read(at.settings_path()).unwrap(), before);
    assert_eq!(names(at.dir()), ["settings.json"]);
    assert_eq!(load_state(&at), State::Enabled(saved));
}

#[test]
fn a_settings_file_that_is_a_directory_is_not_replaced() {
    let root = tempdir();
    let state = state_dir(&root);
    let at = location(&state);
    let saved = enable(&at, consent()).unwrap();
    std::fs::remove_file(at.settings_path()).unwrap();
    std::fs::create_dir(at.settings_path()).unwrap();
    assert!(settings::save(&at, &saved).is_err());
    std::fs::remove_dir(at.settings_path()).unwrap();
    assert!(names(at.dir()).is_empty(), "no stray temp file");
}

// ---- modes of pre-existing files ------------------------------------

#[test]
fn a_pre_existing_settings_file_is_tightened_to_exactly_0600() {
    for mode in [0o644, 0o755, 0o4600, 0o640, 0o660] {
        let root = tempdir();
        let state = state_dir(&root);
        let at = location(&state);
        let saved = enable(&at, consent()).unwrap();
        std::fs::set_permissions(at.settings_path(), std::fs::Permissions::from_mode(mode))
            .unwrap();
        assert_eq!(load_state(&at), State::Enabled(saved), "{mode:o}");
        assert_eq!(mode_of(&at.settings_path()), 0o600, "{mode:o}");
    }
}

#[test]
fn a_pre_existing_sidecar_is_tightened_before_sqlite_uses_it() {
    let root = tempdir();
    let (_, at) = enabled_location(&root);
    drop(open_store(&at).unwrap());
    let wal = at.dir().join("history.db-wal");
    std::fs::write(&wal, b"").unwrap();
    std::fs::set_permissions(&wal, std::fs::Permissions::from_mode(0o644)).unwrap();
    drop(open_store(&at).unwrap());
    assert_eq!(mode_of(&wal), 0o600);
    assert_eq!(mode_of(&at.db_path()), 0o600);
}

#[test]
fn an_unreadable_sidecar_fails_the_open_instead_of_surviving_loose() {
    let root = tempdir();
    let (_, at) = enabled_location(&root);
    drop(open_store(&at).unwrap());
    let shm = at.dir().join("history.db-shm");
    std::fs::write(&shm, b"").unwrap();
    // Owner write-only, group/other read: we can't open it to tighten it.
    std::fs::set_permissions(&shm, std::fs::Permissions::from_mode(0o244)).unwrap();
    if std::fs::File::open(&shm).is_ok() {
        return; // running as root: the file can be read after all
    }
    assert!(
        open_store(&at).is_err(),
        "must not open with a 0244 sidecar"
    );
}

#[test]
fn a_hard_linked_sidecar_is_refused_without_chmodding_the_other_file() {
    let root = tempdir();
    let (state, at) = enabled_location(&root);
    drop(open_store(&at).unwrap());
    let other = state.join("other-file");
    std::fs::write(&other, b"x").unwrap();
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::hard_link(&other, at.dir().join("history.db-journal")).unwrap();
    assert!(open_store(&at).is_err());
    assert_eq!(mode_of(&other), 0o644, "chmod must follow validation");
}

#[test]
fn a_pre_existing_database_is_tightened_to_0600() {
    let root = tempdir();
    let (_, at) = enabled_location(&root);
    drop(open_store(&at).unwrap());
    std::fs::set_permissions(at.db_path(), std::fs::Permissions::from_mode(0o666)).unwrap();
    drop(open_store(&at).unwrap());
    assert_eq!(mode_of(&at.db_path()), 0o600);
}

// ---- umask ----------------------------------------------------------

const UMASK_CHILD: &str = "SNITCHWATCH_TEST_UMASK";

/// The work, in a child process whose umask is set: a umask is process-wide
/// and would race the other tests.
fn enable_under(umask: u32) {
    let root = tempdir();
    let state = state_dir(&root);
    let at = location(&state);
    // SAFETY: umask takes an integer and cannot fail.
    unsafe { libc::umask(umask) };
    enable(&at, consent()).unwrap();
    assert_eq!(mode_of(at.dir()), 0o700, "umask {umask:o}");
    assert_eq!(mode_of(&at.settings_path()), 0o600);
    drop(open_store(&at).unwrap());
    assert_eq!(mode_of(&at.db_path()), 0o600);
    // A later enable works on the same directory.
    enable(&at, consent()).unwrap();
}

#[test]
fn enabling_under_a_umask_that_masks_owner_bits_still_yields_0700() {
    if let Ok(value) = std::env::var(UMASK_CHILD) {
        enable_under(u32::from_str_radix(&value, 8).unwrap());
        return;
    }
    for umask in [0o477, 0o277, 0o377, 0o077, 0o000] {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "history::tests::enabling_under_a_umask_that_masks_owner_bits_still_yields_0700",
                "--test-threads=1",
            ])
            .env(UMASK_CHILD, format!("{umask:o}"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "umask {umask:o}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

// ---- the database ---------------------------------------------------

fn pragma(store: &Store, name: &str) -> rusqlite::types::Value {
    store
        .connection()
        .query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
        .unwrap()
}

fn enabled_location(root: &tempfile::TempDir) -> (PathBuf, Location) {
    let state = state_dir(root);
    let at = location(&state);
    enable(&at, consent()).unwrap();
    (state, at)
}

#[test]
fn the_database_is_created_0600_with_the_pinned_layout() {
    let root = tempdir();
    let (_, at) = enabled_location(&root);
    let store = open_store(&at).unwrap();
    assert_eq!(mode_of(&at.db_path()), 0o600);
    assert_eq!(pragma(&store, "page_size"), 4096.into());
    assert_eq!(pragma(&store, "auto_vacuum"), 2.into(), "INCREMENTAL");
    assert_eq!(pragma(&store, "secure_delete"), 1.into());
    assert_eq!(pragma(&store, "journal_mode"), "delete".to_string().into());
    assert_eq!(
        pragma(&store, "user_version"),
        schema::SCHEMA_VERSION.into()
    );
    drop(store);
    let header = std::fs::read(at.db_path()).unwrap();
    assert_eq!(u16::from_be_bytes([header[16], header[17]]), 4096);
}

#[test]
fn reopening_keeps_the_data_and_the_version() {
    let root = tempdir();
    let (_, at) = enabled_location(&root);
    drop(open_store(&at).unwrap());
    let store = open_store(&at).unwrap();
    assert_eq!(pragma(&store, "user_version"), 1.into());
    let tables: i64 = store
        .connection()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 2);
}

fn set_user_version(at: &Location, version: i64) {
    let conn = rusqlite::Connection::open(at.db_path()).unwrap();
    conn.execute_batch(&format!("PRAGMA user_version = {version};"))
        .unwrap();
}

#[test]
fn a_newer_schema_version_is_refused() {
    let root = tempdir();
    let (_, at) = enabled_location(&root);
    drop(open_store(&at).unwrap());
    set_user_version(&at, schema::SCHEMA_VERSION + 1);
    let Err(reason) = open_store(&at) else {
        panic!("a newer schema was opened");
    };
    assert!(reason.0.contains("newer schema"), "{reason}");
}

#[test]
fn a_new_empty_file_is_version_zero_and_is_created_at_the_current_version() {
    // Version 1 is the first schema, so the only "older" database is a new
    // one: version 0, no objects.
    let root = tempdir();
    let (_, at) = enabled_location(&root);
    std::fs::write(at.db_path(), b"").unwrap();
    std::fs::set_permissions(at.db_path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    let store = open_store(&at).unwrap();
    assert_eq!(
        pragma(&store, "user_version"),
        schema::SCHEMA_VERSION.into()
    );
}

#[test]
fn a_foreign_database_at_version_zero_is_refused() {
    let root = tempdir();
    let (_, at) = enabled_location(&root);
    {
        let conn = rusqlite::Connection::open(at.db_path()).unwrap();
        conn.execute_batch("CREATE TABLE other (x);").unwrap();
    }
    assert!(open_store(&at).is_err());
}

#[test]
fn a_database_with_another_page_size_is_refused() {
    let root = tempdir();
    let (_, at) = enabled_location(&root);
    {
        let conn = rusqlite::Connection::open(at.db_path()).unwrap();
        conn.execute_batch("PRAGMA page_size = 8192; PRAGMA auto_vacuum = 2;")
            .unwrap();
        conn.execute_batch(&schema::schema_sql()).unwrap();
        conn.execute_batch("PRAGMA user_version = 1;").unwrap();
    }
    let Err(reason) = open_store(&at) else {
        panic!("page size 8192 was accepted");
    };
    assert!(reason.0.contains("page size"), "{reason}");
}

#[test]
fn a_database_without_incremental_vacuum_is_refused() {
    let root = tempdir();
    let (_, at) = enabled_location(&root);
    {
        let conn = rusqlite::Connection::open(at.db_path()).unwrap();
        conn.execute_batch("PRAGMA page_size = 4096; PRAGMA auto_vacuum = 0;")
            .unwrap();
        conn.execute_batch(&schema::schema_sql()).unwrap();
        conn.execute_batch("PRAGMA user_version = 1;").unwrap();
    }
    assert!(open_store(&at).is_err());
}

#[test]
fn a_database_with_an_extra_table_or_trigger_is_refused() {
    let root = tempdir();
    let (_, at) = enabled_location(&root);
    drop(open_store(&at).unwrap());
    {
        let conn = rusqlite::Connection::open(at.db_path()).unwrap();
        conn.execute_batch("CREATE TABLE extra (x);").unwrap();
    }
    assert!(open_store(&at).is_err());
}

#[test]
fn a_same_sized_but_different_schema_is_refused() {
    // Same object names and count, but an index on another column: only the
    // `CREATE` text comparison catches it.
    let root = tempdir();
    let (_, at) = enabled_location(&root);
    drop(open_store(&at).unwrap());
    {
        let conn = rusqlite::Connection::open(at.db_path()).unwrap();
        conn.execute_batch("DROP INDEX events_ts; CREATE INDEX events_ts ON events (dst_host);")
            .unwrap();
    }
    assert!(open_store(&at).is_err());
}

#[test]
fn an_unsafe_sidecar_is_refused_and_not_followed() {
    let root = tempdir();
    let (state, at) = enabled_location(&root);
    let target = state.join("victim");
    std::fs::write(&target, b"keep").unwrap();
    symlink(&target, at.dir().join("history.db-journal")).unwrap();
    assert!(open_store(&at).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"keep");
}

#[test]
fn the_database_is_only_opened_in_the_checked_directory() {
    // A symlinked history.db is refused (O_NOFOLLOW), never followed.
    let root = tempdir();
    let (state, at) = enabled_location(&root);
    let target = state.join("victim.db");
    std::fs::write(&target, b"").unwrap();
    symlink(&target, at.db_path()).unwrap();
    assert!(open_store(&at).is_err());
    assert_eq!(std::fs::metadata(&target).unwrap().len(), 0);
}

#[test]
fn settings_survive_the_database_being_recreated() {
    let root = tempdir();
    let (_, at) = enabled_location(&root);
    drop(open_store(&at).unwrap());
    std::fs::remove_file(at.db_path()).unwrap();
    let before = std::fs::read(at.settings_path()).unwrap();
    let store = open_store(&at).unwrap();
    assert_eq!(pragma(&store, "user_version"), 1.into());
    assert_eq!(std::fs::read(at.settings_path()).unwrap(), before);
    assert!(matches!(load_state(&at), State::Enabled(_)));
}

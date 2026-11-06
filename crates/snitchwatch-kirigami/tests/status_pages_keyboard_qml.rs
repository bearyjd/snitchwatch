//! Q01 slice 6: the status, settings and first-run pages by keyboard alone,
//! with REAL key events (QtTest) and every control named (the shared guard,
//! `tests/support/a11y_guard.qmlpart`, Qt's accessibility layer on):
//!   * Daemon Health (with the deny section), Settings & Diagnostics, Security
//!     Scan, Geography (no database, and with rows), Traffic, Onboarding, the
//!     silent-mode choice (and its Deny warning), the first-run Recommended
//!     rules groups, and the AI-ready export dialog;
//!   * the exact Tab itinerary of each, as the names a screen reader says;
//!   * what starts, saves, enables or chooses a policy (Run Deep Scan, Start
//!     daemon, Continue, Review recommended rules, a group switch, Save
//!     privately) acts on one fresh Space press and on nothing else: not
//!     Return or Enter, and not a Space auto-repeat or a key still held from
//!     before the focus arrived.
//!
//! Auto-repeats are real: `tests/support/key_repeat.h` re-sends a key given
//! `Qt.GroupSwitchModifier` flagged as an auto-repeat. Nothing here touches
//! the host: `systemctl` is a recording stub on PATH, the scanner binary is
//! missing (a preflight failure, no polkit), and the config, data and runtime
//! directories are temporary. Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};
use snitchwatch_kirigami::scanner::{force_transport_for_tests, run_deep_scan_via, Transport};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

#[path = "../src/accessibility_test.rs"]
mod accessibility;
#[path = "../src/key_repeat_test.rs"]
mod key_repeat;

const PROBE_URL: &str = "qrc:/status_pages_probe.qml";
const DONE_MARKER: &str = "STATUS_PAGES_PROBE_DONE";

/// The shared guard helpers, spliced in at the `// @a11y-guard` line.
const GUARD: &str = include_str!("support/a11y_guard.qmlpart");
const QML: &str = include_str!("probes/status_pages_probe.qml");

/// A one-node IPv4 MaxMind DB that puts every address starting with a 0 bit
/// (8.8.8.8, 1.1.1.1) in the United States: what an installed system database
/// would do to the fixture addresses.
fn us_country_mmdb() -> Vec<u8> {
    let string = |s: &str| [&[0x40 | s.len() as u8][..], s.as_bytes()].concat();
    let map =
        |entries: &[Vec<u8>]| [&[0xE0 | (entries.len() / 2) as u8][..], &entries.concat()].concat();
    let uint = |kind: u8, value: u32| match value {
        0 => vec![kind << 5],
        v => vec![kind << 5 | 1, v as u8],
    };
    let data = map(&[
        string("country"),
        map(&[
            string("iso_code"),
            string("US"),
            string("names"),
            map(&[string("en"), string("United States")]),
        ]),
    ]);
    let metadata = map(&[
        string("binary_format_major_version"),
        uint(5, 2),
        string("binary_format_minor_version"),
        uint(5, 0),
        string("build_epoch"),
        vec![1, 2, 1],
        string("database_type"),
        string("GeoLite2-Country"),
        string("description"),
        map(&[]),
        string("ip_version"),
        uint(5, 4),
        string("languages"),
        vec![0, 4],
        string("node_count"),
        uint(6, 1),
        string("record_size"),
        uint(5, 24),
    ]);
    // One node: bit 0 -> the data record (node_count + 16 + offset 0), bit 1 -> none.
    let mut db = vec![0, 0, 17, 0, 0, 1];
    db.extend([0u8; 16]);
    db.extend(data);
    db.extend(b"\xAB\xCD\xEFMaxMind.com");
    db.extend(metadata);
    db
}

/// The exact Tab itinerary of each view, as accessible names; each ends back
/// at the start button (or, for the dialog, at the control it began on).
const ITINERARIES: &[(&str, &str)] = &[
    (
        "HEALTH",
        r#"["Recheck", "Copy ujust snitchwatch-deny status", "Copy ujust snitchwatch-deny on 10", "Copy ujust snitchwatch-deny confirm", "Copy ujust snitchwatch-deny off", "Start here"]"#,
    ),
    (
        "DIAGNOSTICS",
        r#"["Download optional DB-IP City Lite…", "Download DB-IP Country Lite…", "MaxMind downloads…", "Import Country database…", "Import optional City database…", "Launch at login", "Online research (RDAP) in decision dialog", "Allow remembers for: Forever", "New connections: Ask me (alerts)", "Check again", "Refresh", "Crash log", "Start here"]"#,
    ),
    (
        "GEO_SETUP",
        r#"["Countries by connections", "Download optional DB-IP City Lite…", "Download DB-IP Country Lite…", "MaxMind downloads…", "Import Country database…", "Import optional City database…", "Start here"]"#,
    ),
    (
        "GEO_ROWS",
        r#"["Unknown: 2 connections, allowed 1, denied 0, pending 1", "Start here"]"#,
    ),
    (
        "SILENT",
        r#"["Ask me (alerts)", "Allow and record", "Deny and record", "Continue", "Start here"]"#,
    ),
    (
        "SILENT_DENY",
        r#"["Ask me (alerts)", "Allow and record", "Deny and record", "Review recommended background-service rules", "Continue", "Start here"]"#,
    ),
    (
        "GROUPS",
        r#"["System services", "Details for System services", "App updates", "Details for App updates", "Skip", "Start here"]"#,
    ),
    (
        "EXPORT",
        r#"["Filter by an independent UTC window [from, to)", "Export application: All application identities", "Choose export verdicts (none checked selects none)", "Exact JSON bytes to be saved", "Request preview", "Save privately", "Cancel preview", "Close"]"#,
    ),
    (
        "EXPORT_FILTERS",
        r#"["Filter by an independent UTC window [from, to)", "Export window start in UTC", "Export window end in UTC, exclusive", "Export application: Exact process path", "Export exact process path", "Choose export verdicts (none checked selects none)", "Verdict allow", "Verdict deny", "Verdict blocklist", "Verdict pending", "Verdict unknown", "Exact JSON bytes to be saved", "Request preview", "Save privately", "Cancel preview", "Close"]"#,
    ),
];

#[test]
fn the_status_and_first_run_pages_work_by_keyboard_and_name_every_control() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");

    let host = tempfile::tempdir().unwrap();
    for (var, dir) in [
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_RUNTIME_DIR", "run"),
    ] {
        let path = host.path().join(dir);
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var(var, &path);
    }
    // No polkit and no host broker: the scan is forced onto the native
    // transport with a missing binary (a preflight failure), because the
    // detection would otherwise pick the real session-bus broker inside a
    // Flatpak. The session bus is pointed at a socket that is not there, and
    // both paths must fail locally.
    let missing = host.path().join("not-installed");
    std::env::set_var("SNITCHWATCH_SCANNER_BIN", &missing);
    std::env::set_var(
        "DBUS_SESSION_BUS_ADDRESS",
        format!("unix:path={}", host.path().join("no-bus").display()),
    );
    force_transport_for_tests(Transport::Native(missing.clone()));
    assert!(
        run_deep_scan_via(Transport::Native(missing)).is_err(),
        "a scan with the scanner binary missing must fail locally"
    );
    // The session bus connection itself must fail, which is the first thing a
    // broker scan does: no request (and no polkit) is ever issued. A broker
    // reply error would not prove that, so this is checked on the connection.
    let connected = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async { zbus::connection::Builder::session()?.build().await });
    assert!(
        connected.is_err(),
        "the probe can reach a session bus; a broker scan would contact it"
    );
    assert!(
        run_deep_scan_via(Transport::Broker).is_err(),
        "a broker scan must fail locally"
    );
    // Hermetic GeoIP: a Country database sits in the default search location
    // (a temporary home and data dir), yet the explicit override names a file
    // that is not a database, so no address is classified into a country.
    let database = host.path().join("data/snitchwatch");
    std::fs::create_dir_all(&database).unwrap();
    let fixture = us_country_mmdb();
    std::fs::write(database.join("GeoLite2-Country.mmdb"), &fixture).unwrap();
    std::env::set_var("HOME", host.path());
    let reader = maxminddb::Reader::from_source(fixture).expect("the fixture is a valid database");
    let found: maxminddb::geoip2::Country = reader
        .lookup("8.8.8.8".parse().unwrap())
        .unwrap()
        .decode()
        .unwrap()
        .expect("the fixture database classifies 8.8.8.8");
    assert_eq!(found.country.iso_code, Some("US"));
    let not_a_database = host.path().join("not-a-database.mmdb");
    std::fs::write(&not_a_database, b"not a database").unwrap();
    std::env::set_var("SNITCHWATCH_GEOIP_DB", &not_a_database);
    std::env::remove_var("SNITCHWATCH_GEOIP_CITY_DB");
    // `systemctl` is a stub that records its arguments and fails.
    let bin = host.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = host.path().join("systemctl.log");
    let stub = bin.join("systemctl");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 1\n", log.display()),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![bin.clone()];
    dirs.extend(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(dirs).unwrap());
    std::env::set_var("QML_XHR_ALLOW_FILE_READ", "1");

    let mut app = QGuiApplication::new();
    assert!(
        accessibility::ffi::activate_accessibility(),
        "Qt's accessibility layer did not turn on"
    );
    assert!(key_repeat::ffi::install_auto_repeat_keys());
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));
    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            let mut qml = QML
                .replace("    // @a11y-guard\n", GUARD)
                .replace(
                    "@@DENY_PATH@@",
                    &host.path().join("absent.json").display().to_string(),
                )
                .replace("@@SYSTEMCTL_LOG@@", &log.display().to_string());
            for (name, itinerary) in ITINERARIES {
                qml = qml.replace(&format!("@@{name}_ITINERARY@@"), itinerary);
            }
            engine.load_data(&QByteArray::from(qml.as_str()), &QUrl::from(PROBE_URL));
        }
        // A probe that failed to load has no window: exec() would never return.
        if root_ok.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "probe root was null:\n{captured}"
    );
    let bad: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains("TypeError"))
        .filter(|line| !line.contains(DONE_MARKER))
        .collect();
    assert!(
        bad.is_empty(),
        "status pages probe failed:\n{}",
        bad.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}

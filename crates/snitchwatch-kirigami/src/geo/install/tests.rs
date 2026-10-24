use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};

use super::test_fixtures;
use super::validate_in_worker as validate;
use test_fixtures::{fixture, tree_fixture};

// These tests concern private filesystem/atomic-replacement behavior and
// bounded fixtures only. The separate process integration tests exercise the
// production worker; the test harness executable has no application CLI mode.
fn install(source: &Path, target: &Path) -> Result<(), String> {
    super::install_with_validator(source, target, validate)
}

// Run hostile trees in a disposable process: the pre-fix iterator can allocate
// indefinitely before yielding even its first record. Both memory/CPU limits
// and a parent deadline protect the ordinary test runner.
#[test]
fn hostile_search_trees_are_bounded_and_preserve_the_installed_database() {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "geo::install::tests::hostile_search_tree_child",
            "--nocapture",
        ])
        .env("SNITCHWATCH_HOSTILE_TREE_CHILD", "1");
    unsafe {
        command.pre_exec(|| {
            let memory = libc::rlimit {
                rlim_cur: 128 * 1024 * 1024,
                rlim_max: 128 * 1024 * 1024,
            };
            let cpu = libc::rlimit {
                rlim_cur: 2,
                rlim_max: 2,
            };
            let core = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_DATA, &memory) != 0
                || libc::setrlimit(libc::RLIMIT_CPU, &cpu) != 0
                || libc::setrlimit(libc::RLIMIT_CORE, &core) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                status.success(),
                "hostile-tree validation child failed: {status}"
            );
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("hostile-tree validation exceeded five seconds");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn hostile_search_tree_child() {
    if std::env::var_os("SNITCHWATCH_HOSTILE_TREE_CHILD").is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("hostile.mmdb");
    let target = dir.path().join("installed.mmdb");
    let original = fixture("GeoLite2-Country");
    std::fs::write(&target, &original).unwrap();
    for record_size in [24, 28, 32] {
        for ip_version in [4, 6] {
            let depth = if ip_version == 4 { 33 } else { 129 };
            let mut too_deep = vec![[depth + 16, depth + 16]; depth as usize];
            for node in 0..depth - 1 {
                too_deep[node as usize][0] = node + 1;
            }
            // Acyclic, tiny DAG with exponentially many empty network paths.
            // No yielded record means a record counter cannot bound this.
            let mut empty_dag = vec![[31, 31]; 31];
            for (node, children) in empty_dag.iter_mut().enumerate().take(30) {
                *children = [(node + 1) as u32; 2];
            }
            for nodes in [vec![[0, 17]], vec![[1, 18], [0, 18]], too_deep, empty_dag] {
                let bytes = tree_fixture("GeoLite2-Country", &nodes, record_size, ip_version);
                std::fs::write(&source, bytes).unwrap();
                assert!(
                    install(&source, &target).is_err(),
                    "accepted malformed {record_size}/{ip_version} tree"
                );
                assert_eq!(std::fs::read(&target).unwrap(), original);
            }
        }
    }
}

#[test]
fn valid_tree_record_sizes_and_shared_subtrees_remain_supported() {
    for record_size in [24, 28, 32] {
        for ip_version in [4, 6] {
            let bytes = tree_fixture(
                "GeoLite2-Country",
                &[[1, 1], [18, 18]],
                record_size,
                ip_version,
            );
            validate(&bytes).unwrap();
        }
    }
}

#[test]
fn dbip_country_lite_and_existing_maxmind_country_types_decode_and_install() {
    use crate::geo::resolver::{CountryLookup, MmdbLookup};
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("download.mmdb");
    let target = dir.path().join("app/GeoLite2-Country.mmdb");
    for kind in ["DBIP-Country-Lite", "GeoLite2-Country", "GeoIP2-Country"] {
        let bytes = fixture(kind);
        std::fs::write(&source, &bytes).unwrap();
        install(&source, &target).unwrap_or_else(|e| panic!("{kind}: {e}"));
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        let lookup = MmdbLookup::open(&target).unwrap();
        assert_eq!(lookup.build_epoch(), 1);
        assert!(lookup
            .source_label()
            .contains(if kind == "DBIP-Country-Lite" {
                "IP Geolocation by DB-IP"
            } else {
                "MaxMind"
            }));
        if kind == "DBIP-Country-Lite" {
            assert!(lookup.source_label().contains("CC BY 4.0"));
            assert!(!lookup.source_label().contains("MaxMind"));
        }
        let country = lookup.lookup("8.8.8.8".parse().unwrap()).unwrap();
        assert_eq!(country.code, "US");
    }
}

// Explicit external-artifact check. Ordinary tests never read vendor data or
// use the network. The packaging README records the pinned input and command.
#[test]
#[ignore = "requires the separately downloaded, hash-verified official October 2026 MMDB"]
fn dbip_official_file_compatibility() {
    use crate::geo::resolver::{CountryLookup, MmdbLookup};
    let path = std::path::PathBuf::from(
        std::env::var_os("SNITCHWATCH_DBIP_PROOF")
            .expect("set SNITCHWATCH_DBIP_PROOF to the verified extracted file"),
    );
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len(), 8_348_509);
    validate(&bytes).unwrap();
    let reader = maxminddb::Reader::from_source(&bytes).unwrap();
    assert_eq!(reader.metadata().database_type, "DBIP-Country-Lite");
    assert_eq!(reader.metadata().build_epoch, 1_790_818_572);
    assert_eq!(reader.metadata().node_count, 1_383_101);
    assert_eq!(
        reader.networks(Default::default()).unwrap().count(),
        1_382_908
    );
    let lookup = MmdbLookup::open(&path).unwrap();
    assert_eq!(
        lookup.lookup("8.8.8.8".parse().unwrap()).unwrap().code,
        "US"
    );
    assert!(lookup.source_label().contains("IP Geolocation by DB-IP"));
}

#[test]
fn failed_source_read_and_failed_replacement_preserve_previous_database() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("download.mmdb");
    let target = dir.path().join("country.mmdb");
    let bytes = fixture("GeoLite2-Country");
    std::fs::write(&target, &bytes).unwrap();
    assert!(install(&source, &target).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), bytes);
    std::fs::write(&source, fixture("DBIP-Country-Lite")).unwrap();
    // Exhaust the atomic writer's private temporary names; it must preserve
    // both the previous database and the pre-existing files.
    for attempt in 0..16 {
        std::fs::write(
            dir.path().join(format!(
                ".country.mmdb.tmp-{}-{attempt}",
                std::process::id()
            )),
            b"occupied",
        )
        .unwrap();
    }
    assert!(install(&source, &target).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), bytes);
}

#[test]
fn fresh_startup_discovers_imported_dbip_under_compatible_legacy_filename() {
    use crate::geo::resolver::discover_and_open_with;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("snitchwatch/GeoLite2-Country.mmdb");
    let discover = || {
        discover_and_open_with(
            |p| p == target && p.exists(),
            None,
            "/unused-home",
            dir.path().to_str(),
        )
    };
    assert!(!discover().available);
    let source = dir.path().join("dbip.mmdb");
    std::fs::write(&source, fixture("DBIP-Country-Lite")).unwrap();
    install(&source, &target).unwrap();
    let restarted = discover();
    assert!(restarted.available);
    assert_eq!(restarted.path, target);
    assert!(restarted.source.contains("IP Geolocation by DB-IP"));
    assert_eq!(restarted.build_epoch, 1);
}

#[test]
fn installs_valid_country_database_privately_and_replaces_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("download.mmdb");
    let target = dir.path().join("app/GeoLite2-Country.mmdb");
    let bytes = fixture("GeoLite2-Country");
    std::fs::write(&source, &bytes).unwrap();
    install(&source, &target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(target.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    install(&target, &target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), bytes);
}

#[test]
fn installs_exact_validated_snapshot_and_preserves_old_bytes_on_validator_failure() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.mmdb");
    let target = dir.path().join("installed.mmdb");
    let original = fixture("GeoLite2-Country");
    std::fs::write(&source, &original).unwrap();
    std::fs::write(&target, b"previous installation").unwrap();
    for script in [
        "exec /missing/database-validator",
        "kill -KILL $$",
        "printf invalid",
        "exec sleep 30",
    ] {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", script]);
        let result = super::install_with_validator(&source, &target, |snapshot| {
            assert_eq!(snapshot, original);
            super::worker::validate_with_command(
                snapshot,
                command,
                std::time::Duration::from_millis(50),
            )
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"previous installation");
    }
    super::install_with_validator(&source, &target, |snapshot| {
        validate(snapshot)?;
        std::fs::write(&source, b"source changed after validation").unwrap();
        Ok(())
    })
    .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), original);
}

#[test]
fn invalid_wrong_type_and_oversized_files_preserve_existing_database() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("download.mmdb");
    let target = dir.path().join("GeoLite2-Country.mmdb");
    let original = fixture("GeoLite2-Country");
    std::fs::write(&target, &original).unwrap();
    for bytes in [
        b"not a database".to_vec(),
        fixture("GeoLite2-ASN"),
        fixture("DBIP-City-Lite"),
        fixture("DBIP-ASN-Lite"),
    ] {
        std::fs::write(&source, bytes).unwrap();
        assert!(install(&source, &target).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), original);
    }
    std::fs::File::create(&source)
        .unwrap()
        .set_len(MAX_DATABASE_BYTES + 1)
        .unwrap();
    assert!(install(&source, &target).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), original);
}

#[test]
fn corrupt_tree_and_country_records_are_rejected() {
    let mut bytes = fixture("GeoLite2-Country");
    bytes[0..3].copy_from_slice(&[0xff, 0xff, 0xff]);
    assert!(validate(&bytes).is_err());
    let mut bytes = fixture("GeoLite2-Country");
    let index = bytes.windows(2).position(|p| p == b"US").unwrap();
    bytes[index] = b'1';
    assert!(validate(&bytes).is_err());
}

#[test]
fn invalid_later_country_record_preserves_existing_database() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("download.mmdb");
    let target = dir.path().join("installed.mmdb");
    let original = fixture("GeoLite2-Country");
    let mut bytes = original.clone();
    let marker = bytes
        .windows(14)
        .position(|p| p == b"\xab\xcd\xefMaxMind.com")
        .unwrap();
    let mut second_record = bytes[22..marker].to_vec();
    let code = second_record.windows(2).position(|p| p == b"US").unwrap();
    second_record[code] = b'1';
    let second_pointer = (17 + second_record.len() as u32).to_be_bytes();
    bytes[3..6].copy_from_slice(&second_pointer[1..]);
    bytes.splice(marker..marker, second_record);
    std::fs::write(&source, bytes).unwrap();
    std::fs::write(&target, &original).unwrap();
    assert!(install(&source, &target).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), original);
}

#[test]
fn symlink_source_and_target_are_rejected_without_touching_referent() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("download.mmdb");
    let linked = dir.path().join("link.mmdb");
    let target = dir.path().join("GeoLite2-Country.mmdb");
    let bytes = fixture("GeoLite2-Country");
    std::fs::write(&source, &bytes).unwrap();
    symlink(&source, &linked).unwrap();
    assert!(install(&linked, &target).is_err());
    assert!(!target.exists());
    symlink(&source, &target).unwrap();
    assert!(install(&source, &target).is_err());
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
}

#[test]
fn city_profile_is_explicit_and_cannot_promote_country_limits() {
    let bytes = fixture("DBIP-City-Lite");
    assert!(validate(&bytes).is_err());
    validate_profile(&bytes, DatabaseProfile::City).unwrap();
    assert!(validate_profile(&fixture("DBIP-Country-Lite"), DatabaseProfile::City).is_err());
    assert_eq!(DatabaseProfile::Country.max_input(), 32 * 1024 * 1024);
    assert_eq!(DatabaseProfile::Country.max_networks(), 2_000_000);
    assert_eq!(DatabaseProfile::City.max_input(), 256 * 1024 * 1024);
}

#[test]
fn invalid_city_import_preserves_both_datasets_and_selection() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("download.mmdb");
    let country = dir.path().join("GeoLite2-Country.mmdb");
    let city = dir.path().join("dbip-city-lite.mmdb");
    let selection = dir.path().join("geo-dataset");
    std::fs::write(&country, b"country before").unwrap();
    std::fs::write(&city, b"city before").unwrap();
    std::fs::write(&selection, b"country").unwrap();
    std::fs::write(&source, fixture("DBIP-Country-Lite")).unwrap();
    assert!(
        install_profile_with_validator(&source, &city, DatabaseProfile::City, |bytes| {
            validate_profile(bytes, DatabaseProfile::City)
        })
        .is_err()
    );
    assert_eq!(std::fs::read(country).unwrap(), b"country before");
    assert_eq!(std::fs::read(city).unwrap(), b"city before");
    assert_eq!(std::fs::read(selection).unwrap(), b"country");
}

#[test]
fn city_fields_validate_and_decode_without_promising_physical_precision() {
    use crate::geo::resolver::{CountryLookup, GeoCapability, MmdbLookup, SharedResolver};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("city.mmdb");
    for (latitude, longitude) in [
        (Some(45.5019), Some(-73.5674)),
        (None, None),
        (Some(45.5), None),
    ] {
        let bytes = test_fixtures::city_fixture("Montreal", latitude, longitude, 1);
        validate_profile(&bytes, DatabaseProfile::City).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let lookup = MmdbLookup::open(&path).unwrap();
        assert_eq!(lookup.capability(), GeoCapability::City);
        let resolver = SharedResolver::new(Some(std::sync::Arc::new(lookup)));
        let resolved = resolver.resolve("8.8.8.8");
        let place = resolved.place.unwrap();
        assert_eq!(place.city.as_deref(), Some("Montreal"));
        assert_eq!(place.subdivisions, ["QC"]);
        assert_eq!(
            place.coordinates.is_some(),
            latitude.is_some() && longitude.is_some()
        );
        assert_eq!(place.precision.is_some(), place.coordinates.is_some());
        assert!(place.provider.contains("DB-IP"));
    }
    for bytes in [
        test_fixtures::city_fixture("Montreal", Some(f64::NAN), Some(0.0), 1),
        test_fixtures::city_fixture("Montreal", Some(f64::INFINITY), None, 1),
        test_fixtures::city_fixture("Montreal", Some(91.0), Some(0.0), 1),
        test_fixtures::city_fixture("Montreal", Some(0.0), Some(-181.0), 1),
        test_fixtures::city_fixture("Bad\nname", None, None, 1),
        test_fixtures::city_fixture(&"x".repeat(257), None, None, 1),
        test_fixtures::city_fixture("Montreal", None, None, 9),
    ] {
        assert!(validate_profile(&bytes, DatabaseProfile::City).is_err());
    }
}

#[test]
fn successful_import_selects_dataset_and_country_reselects_with_city_present() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.mmdb");
    let country = dir.path().join("GeoLite2-Country.mmdb");
    let city = dir.path().join("dbip-city-lite.mmdb");
    let selection = dir.path().join("geo-dataset");
    for profile in [DatabaseProfile::City, DatabaseProfile::Country] {
        let bytes = fixture(if profile == DatabaseProfile::City {
            "DBIP-City-Lite"
        } else {
            "DBIP-Country-Lite"
        });
        std::fs::write(&source, &bytes).unwrap();
        let target = if profile == DatabaseProfile::City {
            &city
        } else {
            &country
        };
        install_selected_with_validator(&source, target, profile, |bytes| {
            validate_profile(bytes, profile)
        })
        .unwrap();
        match crate::geo::dataset::read(dir.path()) {
            crate::geo::dataset::Selection::Installed {
                profile: selected,
                path,
            } => {
                assert_eq!(selected, profile);
                assert_eq!(std::fs::read(path).unwrap(), bytes);
            }
            _ => panic!("import did not commit its dataset manifest"),
        }
    }
    assert!(selection.exists());
}

#[test]
fn manifest_failure_preserves_previous_active_generation_and_manual_files() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.mmdb");
    let target = dir.path().join("dbip-city-lite.mmdb");
    std::fs::write(&target, b"manual city before").unwrap();
    std::fs::write(&source, fixture("DBIP-City-Lite")).unwrap();
    install_selected_with_validator(&source, &target, DatabaseProfile::City, |bytes| {
        validate_profile(bytes, DatabaseProfile::City)
    })
    .unwrap();
    let previous_manifest = std::fs::read(dir.path().join("geo-dataset")).unwrap();
    let previous_path = match crate::geo::dataset::read(dir.path()) {
        crate::geo::dataset::Selection::Installed { path, .. } => path,
        _ => unreachable!(),
    };
    let previous_bytes = std::fs::read(&previous_path).unwrap();
    let before_count = std::fs::read_dir(dir.path()).unwrap().count();
    let failing_write =
        |_: &Path, _: &[u8]| Err(std::io::Error::other("injected manifest write failure"));
    assert!(install_transaction(
        &source,
        &target,
        DatabaseProfile::City,
        |bytes| validate_profile(bytes, DatabaseProfile::City),
        failing_write
    )
    .is_err());
    assert_eq!(
        std::fs::read(dir.path().join("geo-dataset")).unwrap(),
        previous_manifest
    );
    assert_eq!(std::fs::read(previous_path).unwrap(), previous_bytes);
    assert_eq!(std::fs::read(&target).unwrap(), b"manual city before");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before_count);
}

#[test]
fn error_after_manifest_commit_reports_success_and_retains_selected_file() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.mmdb");
    let target = dir.path().join("dbip-city-lite.mmdb");
    std::fs::write(&source, fixture("DBIP-City-Lite")).unwrap();
    let sync_failure = |path: &Path, bytes: &[u8]| {
        crate::private_files::write_private(path, bytes)?;
        Err(std::io::Error::other("injected directory sync failure"))
    };
    install_transaction(
        &source,
        &target,
        DatabaseProfile::City,
        |bytes| validate_profile(bytes, DatabaseProfile::City),
        sync_failure,
    )
    .unwrap();
    assert!(
        matches!(crate::geo::dataset::read(dir.path()), crate::geo::dataset::Selection::Installed { path, .. } if path.is_file())
    );
}

#[test]
#[ignore = "requires separately downloaded and hash-verified official October 2026 City MMDB"]
fn dbip_official_city_file_profile_benchmark() {
    let path = std::path::PathBuf::from(
        std::env::var_os("SNITCHWATCH_DBIP_CITY_PROOF").expect("set SNITCHWATCH_DBIP_CITY_PROOF"),
    );
    let started = std::time::Instant::now();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len(), 126_998_165);
    validate_profile(&bytes, DatabaseProfile::City).unwrap();
    let validation_wall = started.elapsed();
    let reader = maxminddb::Reader::from_source(&bytes).unwrap();
    assert_eq!(reader.metadata().database_type, "DBIP-City-Lite");
    assert_eq!(reader.metadata().node_count, 14_297_794);
    let (graph_steps, height) =
        super::tree::work_stats(&bytes, reader.metadata(), DatabaseProfile::City.max_steps())
            .unwrap();
    let networks = reader
        .networks(Default::default())
        .unwrap()
        .map(|record| record.map(|_| 1usize))
        .sum::<Result<usize, _>>()
        .unwrap();
    eprintln!("City profile evidence: bytes={} nodes={} networks={} expanded_graph_steps={} tree_height={} validation_wall={:?}", bytes.len(), reader.metadata().node_count, networks, graph_steps, height, validation_wall);
}

#[test]
fn city_publisher_surrounding_name_whitespace_is_normalized() {
    use crate::geo::resolver::{CountryLookup, MmdbLookup};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("city.mmdb");
    for locale in ["en", "es"] {
        let bytes = test_fixtures::country_localized_name_fixture(locale, "\tGuadeloupe\n");
        validate_profile(&bytes, DatabaseProfile::City).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let place = MmdbLookup::open(&path)
            .unwrap()
            .lookup_place("5.102.74.1".parse().unwrap())
            .unwrap();
        assert_eq!(
            place.country.name,
            if locale == "en" { "Guadeloupe" } else { "GL" }
        );
    }
    let embedded_control = test_fixtures::country_localized_name_fixture("es", "Gua\tdeloupe");
    assert!(validate_profile(&embedded_control, DatabaseProfile::City).is_err());
}

#[test]
#[ignore = "requires separately downloaded and hash-verified official October 2026 City MMDB"]
fn dbip_official_city_resolver_point_smoke() {
    use crate::geo::resolver::{Bucket, CountryLookup, GeoCapability, MmdbLookup, SharedResolver};
    let path = std::path::PathBuf::from(
        std::env::var_os("SNITCHWATCH_DBIP_CITY_PROOF").expect("set SNITCHWATCH_DBIP_CITY_PROOF"),
    );
    let lookup = std::sync::Arc::new(MmdbLookup::open(&path).unwrap());
    assert_eq!(lookup.capability(), GeoCapability::City);
    for address in ["192.168.1.1", "fd00::1"] {
        assert!(lookup.lookup_place(address.parse().unwrap()).is_none());
    }
    let resolver = SharedResolver::new(Some(lookup));
    let us = resolver.resolve("8.8.8.8").place.unwrap();
    assert_eq!(us.country.code, "US");
    assert_eq!(us.city.as_deref(), Some("Mountain View"));
    let location = us.coordinates.unwrap();
    assert_eq!((location.latitude, location.longitude), (37.422, -122.085));
    let mut positions = Vec::new();
    for address in ["2001:4860:4860::8888", "2606:4700:4700::1111"] {
        let place = resolver.resolve(address).place.unwrap();
        assert_eq!(place.country.code, "CA");
        assert_eq!(place.city.as_deref(), Some("Montreal"));
        assert!(
            place.provider.contains("DB-IP")
                && place
                    .precision
                    .as_deref()
                    .is_some_and(|s| s.contains("Approximate"))
        );
        let location = place.coordinates.unwrap();
        assert!(location.latitude.is_finite() && location.longitude.is_finite());
        assert_eq!((location.latitude, location.longitude), (45.5019, -73.5674));
        positions.push(location);
    }
    assert_eq!(positions[0], positions[1]);
    for address in ["192.168.1.1", "fd00::1"] {
        let result = resolver.resolve(address);
        assert_eq!(result.bucket, Bucket::Local);
        assert!(result.place.is_none());
    }
}

use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};

// A tiny synthetic MMDB: one IPv4 search node, one country record, and real
// binary metadata. No vendor data, network, environment mutation or mocks.
pub(crate) fn fixture(database_type: &str) -> Vec<u8> {
    fn string(value: &str) -> Vec<u8> {
        assert!(value.len() < 29);
        let mut out = vec![0x40 | value.len() as u8];
        out.extend_from_slice(value.as_bytes());
        out
    }
    fn map(entries: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
        let mut out = vec![0xe0 | entries.len() as u8];
        for (key, value) in entries {
            out.extend(string(key));
            out.extend(value);
        }
        out
    }
    let mut out = vec![0, 0, 17, 0, 0, 17];
    out.extend([0; 16]);
    out.extend(map(vec![(
        "country",
        map(vec![("iso_code", string("US"))]),
    )]));
    out.extend_from_slice(b"\xab\xcd\xefMaxMind.com");
    out.extend(map(vec![
        ("binary_format_major_version", vec![0xa1, 2]),
        ("binary_format_minor_version", vec![0xa1, 0]),
        ("build_epoch", vec![1, 2, 1]),
        ("database_type", string(database_type)),
        ("description", map(vec![])),
        ("ip_version", vec![0xa1, 4]),
        ("languages", vec![0, 4]),
        ("node_count", vec![0xc1, 1]),
        ("record_size", vec![0xa1, 24]),
    ]));
    out
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

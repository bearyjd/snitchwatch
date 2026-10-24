// A tiny synthetic MMDB: one IPv4 search node, one country record, and real
// binary metadata. No vendor data, network, environment mutation or mocks.
pub(crate) fn fixture(database_type: &str) -> Vec<u8> {
    tree_fixture(database_type, &[[17, 17]], 24, 4)
}

pub(crate) fn tree_fixture(
    database_type: &str,
    nodes: &[[u32; 2]],
    record_size: u16,
    ip_version: u8,
) -> Vec<u8> {
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
    let mut out = Vec::new();
    for &[left, right] in nodes {
        match record_size {
            24 => {
                out.extend_from_slice(&left.to_be_bytes()[1..]);
                out.extend_from_slice(&right.to_be_bytes()[1..]);
            }
            28 => {
                out.extend_from_slice(&left.to_be_bytes()[1..]);
                out.push(((left >> 20) as u8 & 0xf0) | ((right >> 24) as u8 & 0x0f));
                out.extend_from_slice(&right.to_be_bytes()[1..]);
            }
            32 => {
                out.extend_from_slice(&left.to_be_bytes());
                out.extend_from_slice(&right.to_be_bytes());
            }
            _ => panic!("unsupported fixture record size"),
        }
    }
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
        ("ip_version", vec![0xa1, ip_version]),
        ("languages", vec![0, 4]),
        ("node_count", {
            let mut value = vec![0xc4];
            value.extend_from_slice(&(nodes.len() as u32).to_be_bytes());
            value
        }),
        ("record_size", vec![0xa1, record_size as u8]),
    ]));
    out
}

/// Controlled City fields, retaining the tiny real MMDB tree/metadata.
pub(crate) fn city_fixture(
    city_name: &str,
    latitude: Option<f64>,
    longitude: Option<f64>,
    subdivisions: usize,
) -> Vec<u8> {
    fn string(value: &str) -> Vec<u8> {
        let mut out = if value.len() < 29 {
            vec![0x40 | value.len() as u8]
        } else {
            assert!(value.len() < 285);
            vec![0x5d, (value.len() - 29) as u8]
        };
        out.extend(value.as_bytes());
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
    fn double(value: f64) -> Vec<u8> {
        let mut out = vec![0x68];
        out.extend(value.to_be_bytes());
        out
    }
    let mut location = Vec::new();
    if let Some(value) = latitude {
        location.push(("latitude", double(value)));
    }
    if let Some(value) = longitude {
        location.push(("longitude", double(value)));
    }
    let mut sub = vec![subdivisions as u8, 4];
    for _ in 0..subdivisions {
        sub.extend(map(vec![("iso_code", string("QC"))]));
    }
    let record = map(vec![
        ("country", map(vec![("iso_code", string("CA"))])),
        (
            "city",
            map(vec![("names", map(vec![("en", string(city_name))]))]),
        ),
        ("location", map(location)),
        ("subdivisions", sub),
    ]);
    let mut bytes = fixture("DBIP-City-Lite");
    let marker = bytes
        .windows(14)
        .position(|s| s == b"\xab\xcd\xefMaxMind.com")
        .unwrap();
    bytes.splice(22..marker, record);
    bytes
}

/// Publisher regression: the October 2026 City file contains a leading tab
/// in country.names.es (Guadeloupe). Names remain bounded before trimming.
pub(crate) fn country_localized_name_fixture(locale: &str, name: &str) -> Vec<u8> {
    assert_eq!(locale.len(), 2);
    assert!(name.len() < 29);
    let mut record = vec![0xe1, 0x47];
    record.extend(b"country");
    record.extend([0xe2, 0x48]);
    record.extend(b"iso_code");
    record.extend([0x42, b'G', b'L', 0x45]);
    record.extend(b"names");
    record.extend([0xe1, 0x42]);
    record.extend(locale.as_bytes());
    record.push(0x40 | name.len() as u8);
    record.extend(name.as_bytes());
    let mut bytes = fixture("DBIP-City-Lite");
    let marker = bytes
        .windows(14)
        .position(|s| s == b"\xab\xcd\xefMaxMind.com")
        .unwrap();
    bytes.splice(22..marker, record);
    bytes
}

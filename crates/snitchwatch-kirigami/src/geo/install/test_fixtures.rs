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

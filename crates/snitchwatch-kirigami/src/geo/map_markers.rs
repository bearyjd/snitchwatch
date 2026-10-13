//! Qt-free marker model for the Monitor map band: the join of per-country
//! counts with map centroids, clustering by grid bucket, and the spoken
//! summaries. The cxx-qt `GeoModel` only serialises the result for QML.
//!
//! Markers are placed by the *approximate* country of a destination IP, so
//! everything here is country-level; there is no finer position to leak.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use snitchwatch_bridge::ws_messages::ServerMessage;

use super::resolver::SharedResolver;
use super::store::DisplayRow;
use super::world_map;

/// One country on the map, in grid units (0.1 degree).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub code: String,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub total: i32,
    pub allowed: i32,
    pub denied: i32,
    pub pending: i32,
}

/// Markers merged by [`cluster`]; a lone country is a cluster of one.
#[derive(Debug, Clone, PartialEq)]
pub struct Cluster {
    /// Total-weighted centroid, grid units.
    pub x: f64,
    pub y: f64,
    /// Members, busiest first (ties by code).
    pub members: Vec<Marker>,
    pub total: i32,
    pub allowed: i32,
    pub denied: i32,
    pub pending: i32,
}

/// Join per-country counts with centroids. Buckets without an ISO code
/// (local network, unknown) and countries with no centroid get no marker;
/// the Geography page still lists them.
pub fn markers(rows: &[DisplayRow]) -> Vec<Marker> {
    rows.iter()
        .filter(|r| r.total > 0)
        .filter_map(|r| {
            let (x, y) = world_map::lookup(&r.country_code)?.centroid;
            Some(Marker {
                code: r.country_code.to_ascii_uppercase(),
                name: r.country_name.clone(),
                x,
                y,
                total: r.total,
                allowed: r.allowed,
                denied: r.denied,
                pending: r.pending,
            })
        })
        .collect()
}

/// Merge markers that fall in the same grid bucket of `radius_px` screen
/// pixels at `scale` pixels per grid unit. A non-positive `scale` or radius
/// merges nothing. Output order is stable: busiest cluster first, then by
/// first member code.
pub fn cluster(markers: &[Marker], scale: f64, radius_px: f64) -> Vec<Cluster> {
    let cell = if scale > 0.0 && radius_px > 0.0 {
        radius_px / scale
    } else {
        0.0
    };
    let mut buckets: BTreeMap<(i64, i64, String), Vec<Marker>> = BTreeMap::new();
    for m in markers {
        let key = if cell > 0.0 {
            (
                (f64::from(m.x) / cell).floor() as i64,
                (f64::from(m.y) / cell).floor() as i64,
                String::new(),
            )
        } else {
            (0, 0, m.code.clone())
        };
        buckets.entry(key).or_default().push(m.clone());
    }
    let mut out: Vec<Cluster> = buckets.into_values().map(build).collect();
    out.sort_by(|a, b| {
        b.total
            .cmp(&a.total)
            .then_with(|| a.members[0].code.cmp(&b.members[0].code))
    });
    out
}

fn build(mut members: Vec<Marker>) -> Cluster {
    members.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.code.cmp(&b.code)));
    let total: i32 = members.iter().map(|m| m.total).sum();
    let weight = f64::from(total.max(1));
    let x = members
        .iter()
        .map(|m| f64::from(m.x * m.total))
        .sum::<f64>()
        / weight;
    let y = members
        .iter()
        .map(|m| f64::from(m.y * m.total))
        .sum::<f64>()
        / weight;
    Cluster {
        x,
        y,
        total,
        allowed: members.iter().map(|m| m.allowed).sum(),
        denied: members.iter().map(|m| m.denied).sum(),
        pending: members.iter().map(|m| m.pending).sum(),
        members,
    }
}

impl Cluster {
    /// The verdict that decides the marker's shape and colour: the largest
    /// count, ties going to the one that needs attention first
    /// (denied, then pending, then allowed). `"none"` when nothing is decided.
    pub fn dominant(&self) -> &'static str {
        let best = self.allowed.max(self.denied).max(self.pending);
        if best == 0 {
            "none"
        } else if self.denied == best {
            "denied"
        } else if self.pending == best {
            "pending"
        } else {
            "allowed"
        }
    }

    /// More than one verdict present: the marker also gets a ring.
    pub fn mixed(&self) -> bool {
        [self.allowed, self.denied, self.pending]
            .iter()
            .filter(|n| **n > 0)
            .count()
            > 1
    }

    /// Spoken summary: `Germany: 3 denied, 1 pending`, or for a cluster
    /// `3 countries (Germany, France and 1 more): 4 allowed, 2 denied`.
    pub fn summary(&self) -> String {
        let counts = describe_counts(self.allowed, self.denied, self.pending, self.total);
        if let [only] = self.members.as_slice() {
            return format!("{}: {}", only.name, counts);
        }
        let shown: Vec<&str> = self
            .members
            .iter()
            .take(2)
            .map(|m| m.name.as_str())
            .collect();
        let rest = self.members.len() - shown.len();
        let names = if rest == 0 {
            shown.join(" and ")
        } else {
            format!("{} and {} more", shown.join(", "), rest)
        };
        format!("{} countries ({}): {}", self.members.len(), names, counts)
    }

    fn to_json(&self) -> Value {
        json!({
            "x": self.x,
            "y": self.y,
            "total": self.total,
            "allowed": self.allowed,
            "denied": self.denied,
            "pending": self.pending,
            "dominant": self.dominant(),
            "mixed": self.mixed(),
            "summary": self.summary(),
            "members": self.members.iter().map(|m| json!({
                "code": m.code,
                "name": m.name,
                "total": m.total,
                "summary": describe_counts(m.allowed, m.denied, m.pending, m.total),
            })).collect::<Vec<_>>(),
        })
    }
}

fn describe_counts(allowed: i32, denied: i32, pending: i32, total: i32) -> String {
    let parts: Vec<String> = [
        (allowed, "allowed"),
        (denied, "denied"),
        (pending, "pending"),
    ]
    .iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, label)| format!("{n} {label}"))
    .collect();
    if parts.is_empty() {
        format!("{total} connections")
    } else {
        parts.join(", ")
    }
}

/// The clusters for `rows` as the JSON array `MapView.qml` draws.
pub fn clusters_json(rows: &[DisplayRow], scale: f64, radius_px: f64) -> String {
    let clusters = cluster(&markers(rows), scale, radius_px);
    Value::Array(clusters.iter().map(Cluster::to_json).collect()).to_string()
}

/// All country outlines as one SVG path (grid units), drawn as a single
/// `ShapePath`.
pub fn land_path() -> &'static str {
    static PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let mut shapes: Vec<_> = world_map::countries()
            .filter(|c| !c.path.is_empty())
            .collect();
        shapes.sort_by_key(|c| c.code);
        shapes.iter().map(|c| c.path).collect::<Vec<_>>().join(" ")
    })
}

/// Countries a message touches (inserted or updated rows), for pulses.
/// Local and unresolved destinations have no marker, so they are skipped.
pub fn activity_codes(msg: &ServerMessage, resolver: &SharedResolver) -> Vec<String> {
    let rows = match msg {
        ServerMessage::InsertConnectionRows { rows }
        | ServerMessage::UpdateConnectionRows { rows } => rows,
        _ => return Vec::new(),
    };
    let codes: BTreeSet<String> = rows
        .iter()
        .filter_map(|r| match resolver.resolve(&r.dst_ip).bucket {
            super::resolver::Bucket::Country(code) => Some(code.to_ascii_uppercase()),
            _ => None,
        })
        .collect();
    codes.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(code: &str, name: &str, a: i32, d: i32, p: i32) -> DisplayRow {
        DisplayRow {
            country_code: code.to_string(),
            country_name: name.to_string(),
            flag: String::new(),
            total: a + d + p,
            allowed: a,
            denied: d,
            pending: p,
        }
    }

    fn marker(code: &str, x: i32, y: i32, total: i32) -> Marker {
        Marker {
            code: code.to_string(),
            name: code.to_string(),
            x,
            y,
            total,
            allowed: total,
            denied: 0,
            pending: 0,
        }
    }

    #[test]
    fn join_uses_centroids_and_skips_buckets_without_one() {
        let rows = vec![
            row("DE", "Germany", 1, 3, 1),
            row("", "Local network", 4, 0, 0),
            row("ZZ", "Nowhere", 2, 0, 0),
        ];
        let m = markers(&rows);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].code, "DE");
        assert_eq!((m[0].x, m[0].y), world_map::lookup("DE").unwrap().centroid);
        assert_eq!(
            (m[0].allowed, m[0].denied, m[0].pending, m[0].total),
            (1, 3, 1, 5)
        );
    }

    #[test]
    fn markers_in_one_bucket_merge_with_summed_counts() {
        // 20 px at 0.2 px/unit is a 100-unit cell.
        let ms = vec![
            marker("AA", 110, 110, 2),
            marker("BB", 150, 190, 5),
            marker("CC", 900, 900, 1),
        ];
        let cs = cluster(&ms, 0.2, 20.0);
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].total, 7);
        assert_eq!(cs[0].allowed, 7);
        let codes: Vec<&str> = cs[0].members.iter().map(|m| m.code.as_str()).collect();
        assert_eq!(codes, ["BB", "AA"]);
        assert_eq!(cs[1].members[0].code, "CC");
        // Weighted toward the busier member.
        assert!(cs[0].x > 130.0 && cs[0].x < 150.0);
    }

    #[test]
    fn markers_in_different_buckets_stay_apart_and_zooming_in_splits_them() {
        let ms = vec![marker("AA", 110, 110, 1), marker("BB", 350, 110, 1)];
        assert_eq!(cluster(&ms, 0.2, 20.0).len(), 2);
        let near = vec![marker("AA", 110, 110, 1), marker("BB", 150, 110, 1)];
        assert_eq!(cluster(&near, 0.2, 20.0).len(), 1);
        // 5x zoom: the cell shrinks to 20 units and they separate.
        assert_eq!(cluster(&near, 1.0, 20.0).len(), 2);
    }

    #[test]
    fn degenerate_scale_merges_nothing() {
        let ms = vec![marker("AA", 1, 1, 1), marker("BB", 1, 1, 1)];
        assert_eq!(cluster(&ms, 0.0, 20.0).len(), 2);
        assert_eq!(cluster(&ms, 0.2, 0.0).len(), 2);
    }

    #[test]
    fn ordering_is_stable_regardless_of_input_order() {
        let a = vec![
            marker("AA", 0, 0, 3),
            marker("BB", 5000, 0, 3),
            marker("CC", 0, 5000, 9),
        ];
        let mut b = a.clone();
        b.reverse();
        let codes = |cs: Vec<Cluster>| -> Vec<String> {
            cs.iter().map(|c| c.members[0].code.clone()).collect()
        };
        assert_eq!(codes(cluster(&a, 0.2, 20.0)), ["CC", "AA", "BB"]);
        assert_eq!(codes(cluster(&b, 0.2, 20.0)), ["CC", "AA", "BB"]);
    }

    #[test]
    fn dominant_verdict_and_ring() {
        let c = |a, d, p| {
            build(vec![Marker {
                allowed: a,
                denied: d,
                pending: p,
                total: a + d + p,
                ..marker("AA", 0, 0, 0)
            }])
        };
        assert_eq!(c(5, 1, 0).dominant(), "allowed");
        assert!(c(5, 1, 0).mixed());
        assert_eq!(c(2, 2, 0).dominant(), "denied");
        assert_eq!(c(1, 0, 1).dominant(), "pending");
        assert!(!c(3, 0, 0).mixed());
        assert_eq!(c(0, 0, 0).dominant(), "none");
    }

    #[test]
    fn summaries_name_the_country_and_omit_zero_counts() {
        let de = build(markers(&[row("DE", "Germany", 0, 3, 1)]));
        assert_eq!(de.summary(), "Germany: 3 denied, 1 pending");
        let two = build(markers(&[
            row("DE", "Germany", 4, 0, 0),
            row("FR", "France", 0, 2, 0),
        ]));
        assert_eq!(
            two.summary(),
            "2 countries (Germany and France): 4 allowed, 2 denied"
        );
        let many = build(markers(&[
            row("DE", "Germany", 3, 0, 0),
            row("FR", "France", 2, 0, 0),
            row("NL", "Netherlands", 1, 0, 0),
        ]));
        assert_eq!(
            many.summary(),
            "3 countries (Germany, France and 1 more): 6 allowed"
        );
    }

    #[test]
    fn json_carries_what_the_view_needs() {
        let rows = vec![row("DE", "Germany", 1, 0, 0)];
        let v: Value = serde_json::from_str(&clusters_json(&rows, 0.2, 28.0)).unwrap();
        assert_eq!(v[0]["members"][0]["code"], "DE");
        assert_eq!(v[0]["summary"], "Germany: 1 allowed");
        assert_eq!(v[0]["dominant"], "allowed");
        assert_eq!(clusters_json(&[], 0.2, 28.0), "[]");
    }

    #[test]
    fn land_path_is_one_path_covering_the_shapes() {
        let p = land_path();
        assert!(p.starts_with('M'));
        assert_eq!(
            p.matches('M').count(),
            world_map::countries()
                .map(|c| c.path.matches('M').count())
                .sum::<usize>()
        );
    }

    #[test]
    fn activity_codes_are_distinct_countries_of_touched_rows() {
        use crate::geo::resolver::fakes::FakeLookup;
        use snitchwatch_bridge::ws_messages::ConnectionRow;
        use std::sync::Arc;
        let lookup =
            FakeLookup::default()
                .with("81.2.69.142", "gb", "UK")
                .with("140.82.121.4", "US", "US");
        let resolver = SharedResolver::new(Some(Arc::new(lookup)));
        let mk = |ip: &str| ConnectionRow {
            id: ip.to_string(),
            process: "p".into(),
            process_path: None,
            dst_host: "h".into(),
            dst_ip: ip.to_string(),
            dst_port: 443,
            protocol: "tcp".into(),
            direction: "outgoing".into(),
            action: None,
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
            matched_rule: None,
            auto_answer: None,
            answer_deadline_ms: None,
            deferred: false,
            decided_by_default: false,
        };
        let msg = ServerMessage::InsertConnectionRows {
            rows: vec![
                mk("81.2.69.142"),
                mk("140.82.121.4"),
                mk("81.2.69.142"),
                mk("10.0.0.1"),
            ],
        };
        assert_eq!(activity_codes(&msg, &resolver), ["GB", "US"]);
        assert!(activity_codes(&ServerMessage::ClearConnectionRows, &resolver).is_empty());
    }
}

//! Qt-free world map data: country shapes and marker centroids.
//!
//! Parses the committed `data/world-map.txt` (generated offline by
//! `scripts/gen_world_map.py` from Natural Earth 1:110m, public domain; see
//! `docs/map-data.md`). Nothing is fetched at build, test or run time.
//!
//! Coordinates are an equirectangular grid of [`GRID_WIDTH`] x [`GRID_HEIGHT`]
//! units (0.1 degree). Countries the 110m set drops (tiny states) have an
//! empty `path` but carry a centroid from a finer Natural Earth file; the few
//! codes no Natural Earth file has get no marker (`Header::no_marker`).

use std::collections::HashMap;
use std::sync::OnceLock;

const RAW: &str = include_str!("../../data/world-map.txt");

pub const GRID_WIDTH: i32 = 3600;
pub const GRID_HEIGHT: i32 = 1800;

/// One country: ISO-A2 code, English name, marker centroid and SVG path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Country {
    pub code: &'static str,
    pub name: &'static str,
    /// Marker position on the grid.
    pub centroid: (i32, i32),
    /// Absolute `M/L/Z` SVG path; empty when the country has no 110m shape.
    pub path: &'static str,
}

/// Provenance and round-trip counts from the file header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Header {
    pub sha256: &'static str,
    pub tag: &'static str,
    pub source_features: usize,
    pub source_polygons: usize,
    /// Polygons that collapsed to fewer than 3 grid points and were dropped.
    pub degenerate_polygons: usize,
    pub skipped_uncoded: usize,
    pub no_shape: Vec<&'static str>,
    /// Codes with neither shape nor centroid in any Natural Earth source.
    pub no_marker: Vec<&'static str>,
    /// `(url, sha256)` of each supplementary marker-only source.
    pub supplements: Vec<(&'static str, &'static str)>,
}

struct Parsed {
    header: Header,
    by_code: HashMap<&'static str, Country>,
}

fn parse_centroid(s: &str) -> Option<(i32, i32)> {
    let (x, y) = s.split_once(',')?;
    Some((x.parse().ok()?, y.parse().ok()?))
}

fn parse(raw: &'static str) -> Parsed {
    let mut header = Header::default();
    let mut by_code = HashMap::new();
    for line in raw.lines() {
        if let Some(meta) = line.strip_prefix("# ") {
            let Some((key, val)) = meta.split_once(": ") else {
                continue;
            };
            match key {
                "sha256" => header.sha256 = val,
                "tag" => header.tag = val,
                "source_features" => header.source_features = val.parse().unwrap_or(0),
                "source_polygons" => header.source_polygons = val.parse().unwrap_or(0),
                "degenerate_polygons" => header.degenerate_polygons = val.parse().unwrap_or(0),
                "skipped_uncoded" => header.skipped_uncoded = val.parse().unwrap_or(0),
                "no_shape" => header.no_shape = val.split_whitespace().collect(),
                "no_marker" => header.no_marker = val.split_whitespace().collect(),
                k if k.starts_with("supplement_") => {
                    if let Some((url, rest)) = val.split_once(" sha256 ") {
                        let hash = rest.split(' ').next().unwrap_or("");
                        header.supplements.push((url, hash));
                    }
                }
                _ => {}
            }
            continue;
        }
        let mut cols = line.splitn(4, '\t');
        let (Some(code), Some(name), Some(cen), Some(path)) =
            (cols.next(), cols.next(), cols.next(), cols.next())
        else {
            continue;
        };
        let Some(centroid) = parse_centroid(cen) else {
            continue;
        };
        by_code.insert(
            code,
            Country {
                code,
                name,
                centroid,
                path,
            },
        );
    }
    Parsed { header, by_code }
}

fn parsed() -> &'static Parsed {
    static CELL: OnceLock<Parsed> = OnceLock::new();
    CELL.get_or_init(|| parse(RAW))
}

/// Look up a country by ISO-A2 code (either case).
pub fn lookup(code: &str) -> Option<&'static Country> {
    parsed().by_code.get(code.to_ascii_uppercase().as_str())
}

/// All countries, in no particular order.
pub fn countries() -> impl Iterator<Item = &'static Country> {
    parsed().by_code.values()
}

pub fn header() -> &'static Header {
    &parsed().header
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOCS: &str = include_str!("../../../../docs/map-data.md");
    const SIZE_BUDGET: usize = 150 * 1024;

    /// All officially assigned ISO 3166-1 alpha-2 codes (249) plus Kosovo
    /// `XK` (user-assigned, but emitted by MaxMind and Natural Earth). The
    /// resolver returns whatever the operator's `.mmdb` says, so this is the
    /// full set it can produce for a real country.
    const ISO_CODES: &str = "AD AE AF AG AI AL AM AO AQ AR AS AT AU AW AX AZ BA BB BD BE BF BG BH \
BI BJ BL BM BN BO BQ BR BS BT BV BW BY BZ CA CC CD CF CG CH CI CK CL CM CN CO CR CU CV CW CX CY CZ \
DE DJ DK DM DO DZ EC EE EG EH ER ES ET FI FJ FK FM FO FR GA GB GD GE GF GG GH GI GL GM GN GP GQ GR \
GS GT GU GW GY HK HM HN HR HT HU ID IE IL IM IN IO IQ IR IS IT JE JM JO JP KE KG KH KI KM KN KP KR \
KW KY KZ LA LB LC LI LK LR LS LT LU LV LY MA MC MD ME MF MG MH MK ML MM MN MO MP MQ MR MS MT MU MV \
MW MX MY MZ NA NC NE NF NG NI NL NO NP NR NU NZ OM PA PE PF PG PH PK PL PM PN PR PS PT PW PY QA RE \
RO RS RU RW SA SB SC SD SE SG SH SI SJ SK SL SM SN SO SR SS ST SV SX SY SZ TC TD TF TG TH TJ TK TL \
TM TN TO TR TT TV TW TZ UA UG UM US UY UZ VA VC VE VG VI VN VU WF WS YE YT ZA ZM ZW XK";

    /// Codes the 110m set has no shape for (tiny states and small territories).
    /// Their centroids come from the 50m, tiny-countries or 10m files.
    const NO_SHAPE_110M: &str = "AD AG AI AS AW AX BB BH BL BM BQ CK CV CW DM FM FO GD GG GI GP GS GU HK HM IM IO JE KI KM KN KY LC LI MC MF MH MO MP MQ MS MT MU MV NF NR NU PF PM PN PW SC SG SH SM ST SX TC TK TO TV UM VA VC VG VI WF WS";

    /// Assigned codes no Natural Earth file (110m, 50m, tiny, 10m) has: parts
    /// of a parent feature (GF in FR, SJ in NO, RE and YT in FR) or uninhabited
    /// or tiny territories. They get no marker and are never hand-entered.
    const NO_MARKER: &str = "BV CC CX GF RE SJ YT";

    fn words(s: &str) -> Vec<&str> {
        s.split_whitespace().collect()
    }

    #[test]
    fn file_is_under_size_budget() {
        assert!(
            RAW.len() < SIZE_BUDGET,
            "world-map.txt is {} bytes",
            RAW.len()
        );
    }

    #[test]
    fn country_count_matches_source() {
        let h = header();
        let shaped = countries().filter(|c| !c.path.is_empty()).count();
        assert_eq!(shaped, h.source_features - h.skipped_uncoded);
        assert_eq!(countries().count(), shaped + h.no_shape.len());
        assert_eq!(h.supplements.len(), 3);
    }

    #[test]
    fn polygon_count_round_trips() {
        let polys: usize = countries().map(|c| c.path.matches('M').count()).sum();
        assert_eq!(
            polys + header().degenerate_polygons,
            header().source_polygons
        );
    }

    #[test]
    fn every_iso_code_has_a_centroid_or_is_documented_no_marker() {
        let no_shape = words(NO_SHAPE_110M);
        let no_marker = words(NO_MARKER);
        for code in words(ISO_CODES) {
            if no_marker.contains(&code) {
                assert!(lookup(code).is_none(), "{code} should have no marker");
                continue;
            }
            let c = lookup(code).unwrap_or_else(|| panic!("{code} missing"));
            assert!((0..=GRID_WIDTH).contains(&c.centroid.0), "{code} x");
            assert!((0..=GRID_HEIGHT).contains(&c.centroid.1), "{code} y");
            assert_eq!(c.path.is_empty(), no_shape.contains(&code), "{code} shape");
        }
        let mut documented = header().no_shape.clone();
        documented.sort_unstable();
        let mut expected = no_shape;
        expected.sort_unstable();
        assert_eq!(documented, expected);
        assert_eq!(header().no_marker, no_marker);
    }

    #[test]
    fn lookup_is_case_insensitive_and_rejects_unknown() {
        assert_eq!(lookup("fr"), lookup("FR"));
        assert!(lookup("ZZ").is_none());
        assert!(lookup("").is_none());
    }

    #[test]
    fn france_centroid_is_mainland_france() {
        let (x, y) = lookup("FR").unwrap().centroid;
        let lon = f64::from(x) / 10.0 - 180.0;
        let lat = 90.0 - f64::from(y) / 10.0;
        assert!((0.0..=4.0).contains(&lon), "lon {lon}");
        assert!((44.0..=48.0).contains(&lat), "lat {lat}");
    }

    #[test]
    fn paths_use_only_m_l_z_and_numbers() {
        for c in countries() {
            let toks: Vec<&str> = c.path.split_whitespace().collect();
            let mut i = 0;
            while i < toks.len() {
                let t = toks[i];
                if t == "Z" {
                    i += 1;
                    continue;
                }
                let (cmd, first) = t.split_at(1);
                assert!(cmd == "M" || cmd == "L", "{}: command {t}", c.code);
                let x: i32 = first.parse().unwrap_or_else(|_| panic!("{}: {t}", c.code));
                let y: i32 = toks[i + 1].parse().unwrap_or_else(|_| panic!("{}", c.code));
                assert!((0..=GRID_WIDTH).contains(&x) && (0..=GRID_HEIGHT).contains(&y));
                i += 2;
            }
        }
    }

    #[test]
    fn header_hashes_match_docs_note() {
        let h = header();
        assert_eq!(h.sha256.len(), 64);
        assert!(
            DOCS.contains(h.sha256),
            "docs/map-data.md lacks {}",
            h.sha256
        );
        assert!(DOCS.contains(h.tag), "docs/map-data.md lacks {}", h.tag);
        for (url, hash) in &h.supplements {
            assert_eq!(hash.len(), 64, "{url}");
            assert!(DOCS.contains(hash), "docs/map-data.md lacks {hash}");
            assert!(DOCS.contains(url), "docs/map-data.md lacks {url}");
        }
    }
}

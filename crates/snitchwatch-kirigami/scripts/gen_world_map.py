#!/usr/bin/env python3
"""Generate data/world-map.txt from Natural Earth 1:110m admin-0 countries.

Dev-time only; offline (never fetched at build or test time). Usage:

    python3 -I scripts/gen_world_map.py <ne_110m_admin_0_countries.geojson> \
        <ne_50m_admin_0_countries.geojson> \
        <ne_50m_admin_0_tiny_countries.geojson> \
        <ne_10m_admin_0_countries.geojson> \
        <ne_10m_admin_0_map_subunits.geojson> > data/world-map.txt

Shapes come from the 110m file. Codes it drops get a marker-only centroid
(the feature's LABEL_X/LABEL_Y, else its polygon centroid) from the first of
the 50m, 50m tiny-countries, 10m and 10m map-subunits files that has the code. Codes none of them
have get no marker at all; nothing is ever entered by hand.

Source: https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/geojson/ne_110m_admin_0_countries.geojson
Licence: public domain (https://www.naturalearthdata.com/about/terms-of-use/).
See docs/map-data.md.

Output is line based. `#` lines are the header; every other line is
`CODE<TAB>NAME<TAB>CX,CY<TAB>PATH` with an equirectangular grid of
3600x1800 integer units (0.1 degree; x = (lon+180)*10, y = (90-lat)*10).
PATH is absolute SVG `M x y L x y ... Z` (empty for "no shape" countries).
The centroid is the area-weighted centroid of the largest polygon's exterior
ring, so e.g. France sits in mainland France, not between Europe and Guiana.
"""
import hashlib
import json
import sys

TAG = "v5.1.2"
URL = (
    "https://raw.githubusercontent.com/nvkelso/natural-earth-vector/"
    + TAG
    + "/geojson/ne_110m_admin_0_countries.geojson"
)
BASE = "https://raw.githubusercontent.com/nvkelso/natural-earth-vector/" + TAG + "/geojson/"
# Supplementary marker-only sources, tried in this order.
SUPPLEMENTS = [
    ("50m", "ne_50m_admin_0_countries.geojson"),
    ("tiny", "ne_50m_admin_0_tiny_countries.geojson"),
    ("10m", "ne_10m_admin_0_countries.geojson"),
    ("subunits", "ne_10m_admin_0_map_subunits.geojson"),
]
LICENCE_URL = "https://www.naturalearthdata.com/about/terms-of-use/"
SCALE = 10  # grid units per degree

# Codes a resolver can return for a real country: all 249 assigned ISO 3166-1
# alpha-2 codes plus XK (Kosovo, user-assigned; used by MaxMind and Natural Earth).
ISO_CODES = set(
    """AD AE AF AG AI AL AM AO AQ AR AS AT AU AW AX AZ BA BB BD BE BF BG BH BI BJ BL
BM BN BO BQ BR BS BT BV BW BY BZ CA CC CD CF CG CH CI CK CL CM CN CO CR CU CV CW CX
CY CZ DE DJ DK DM DO DZ EC EE EG EH ER ES ET FI FJ FK FM FO FR GA GB GD GE GF GG GH
GI GL GM GN GP GQ GR GS GT GU GW GY HK HM HN HR HT HU ID IE IL IM IN IO IQ IR IS IT
JE JM JO JP KE KG KH KI KM KN KP KR KW KY KZ LA LB LC LI LK LR LS LT LU LV LY MA MC
MD ME MF MG MH MK ML MM MN MO MP MQ MR MS MT MU MV MW MX MY MZ NA NC NE NF NG NI NL
NO NP NR NU NZ OM PA PE PF PG PH PK PL PM PN PR PS PT PW PY QA RE RO RS RU RW SA SB
SC SD SE SG SH SI SJ SK SL SM SN SO SR SS ST SV SX SY SZ TC TD TF TG TH TJ TK TL TM
TN TO TR TT TV TW TZ UA UG UM US UY UZ VA VC VE VG VI VN VU WF WS YE YT ZA ZM ZW XK""".split()
)

# Natural Earth features that have no usable ISO-A2 (not in ISO 3166-1, no
# MaxMind code): skipped, and counted in the header.
UNCODED = {"N. Cyprus", "Somaliland"}



def gx(lon):
    return round((lon + 180.0) * SCALE)


def gy(lat):
    return round((90.0 - lat) * SCALE)


def ring_area_centroid(ring):
    """Shoelace signed area and centroid of a lon/lat ring (planar)."""
    a = cx = cy = 0.0
    for (x0, y0), (x1, y1) in zip(ring, ring[1:]):
        w = x0 * y1 - x1 * y0
        a += w
        cx += (x0 + x1) * w
        cy += (y0 + y1) * w
    a *= 0.5
    if a == 0:
        x, y = ring[0]
        return 0.0, x, y
    return abs(a), cx / (6 * a), cy / (6 * a)


def ring_path(ring):
    pts = []
    for lon, lat in ring[:-1]:
        p = (gx(lon), gy(lat))
        if not pts or pts[-1] != p:
            pts.append(p)
    if len(pts) > 1 and pts[0] == pts[-1]:
        pts.pop()
    if len(pts) < 3:
        return ""
    body = " ".join("L%d %d" % p for p in pts[1:])
    return "M%d %d %s Z" % (pts[0][0], pts[0][1], body)


def polygons(geom):
    return [geom["coordinates"]] if geom["type"] == "Polygon" else geom["coordinates"]


def feature_code(p):
    code = p["ISO_A2_EH"]
    return p["ISO_A2"] if code == "-99" else code


def feature_centroid(f):
    """(lon, lat): LABEL_X/LABEL_Y if present, else largest-polygon centroid."""
    p = f["properties"]
    if p.get("LABEL_X") is not None and p.get("LABEL_Y") is not None:
        return p["LABEL_X"], p["LABEL_Y"]
    g = f["geometry"]
    if g["type"] == "Point":
        return tuple(g["coordinates"][:2])
    best = max(polygons(g), key=lambda poly: ring_area_centroid(poly[0])[0])
    _, lon, lat = ring_area_centroid(best[0])
    return lon, lat


def main(path, supplement_paths):
    raw = open(path, "rb").read()
    digest = hashlib.sha256(raw).hexdigest()
    feats = json.loads(raw.decode("utf-8"))["features"]
    rows = {}
    n_polys = 0
    n_degenerate = 0
    skipped = []
    for f in feats:
        p = f["properties"]
        code = feature_code(p)
        if code == "-99":
            skipped.append(p["NAME"])
            continue
        assert p["NAME"] not in UNCODED or code == "-99", p["NAME"]
        polys = polygons(f["geometry"])
        n_polys += len(polys)
        best = max(polys, key=lambda poly: ring_area_centroid(poly[0])[0])
        _, clon, clat = ring_area_centroid(best[0])
        paths = [s for s in (ring_path(poly[0]) for poly in polys) if s]
        n_degenerate += len(polys) - len(paths)
        assert code not in rows, code
        rows[code] = (p["NAME"], (gx(clon), gy(clat)), " ".join(paths))
    assert sorted(skipped) == sorted(UNCODED), skipped
    # Marker-only centroids for every code the 110m set lacks.
    wanted = {c for c in ISO_CODES if c not in rows}
    used = {}
    sup_meta = []
    for (label, fname), sp in zip(SUPPLEMENTS, supplement_paths):
        sraw = open(sp, "rb").read()
        sup_meta.append((label, fname, hashlib.sha256(sraw).hexdigest()))
        for f in json.loads(sraw.decode("utf-8"))["features"]:
            code = feature_code(f["properties"])
            if code in wanted and code not in used:
                lon, lat = feature_centroid(f)
                rows[code] = (f["properties"]["NAME"], (gx(lon), gy(lat)), "")
                used[code] = label
    no_marker = sorted(wanted - set(used))
    out = [
        "# snitchwatch world map: Natural Earth 1:110m admin-0 countries (public domain)",
        "# source: " + URL,
        "# tag: " + TAG,
        "# sha256: " + digest,
        "# licence: public domain, " + LICENCE_URL,
        "# grid: 3600x1800, 0.1 degree equirectangular, x=(lon+180)*10 y=(90-lat)*10",
        "# source_features: %d" % len(feats),
        "# source_polygons: %d" % n_polys,
        "# degenerate_polygons: %d" % n_degenerate,
        "# skipped_uncoded: %d" % len(skipped),
        "# no_shape: " + " ".join(sorted(used)),
        "# no_marker: " + " ".join(no_marker),
        "# format: CODE<TAB>NAME<TAB>CX,CY<TAB>PATH",
    ]
    for label, fname, sdigest in sup_meta:
        out.insert(
            5,
            "# supplement_%s: %s%s sha256 %s (codes: %s)"
            % (
                label,
                BASE,
                fname,
                sdigest,
                " ".join(sorted(c for c, l in used.items() if l == label)),
            ),
        )
    for code in sorted(rows):
        name, (x, y), d = rows[code]
        out.append("%s\t%s\t%d,%d\t%s" % (code, name, x, y, d))
    sys.stdout.write("\n".join(out) + "\n")


if __name__ == "__main__":
    if len(sys.argv) != 2 + len(SUPPLEMENTS):
        sys.exit("usage: gen_world_map.py <110m> <50m> <50m tiny> <10m> <10m subunits> geojson files")
    main(sys.argv[1], sys.argv[2:])

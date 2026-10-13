#!/usr/bin/env python3
"""Generate data/world-map.txt from Natural Earth 1:110m admin-0 countries.

Dev-time only; offline (never fetched at build or test time). Usage:

    python3 -I scripts/gen_world_map.py <ne_110m_admin_0_countries.geojson> \
        > data/world-map.txt

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
LICENCE_URL = "https://www.naturalearthdata.com/about/terms-of-use/"
SCALE = 10  # grid units per degree

# Natural Earth features that have no usable ISO-A2 (not in ISO 3166-1, no
# MaxMind code): skipped, and counted in the header.
UNCODED = {"N. Cyprus", "Somaliland"}

# Countries/territories the 110m set drops (too small). Hand-entered
# (name, lat, lon) approximate geographic centres, to within about a degree,
# from the CIA World Factbook "Geographic coordinates" entries (public domain,
# https://www.cia.gov/the-world-factbook/). Markers only need to land on the
# right spot of the map, not be surveyed.
MANUAL = {
    "AD": ("Andorra", 42.5, 1.5),
    "AG": ("Antigua and Barbuda", 17.05, -61.8),
    "AI": ("Anguilla", 18.25, -63.17),
    "AS": ("American Samoa", -14.33, -170.0),
    "AW": ("Aruba", 12.5, -69.97),
    "AX": ("Aland Islands", 60.22, 20.0),
    "BB": ("Barbados", 13.17, -59.53),
    "BH": ("Bahrain", 26.0, 50.55),
    "BL": ("Saint Barthelemy", 17.9, -62.83),
    "BM": ("Bermuda", 32.33, -64.75),
    "BQ": ("Bonaire, Sint Eustatius and Saba", 12.18, -68.25),
    "BV": ("Bouvet Island", -54.43, 3.4),
    "CC": ("Cocos (Keeling) Islands", -12.17, 96.83),
    "CK": ("Cook Islands", -21.23, -159.77),
    "CV": ("Cabo Verde", 16.0, -24.0),
    "CW": ("Curacao", 12.17, -69.0),
    "CX": ("Christmas Island", -10.5, 105.67),
    "DM": ("Dominica", 15.42, -61.33),
    "FM": ("Micronesia", 6.92, 158.25),
    "FO": ("Faroe Islands", 62.0, -6.78),
    "GD": ("Grenada", 12.12, -61.67),
    "GF": ("French Guiana", 4.0, -53.0),
    "GG": ("Guernsey", 49.47, -2.58),
    "GI": ("Gibraltar", 36.13, -5.35),
    "GP": ("Guadeloupe", 16.25, -61.58),
    "GS": ("South Georgia and South Sandwich Islands", -54.5, -37.0),
    "GU": ("Guam", 13.47, 144.78),
    "HK": ("Hong Kong", 22.25, 114.17),
    "HM": ("Heard Island and McDonald Islands", -53.1, 72.52),
    "IM": ("Isle of Man", 54.25, -4.5),
    "IO": ("British Indian Ocean Territory", -6.0, 71.5),
    "JE": ("Jersey", 49.25, -2.17),
    "KI": ("Kiribati", 1.42, 173.0),
    "KM": ("Comoros", -12.17, 44.25),
    "KN": ("Saint Kitts and Nevis", 17.33, -62.75),
    "KY": ("Cayman Islands", 19.5, -80.5),
    "LC": ("Saint Lucia", 13.88, -60.97),
    "LI": ("Liechtenstein", 47.27, 9.53),
    "MC": ("Monaco", 43.73, 7.4),
    "MF": ("Saint Martin", 18.08, -63.95),
    "MH": ("Marshall Islands", 9.0, 168.0),
    "MO": ("Macao", 22.17, 113.55),
    "MP": ("Northern Mariana Islands", 15.2, 145.75),
    "MQ": ("Martinique", 14.67, -61.0),
    "MS": ("Montserrat", 16.75, -62.2),
    "MT": ("Malta", 35.83, 14.58),
    "MU": ("Mauritius", -20.28, 57.55),
    "MV": ("Maldives", 3.25, 73.0),
    "NF": ("Norfolk Island", -29.03, 167.95),
    "NR": ("Nauru", -0.53, 166.92),
    "NU": ("Niue", -19.03, -169.87),
    "PF": ("French Polynesia", -15.0, -140.0),
    "PM": ("Saint Pierre and Miquelon", 46.83, -56.33),
    "PN": ("Pitcairn Islands", -25.07, -130.1),
    "PW": ("Palau", 7.5, 134.5),
    "RE": ("Reunion", -21.1, 55.6),
    "SC": ("Seychelles", -4.58, 55.67),
    "SG": ("Singapore", 1.37, 103.8),
    "SH": ("Saint Helena", -15.95, -5.7),
    "SJ": ("Svalbard and Jan Mayen", 78.0, 20.0),
    "SM": ("San Marino", 43.77, 12.42),
    "ST": ("Sao Tome and Principe", 1.0, 7.0),
    "SX": ("Sint Maarten", 18.03, -63.05),
    "TC": ("Turks and Caicos Islands", 21.75, -71.58),
    "TK": ("Tokelau", -9.0, -172.0),
    "TO": ("Tonga", -20.0, -175.0),
    "TV": ("Tuvalu", -8.0, 178.0),
    "UM": ("United States Minor Outlying Islands", 19.3, 166.63),
    "VA": ("Vatican City", 41.9, 12.45),
    "VC": ("Saint Vincent and the Grenadines", 13.25, -61.2),
    "VG": ("British Virgin Islands", 18.5, -64.5),
    "VI": ("US Virgin Islands", 18.33, -64.83),
    "WF": ("Wallis and Futuna", -13.3, -176.2),
    "WS": ("Samoa", -13.58, -172.33),
    "YT": ("Mayotte", -12.83, 45.17),
}


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


def main(path):
    raw = open(path, "rb").read()
    digest = hashlib.sha256(raw).hexdigest()
    feats = json.loads(raw.decode("utf-8"))["features"]
    rows = {}
    n_polys = 0
    n_degenerate = 0
    skipped = []
    for f in feats:
        p = f["properties"]
        code = p["ISO_A2_EH"]
        if code == "-99":
            code = p["ISO_A2"]
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
    for code, (name, lat, lon) in MANUAL.items():
        assert code not in rows, code
        rows[code] = (name, (gx(lon), gy(lat)), "")
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
        "# no_shape: " + " ".join(sorted(MANUAL)),
        "# format: CODE<TAB>NAME<TAB>CX,CY<TAB>PATH",
    ]
    for code in sorted(rows):
        name, (x, y), d = rows[code]
        out.append("%s\t%s\t%d,%d\t%s" % (code, name, x, y, d))
    sys.stdout.write("\n".join(out) + "\n")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: gen_world_map.py <ne_110m_admin_0_countries.geojson>")
    main(sys.argv[1])

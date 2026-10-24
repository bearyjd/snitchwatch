# Optional City geodata and observed geographic hierarchy

Snitchwatch retains bundled Country data. City data is optional: open the official
[DB-IP City Lite download page](https://db-ip.com/db/download/ip-to-city-lite),
download its MMDB archive in the browser, extract the `.gz`, and import the local
`.mmdb` in GeoIP settings. The app performs no background download and needs no
additional network permission. DB-IP Lite has reduced accuracy and coverage.
Positions describe database estimates, not measured endpoint locations, reputation,
ownership, traffic volume, or a physical address.

DB-IP data is [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
Required attribution: **IP Geolocation by [DB-IP](https://db-ip.com)**.
Snitchwatch does not modify the imported database. Its hierarchy groups observed
connection rows using normalized geographic names and canonical IP addresses.

## Selection and failure behavior

A successful import stores a private immutable `geo-import-<random>.mmdb` generation
in application data, then atomically replaces the small `geo-dataset` manifest.
The manifest retains the latest imported Country and City generations and selects one active profile. City misses therefore keep using the most recently imported Country data. Only that manifest makes the generation active. Failure before its commit preserves
the previous selection and its working file; the failed import's staged file is
removed. A manifest rename followed by directory-sync failure is reported as
committed success when the new manifest is visible, with a durability warning in
logs. Imported generations from previous successful imports remain available;
there is no automatic cleanup of user data.

Restart loads the selected generation once into the resolver shared by the
connections filters and geographic model. Country imports explicitly select
Country, including when a City file remains present. City imports select City;
missing City country records fall back to Country data. Missing or corrupt City
startup files leave Country operation available. `SNITCHWATCH_GEOIP_DB` retains
highest priority; `SNITCHWATCH_GEOIP_CITY_DB` has priority over a local selection
when no Country override is set. The existing manual Country search paths remain
unchanged; `dbip-city-lite.mmdb` in application data is the manual City fallback.
Legacy `geo-dataset` files containing `country` or `city` are supported.

Imports remain asynchronous, with one process-wide import at a time. All new MMDB
parsing, including metadata and record decoding, occurs in the early CLI child.
The parent bounds input, wall time and both output streams and reaps the child on
failure. Existing cycle, pointer, IP-depth and expanded-DAG guards remain active.
The search tree is checked against input length before its five-byte-per-node
work arrays are allocated.

## Explicit validation profiles

Metadata never promotes an input to a larger profile. The Country worker flag is
unchanged; City uses `--snitchwatch-validate-city-database`. Country rejects City
metadata, and City rejects Country metadata. Partially populated City records are
valid. Invalid country codes, excessive subdivision arrays, long/control-character
names, and non-finite/out-of-range optional coordinates are rejected. Raw names are bounded before surrounding whitespace is trimmed; embedded controls are rejected. The October 2026 publisher data contains a leading tab in the Spanish country name for Guadeloupe, so surrounding whitespace is normalized rather than rejecting the whole dataset.

| Limit | Country (unchanged) | City |
| --- | ---: | ---: |
| Input | 32 MiB | 256 MiB |
| Network records | 2,000,000 | 32,000,000 |
| Expanded graph visits | 8,000,001 | 128,000,001 |
| Worker data allocation | 256 MiB | 768 MiB |
| CPU | 30 s | 240 s |
| Parent wall time | 60 s | 300 s |

The real early-CLI worker passed the separately downloaded, SHA256-verified
October 2026 proof under these limits. An unoptimized developer build took
162.48 wall seconds (161.91 user / 0.08 system), with 230,888 KiB maximum RSS,
zero stderr, successful protocol result and exit 0. The compiled worker SHA256
was `688edaac0ce705b2d99f2811cbe0a1a4cdbdf452c0814d136c7750360fa9bbb7`.
A separately bounded compiled proof test measured **14,297,601 network records**
and **65,542,107 expanded graph visits**, tree height 128. Its validation took
160.142 seconds; including a second graph traversal and independent network count,
it took 162.50 wall / 161.85 user / 0.12 system seconds, with 236,020 KiB maximum
RSS. These measurements fit the explicit City profile; the former proposed
12M network / 48M traversal limits would reject this valid database. Timings
are measurements of this developer machine, not guaranteed import durations.
The proof is 126,998,165 bytes, metadata
`DBIP-City-Lite`, 14,297,794 nodes, record size 28, IPv6 with IPv4 support. Published
marketing record counts must not be used as MMDB CIDR/network counts.

Pinned proof provenance (developer validation only; blobs are ignored):

- Official archive: `https://download.db-ip.com/free/dbip-city-lite-2026-10.mmdb.gz`
- Archive SHA256: `c839d407f85a1e0bddba354cef16b2c398182d1c34ed8d985555dce134b36b14`
- Raw MMDB SHA256: `9e250f02722d1ad1780f88192f0e908af285478a33e76b88b349c9181ae9d0f5`
- Publisher MD5: `5966907981d3e82f338e823d1b83e002`; SHA1: `df17bd24390108ed1d24dcb7718dc60f6b9abd4f` (both refer to the raw MMDB).

## Consumer API (version 1)

`GeoModel.hierarchyJson()` exposes `GeoStore::hierarchy_json()` on demand;
`hierarchyChanged` signals changes. Existing per-country roles and
`clustersJson(scale, radius, gap)` retain their API. `dbCityAvailable` indicates
database capability, not that any particular connection has a city. `capacityReached`
and the snapshot's matching flag label incomplete geographic counts.

The hierarchy is a deterministic JSON object. `countries`, each country's
`cities`, and each city's `ips` are maps sorted by opaque stable keys:

```text
{ version: 1, observedConnections, capacityReached, countries: {
  countryKey: { key, code: string|null, name, counts, cities: {
    cityKey: { key, name, cityAvailable, place: object|null, counts, ips: {
      ipKey: { key, canonicalIp: string|null, address, coordinates: object|null,
        counts, connections: [{id, app, appPath, host, port, protocol, action}] }
    }}
  }}
}}
```

`counts` contains `total`, `allowed`, `denied`, `pending`; unknown verdicts only
increment total, matching country aggregation. Connections are sorted by ID.
Only observed cached rows are included. No traffic history/bytes or enrichment is
fabricated. Private/local and unknown destinations remain explicit branches.

Country keys are `country:<ISO code>`, `local`, and `unknown`. Missing-city keys are
`<countryKey>:unknown-city`. City keys are opaque `GeoPlace::city_key()` composites
of country, normalized subdivision path/name and exact finite coordinate identity;
there is no assumed geoname ID. Database restart may change keys. Exact-IP keys
are `ip:<canonical IpAddr>`, preserving distinct IPv4/IPv6 addresses and collapsing
alternate IPv6 spellings. Invalid destination strings have `canonicalIp: null`
and must not be used as an exact-IP filter.

`place` holds country, subdivisions, optional city/coordinates, provider and optional
precision text. Missing coordinates remain null; `(0,0)` is never substituted.
Several distinct observed IPs may retain one shared approximate city position:
they must be rendered as a shared anchor/stack, never physically spread apart.
The official sample IPv6 addresses `2001:4860:4860::8888` and
`2606:4700:4700::1111` both resolve to Montreal `(45.5019,-73.5674)`.

The resolver cache has at most 65,536 canonical IP entries. Geographic aggregation
accepts at most 100,000 tracked IDs; existing accepted rows still update/remove
normally after capacity is reached, and country/hierarchy counters use the same
accepted set. The flag stays set until Clear/snapshot reset, since skipped rows
cannot be reconstructed locally. The bridge's default cache is 10,000, with an
exception allowing pending rows to exceed its capacity.


## Prepared live-feed contract

`geo::prepared::GeoMessagePreparer::new(shared_resolver)` and
`prepare(connection_id, &ServerMessage)` produce `PreparedGeoMessage` with
`message: Option<ServerMessage>`, `resolutions: HashMap<connection ID, Resolved>`,
`reset`, `capacity_reached` and `activity_codes`. Input IDs are preserved;
`qualify_ids(connection_id)` adapts raw bridge IDs to the existing live list's
`<connection_id>:<id>` convention. Already-qualified consumers do not call it.

The preparer shadows at most 100,000 accepted IDs, processes every incoming row
in order and deduplicates the final row/fact per accepted ID. Late updates to
accepted IDs remain accurate after arbitrarily many overflow rows. Retained
rows/facts/removal IDs are each bounded by the accepted-ID limit. Clear and
session changes reset the shadow; stale older sessions do not rewind it.
The existing Qt session guard still discards obsolete queued work.

`GeoStore::apply_prepared` and live activity notifications only consume supplied
facts. They never decode after resolver-cache eviction. A missing prepared fact
is ignored as an invariant error rather than silently becoming Unknown or
performing a Qt-thread lookup. Geographic filters should likewise retain supplied
facts with live rows and exclude unavailable facts from all geographic scopes,
including Unknown. The raw unscoped connections list may retain more rows than
geographic aggregation; its partial-geography warning must remain visible.

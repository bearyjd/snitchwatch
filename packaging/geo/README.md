# Country data release input

Both Flatpak profiles include the same `dbip-country-lite.json` module. The
October 2026 DB-IP Country Lite MMDB is fetched at **build time** from the
official, month-specific HTTPS URL and verified against the recorded SHA-256.
The application has no background database updater and needs no API key or
runtime network grant to use this dataset. Data updates ship with app releases.
No database binary is stored in git.

`provenance.json` records the exact archive and extracted database sizes,
SHA-256 hashes, upstream source pages, actual MMDB metadata, and build timestamp.
The full CC BY 4.0 license and `IP Geolocation by DB-IP` attribution are installed
with the data. Database contents are unmodified. Lite data has reduced coverage
and accuracy; country context is not a threat verdict.

The official page's MD5 and SHA-1 identify the **extracted** MMDB, and both
matched the downloaded database. Packaging pins the compressed archive with
SHA-256; provenance also records SHA-256 of the extracted MMDB.

The GUI reports the active database source from MMDB metadata and its build date.
An imported DB-IP file keeps the historical user filename `GeoLite2-Country.mmdb`
for compatibility; its displayed provider remains DB-IP. Explicit environment
overrides, the existing user location, and existing system GeoIP locations all
precede the bundled fallback. The Flatpak fallback is
`/app/share/snitchwatch/dbip-country-lite.mmdb`. Native packages may install the
same verified file at `/usr/share/snitchwatch/dbip-country-lite.mmdb`, attribution
and license under `/usr/share/licenses/snitchwatch/dbip-country-lite/`, and
provenance beside the database. Native data packaging is optional and is not
implicitly installed by the source checkout.

To refresh a release:

1. Open the [official download page](https://db-ip.com/db/download/ip-to-country-lite)
   and verify the release, MMDB link, and current license. Follow the official
   link rather than deriving or trusting an unverified third-party URL.
2. Download with bounded HTTPS (for example `curl --fail --location --proto
   '=https' --tlsv1.2 --max-time 120 --max-filesize 16777216 -o country.mmdb.gz
   OFFICIAL_MONTHLY_URL`). Record SHA-256, decompress, and record the extracted
   SHA-256 and size. Keep these proof inputs outside git.
3. Inspect actual metadata and country decoding using the same `maxminddb`
   reader version as the application. Update the pin and provenance together,
   preserve attribution, and review any schema or licensing change.
4. Run `python3 packaging/geo/test-country-data.py` and the Geo unit/QML checks.
   The explicit external-artifact check for the current pin is:

   ```sh
   SNITCHWATCH_DBIP_PROOF=/absolute/path/to/dbip-country-lite.mmdb \
     cargo test -p snitchwatch-kirigami --lib --locked \
     dbip_official_file_compatibility -- --ignored
   ```

5. Build both Flatpak profiles and inspect the staged database, attribution,
   license, and provenance before publishing the application release. The
   offline shape tests do not establish a successful full Flatpak build.

Rollback is an application release rollback (Flatpak commit), or replacing the
native package with its previous release. User imports remain separate and
take priority. To return to packaged data, remove only the user-selected
override after backing it up and restart Snitchwatch.

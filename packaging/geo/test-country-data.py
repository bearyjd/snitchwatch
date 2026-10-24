#!/usr/bin/env python3
"""Offline assertions for the country-data distribution boundary."""
import hashlib
import json
import gzip
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]
GEO = ROOT / "packaging/geo"


class CountryData(unittest.TestCase):
    def test_release_inputs_are_complete_and_match_provenance(self):
        module = json.loads((GEO / "dbip-country-lite.json").read_text())
        provenance = json.loads((GEO / "provenance.json").read_text())
        self.assertEqual(module["sources"][0], {
            "type": "file",
            "url": provenance["archive_url"],
            "sha256": provenance["archive_sha256"],
        })
        self.assertEqual(module["sources"][1:], [
            {"type": "file", "path": name}
            for name in ["CC-BY-4.0.txt", "ATTRIBUTION.txt", "provenance.json"]
        ])
        for source in module["sources"][1:]:
            self.assertTrue((GEO / source["path"]).is_file())
        self.assertEqual(provenance["release"], "2026-10")
        self.assertEqual(provenance["archive_bytes"], 4123384)
        self.assertEqual(provenance["database_bytes"], 8348509)
        self.assertEqual(provenance["database_sha256"], "dbd70ccfa2a13627eaf4913d19920a1a229f1b3c5439ac01c509995b6364202a")
        self.assertEqual(provenance["license"], "CC-BY-4.0")
        self.assertEqual(provenance["license_text_sha256"], hashlib.sha256((GEO / "CC-BY-4.0.txt").read_bytes()).hexdigest())
        self.assertIn(provenance["attribution"], (GEO / "ATTRIBUTION.txt").read_text())

    def test_both_profiles_use_same_pinned_country_module_without_runtime_network(self):
        self.assertTrue((GEO / "dbip-country-lite.json").is_file(), "Missing pinned country module")
        module = json.loads((GEO / "dbip-country-lite.json").read_text())
        source = module["sources"][0]
        provenance = json.loads((GEO / "provenance.json").read_text())
        self.assertEqual(source["url"], "https://download.db-ip.com/free/dbip-country-lite-2026-10.mmdb.gz")
        self.assertEqual(source["sha256"], "4cd44536ddfd40fec7eca94a233f7d19f38d470e6522e653cb8d27001f5fe177")
        self.assertEqual(source["sha256"], provenance["archive_sha256"])
        self.assertEqual(provenance["metadata"]["database_type"], "DBIP-Country-Lite")
        self.assertEqual(provenance["metadata"]["build_epoch"], 1790818572)
        commands = "\n".join(module["build-commands"])
        for required in ["gzip -dc", "/app/share/snitchwatch/dbip-country-lite.mmdb", "CC-BY-4.0.txt", "ATTRIBUTION.txt", "provenance.json"]:
            self.assertIn(required, commands)
        for name in ["org.snitchwatch.Snitchwatch.yml", "org.snitchwatch.Snitchwatch.system.yml"]:
            manifest = yaml.safe_load((ROOT / "packaging/flatpak" / name).read_text())
            self.assertIn("../geo/dbip-country-lite.json", manifest["modules"])
            self.assertNotIn("--share=network", manifest["finish-args"])
        self.assertEqual(hashlib.sha256((GEO / "CC-BY-4.0.txt").read_bytes()).hexdigest(), "9ba9550ad48438d0836ddab3da480b3b69ffa0aac7b7878b5a0039e7ab429411")

    def test_attribution_and_lookup_paths_survive_packaging(self):
        self.assertTrue((GEO / "ATTRIBUTION.txt").is_file(), "Missing database attribution")
        attribution = (GEO / "ATTRIBUTION.txt").read_text()
        for required in ["IP Geolocation by DB-IP", "https://db-ip.com", "https://creativecommons.org/licenses/by/4.0/", "2026-10", "unmodified"]:
            self.assertIn(required, attribution)
        paths = (ROOT / "crates/snitchwatch-kirigami/src/geo/paths.rs").read_text()
        for path in ["/app/share/snitchwatch/dbip-country-lite.mmdb", "/usr/share/snitchwatch/dbip-country-lite.mmdb"]:
            self.assertIn(path, paths)

    def test_module_commands_stage_exact_bytes_and_redistribution_notices(self):
        module = json.loads((GEO / "dbip-country-lite.json").read_text())
        with tempfile.TemporaryDirectory(prefix="snitchwatch-country-stage-") as tmp:
            stage = Path(tmp)
            archive = stage / module["sources"][0]["url"].rsplit("/", 1)[1]
            database = b"synthetic build-input bytes; database validation is tested by Rust"
            archive.write_bytes(gzip.compress(database))
            for source in module["sources"][1:]:
                shutil.copyfile(GEO / source["path"], stage / source["path"])
            prefix = stage / "prefix"
            for command in module["build-commands"]:
                subprocess.run(command.replace("/app/", str(prefix) + "/"), shell=True, cwd=stage, check=True)
            self.assertEqual((prefix / "share/snitchwatch/dbip-country-lite.mmdb").read_bytes(), database)
            self.assertEqual((prefix / "share/snitchwatch/dbip-country-lite.mmdb").stat().st_mode & 0o777, 0o644)
            for name in ["CC-BY-4.0.txt", "ATTRIBUTION.txt"]:
                self.assertEqual((prefix / "share/licenses/snitchwatch/dbip-country-lite" / name).read_bytes(), (GEO / name).read_bytes())
            self.assertEqual((prefix / "share/snitchwatch/dbip-country-lite.provenance.json").read_bytes(), (GEO / "provenance.json").read_bytes())
            installed = {
                str(path.relative_to(prefix))
                for path in prefix.rglob("*") if path.is_file()
            }
            self.assertEqual(installed, {
                "share/snitchwatch/dbip-country-lite.mmdb",
                "share/snitchwatch/dbip-country-lite.provenance.json",
                "share/licenses/snitchwatch/dbip-country-lite/CC-BY-4.0.txt",
                "share/licenses/snitchwatch/dbip-country-lite/ATTRIBUTION.txt",
            })
            for path in prefix.rglob("*"):
                if path.is_file():
                    self.assertEqual(path.stat().st_mode & 0o777, 0o644)


if __name__ == "__main__":
    unittest.main()

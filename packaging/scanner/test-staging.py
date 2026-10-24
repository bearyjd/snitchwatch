#!/usr/bin/env python3
"""No privilege requests, live installation or execution of staged binaries."""
import hashlib
import os
import shutil
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET

HERE = Path(__file__).resolve().parent


class ScannerStaging(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.dest = self.root / "stage"
        self.marker = self.root / "must-never-run"
        self.binary = self.root / "fake-binary"
        self.binary.write_text(f"#!/bin/sh\ntouch '{self.marker}'\n")
        self.binary.chmod(0o755)
        self.digest = hashlib.sha256(self.binary.read_bytes()).hexdigest()

    def stage(self, dest=None, digest=None, broker=None, env=None):
        return subprocess.run(["bash", str(HERE / "stage.sh"), str(dest or self.dest),
                               str(self.binary), digest or self.digest,
                               str(broker or self.binary), self.digest], capture_output=True, text=True,
                              env={**os.environ, "TMPDIR": str(self.root), **(env or {})})

    def test_complete_idempotent_artifact_and_fixed_policy(self):
        for _ in range(2):
            result = self.stage()
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.marker.exists())
        helper = self.dest / "usr/libexec/snitchwatch-scanner-privileged"
        self.assertEqual(helper.read_bytes(), self.binary.read_bytes())
        self.assertEqual(stat.S_IMODE(helper.stat().st_mode), 0o755)
        policy = self.dest / "usr/share/polkit-1/actions/org.snitchwatch.scanner.policy"
        annotation = ET.parse(policy).find(".//annotate[@key='org.freedesktop.policykit.exec.path']")
        self.assertEqual(annotation.text, "/usr/libexec/snitchwatch-scanner-privileged")
        self.assertEqual(stat.S_IMODE(policy.stat().st_mode), 0o644)
        service = self.dest / "usr/share/dbus-1/services/org.snitchwatch.Scanner.service"
        self.assertIn("Exec=/usr/libexec/snitchwatch-scanner-broker", service.read_text())
        state = self.dest / "usr/lib/tmpfiles.d/snitchwatch-scanner.conf"
        self.assertIn("/var/lib/snitchwatch-scanner 0700 root root", state.read_text())
        if os.geteuid() == 0:
            self.assertEqual(helper.stat().st_uid, 0)
            self.assertEqual(policy.stat().st_uid, 0)

    def test_bad_hash_and_live_root_rejected(self):
        self.assertNotEqual(self.stage(digest="0" * 64).returncode, 0)
        self.assertFalse(self.dest.exists())
        self.assertEqual(list(self.root.glob("snitchwatch-scanner-stage.*")), [])
        self.assertNotEqual(self.stage(dest=Path("/")).returncode, 0)

    def test_mutated_inputs_cannot_replace_verified_bytes(self):
        original = self.binary.read_bytes()
        broker = self.root / "fake-broker"
        broker.write_bytes(original)
        broker.chmod(0o755)
        shim_dir = self.root / "shim"
        shim_dir.mkdir()
        shim = shim_dir / "sha256sum"
        # Run the real hash, then change that input before returning to stage.sh.
        # This deterministically exercises the gap between verification and copy.
        shim.write_text(r"""#!/usr/bin/env python3
import os
from pathlib import Path
import subprocess
import sys
subprocess.run([os.environ["REAL_SHA256SUM"], *sys.argv[1:]], check=True)
count_file = Path(os.environ["HASH_COUNT"])
count = int(count_file.read_text()) if count_file.exists() else 0
sources = [os.environ["SOURCE_SCANNER"], os.environ["SOURCE_BROKER"]]
Path(sources[count]).write_bytes(b"#!/bin/sh\nexit 42\n")
count_file.write_text(str(count + 1))
""")
        shim.chmod(0o755)
        count = self.root / "hash-count"
        result = self.stage(broker=broker, env={
            "PATH": f"{shim_dir}{os.pathsep}{os.environ['PATH']}",
            "REAL_SHA256SUM": shutil.which("sha256sum"),
            "HASH_COUNT": str(count), "SOURCE_SCANNER": str(self.binary),
            "SOURCE_BROKER": str(broker),
        })
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(count.read_text(), "2")
        for name in ("snitchwatch-scanner-privileged", "snitchwatch-scanner-broker"):
            staged = self.dest / "usr/libexec" / name
            self.assertEqual(staged.read_bytes(), original)
            self.assertEqual(hashlib.sha256(staged.read_bytes()).hexdigest(), self.digest)
        self.assertNotEqual(self.binary.read_bytes(), original)
        self.assertNotEqual(broker.read_bytes(), original)
        self.assertFalse(self.marker.exists())
        self.assertEqual(list(self.root.glob("snitchwatch-scanner-stage.*")), [])

    def test_symlink_cannot_escape_stage(self):
        self.dest.mkdir()
        escape = self.root / "outside"
        escape.mkdir()
        (self.dest / "usr").symlink_to(escape, target_is_directory=True)
        self.assertNotEqual(self.stage().returncode, 0)
        self.assertEqual(list(escape.iterdir()), [])


if __name__ == "__main__":
    unittest.main()

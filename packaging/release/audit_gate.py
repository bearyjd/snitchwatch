#!/usr/bin/env python3
"""Fail a release when a RustSec vulnerability affects the shipped bridge binary.

Usage:
  cargo audit --json > audit.json || true     # non-zero exit when anything is found
  audit_gate.py --repo DIR --audit-json audit.json

`cargo audit` reports on the whole Cargo.lock, which also holds the Tauri and
Kirigami shells' dependencies (quick-xml, quinn-proto, ...). The release tarball
ships only `snitchwatch-bridge-cli`, so this gate fails only on vulnerabilities
whose (name, version) is actually compiled into the release build — the same
crate set THIRD-PARTY-LICENSES.md lists (`bridge_artifact.shipped_crates`:
`cargo tree -p snitchwatch-bridge-cli --no-default-features`, so crates only the
GUI shells or the `web-ui` feature pull in don't count).
Everything else is printed as informational. Warnings (unmaintained, unsound,
yanked) never fail the gate. Missing or malformed audit JSON fails closed.

Stdlib only, Python >= 3.11. Contract:
docs/superpowers/plans/2026-10-03-bridge-release-artifact.md (decision L1).
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.dont_write_bytecode = True  # never leave __pycache__ in the checkout
sys.path.insert(0, str(Path(__file__).resolve().parent))

from bridge_artifact import Refusal, shipped_crates  # noqa: E402


def load_vulnerabilities(path: Path) -> list[dict]:
    try:
        report = json.loads(path.read_text())
        found = report["vulnerabilities"]["list"]
    except (OSError, ValueError, KeyError, TypeError) as exc:
        raise Refusal(f"cannot read cargo-audit JSON {path}: {exc!r}") from exc
    if not isinstance(found, list):
        raise Refusal(f"cargo-audit JSON {path}: vulnerabilities.list is not a list")
    return found


def describe(vuln: dict) -> tuple[tuple[str, str], str]:
    try:
        package, advisory = vuln["package"], vuln["advisory"]
        key = (package["name"], package["version"])
        patched = ", ".join(vuln.get("versions", {}).get("patched", [])) or "none"
        text = f"{advisory['id']} {key[0]} {key[1]} (patched: {patched}): {advisory['title']}"
    except (KeyError, TypeError) as exc:
        raise Refusal(f"unexpected cargo-audit vulnerability entry: {exc!r}") from exc
    return key, text


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Fail when a RustSec vulnerability affects the shipped bridge binary."
    )
    parser.add_argument("--repo", required=True, type=Path)
    parser.add_argument("--audit-json", required=True, type=Path)
    args = parser.parse_args()
    try:
        shipped = {(p["name"], p["version"]) for p in shipped_crates(args.repo.resolve())}
        entries = [describe(v) for v in load_vulnerabilities(args.audit_json)]
    except Refusal as exc:
        print(f"audit-gate: FAIL: {exc}", file=sys.stderr)
        return 1
    blocking = sorted(text for key, text in entries if key in shipped)
    for text in sorted(text for key, text in entries if key not in shipped):
        print(f"audit-gate: not in the shipped bridge (informational): {text}")
    for text in blocking:
        print(f"audit-gate: FAIL: shipped in snitchwatch-bridge-cli: {text}", file=sys.stderr)
    if blocking:
        return 1
    print(f"audit-gate: OK: no RustSec vulnerability in the {len(shipped)} shipped crates")
    return 0


if __name__ == "__main__":
    sys.exit(main())

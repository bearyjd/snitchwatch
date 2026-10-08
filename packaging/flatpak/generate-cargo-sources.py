#!/usr/bin/env python3
"""Run the pinned upstream generator and verify registry inputs against Cargo.lock."""

import argparse
import hashlib
import importlib.metadata
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify(lock, sources):
    expected = {}
    for package in lock["package"]:
        source = package.get("source")
        if source is None:
            continue
        if source != "registry+https://github.com/rust-lang/crates.io-index":
            raise ValueError(f"unsupported Cargo source: {source}")
        name, version = package["name"], package["version"]
        dest = f"cargo/vendor/{name}-{version}"
        expected[dest] = (
            f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
            package["checksum"],
        )

    archives, checksums = {}, {}
    configs = []
    for source in sources:
        dest = source.get("dest")
        if source["type"] == "archive":
            if dest in archives or source.get("archive-type") != "tar-gzip":
                raise ValueError("duplicate or unexpected archive")
            archives[dest] = (source["url"], source["sha256"])
        elif source["type"] == "inline":
            if source.get("dest-filename") == ".cargo-checksum.json":
                if dest in checksums:
                    raise ValueError("duplicate crate checksum")
                checksums[dest] = json.loads(source["contents"])
            elif dest == "cargo" and source.get("dest-filename") == "config":
                configs.append(tomllib.loads(source["contents"]))
            else:
                raise ValueError("unexpected inline source")
        else:
            raise ValueError(f"unexpected generated source type: {source['type']}")
    if archives != expected:
        raise ValueError("generated crate URLs/checksums do not match Cargo.lock")
    if checksums != {
        dest: {"package": checksum, "files": {}}
        for dest, (_, checksum) in expected.items()
    }:
        raise ValueError("generated Cargo checksum records do not match Cargo.lock")
    if configs != [{"source": {
        "vendored-sources": {"directory": "cargo/vendor"},
        "crates-io": {"replace-with": "vendored-sources"},
    }}]:
        raise ValueError("unexpected Cargo source replacement configuration")
    return len(expected)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--generator", required=True, type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    here = Path(__file__).resolve().parent
    root = here.parents[1]
    pin = json.loads((here / "cargo-generator-pin.json").read_text())
    if sha256(args.generator) != pin["sha256"]:
        parser.error("generator SHA256 does not match cargo-generator-pin.json")
    lock_path = root / "Cargo.lock"
    lock_hash = sha256(lock_path)
    output = args.output or here / "generated-cargo-sources.json"
    with tempfile.TemporaryDirectory(prefix="snitchwatch-cargo-sources-") as temp:
        generated = Path(temp) / "sources.json"
        command = [sys.executable, str(args.generator.resolve()),
                   str(lock_path), "-o", str(generated)]
        subprocess.run(command, cwd=root, check=True)
        count = verify(tomllib.loads(lock_path.read_text()),
                       json.loads(generated.read_text()))
        if sha256(lock_path) != lock_hash:
            raise ValueError("Cargo.lock changed while generating sources")
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_bytes(generated.read_bytes())
    receipt = {
        "generator": pin,
        "command": command,
        "cargo_lock_sha256": lock_hash,
        "generated_sources_sha256": sha256(output),
        "verified_registry_packages": count,
        "python": sys.version,
        "generator_dependencies": {
            name: importlib.metadata.version(name) for name in ["aiohttp", "tomlkit"]
        },
    }
    output.with_suffix(".provenance.json").write_text(
        json.dumps(receipt, indent=2) + "\n")
    print(f"Verified {count} registry inputs against Cargo.lock: {output}")


if __name__ == "__main__":
    main()

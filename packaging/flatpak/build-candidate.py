#!/usr/bin/env python3
"""Clean system-profile GUI build. CI-only, read-only credentials, fresh state."""
import argparse
import configparser
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request


APP = "org.snitchwatch.Snitchwatch"
WORKFLOW = ".github/workflows/candidate-gui.yml"
MANIFEST = "packaging/flatpak/org.snitchwatch.Snitchwatch.system.yml"


def run(*args, cwd=None):
    return subprocess.check_output(args, cwd=cwd, text=True).strip()


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n"


def export_git(repo, dest, revision, archive):
    with archive.open("wb") as stream:
        subprocess.run(["git", "-C", str(repo), "archive", revision], stdout=stream, check=True)
    with tarfile.open(archive) as tree:
        tree.extractall(dest, filter="data")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    sha = run("git", "rev-parse", "HEAD", cwd=root)
    if (os.environ["GITHUB_REPOSITORY"] != "bearyjd/snitchwatch"
            or os.environ["GITHUB_REF"] != "refs/heads/r13-candidate"
            or sha != os.environ["GITHUB_SHA"]
            or sha != os.environ["REVIEWED_SHA"]
            or os.environ["GITHUB_WORKFLOW_SHA"] != sha):
        raise ValueError("candidate source/ref/workflow mismatch")
    if run("git", "status", "--porcelain", "--untracked-files=all", cwd=root):
        raise ValueError("checkout is not clean")
    submodule = run("git", "rev-parse", "HEAD:vendor/opensnitch", cwd=root)
    if run("git", "rev-parse", "HEAD", cwd=root / "vendor/opensnitch") != submodule:
        raise ValueError("OpenSnitch checkout differs from source gitlink")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix="candidate-gui-", dir=os.environ["RUNNER_TEMP"]) as work:
        work = Path(work)
        source = work / "source"
        source.mkdir()
        export_git(root, source, sha, work / "source.tar")
        export_git(root / "vendor/opensnitch", source / "vendor/opensnitch", submodule,
                   work / "submodule.tar")
        for path in source.rglob("*"):
            if path.name in {"target", ".cargo", ".flatpak-builder"} or path.name == "snitchwatch-kirigami" and path.is_file():
                raise ValueError(f"developer configuration or cached binary in export: {path}")
        manifest = source / MANIFEST
        pin = json.loads((source / "packaging/flatpak/cargo-generator-pin.json").read_text())
        generator = work / "flatpak-cargo-generator.py"
        # The pinned wrapper verifies this download before executing it.
        with urllib.request.urlopen(pin["url"], timeout=60) as response:
            generator.write_bytes(response.read(2 * 1024 * 1024 + 1))
        if digest(generator) != pin["sha256"]:
            raise ValueError("generator hash mismatch")
        subprocess.run(["python3", str(source / "packaging/flatpak/generate-cargo-sources.py"),
                        "--generator", str(generator)], cwd=source, check=True)
        refs = ["runtime/org.kde.Platform/x86_64/6.11", "runtime/org.kde.Sdk/x86_64/6.11",
                "runtime/org.freedesktop.Sdk.Extension.rust-stable/x86_64/25.08"]
        # XDG_DATA_HOME and XDG_CONFIG_HOME are fresh, job-local directories.
        subprocess.run(["flatpak", "remote-add", "--user", "--if-not-exists", "flathub",
                        "https://flathub.org/repo/flathub.flatpakrepo"], check=True)
        subprocess.run(["flatpak", "install", "--user", "--noninteractive", "flathub", *refs], check=True)
        commits = {ref: run("flatpak", "info", "--user", "--show-commit", ref) for ref in refs}
        state, app, repo = work / "state", work / "app", work / "repo"
        command = ["flatpak-builder", "--user", "--arch=x86_64", "--disable-cache", "--keep-build-dirs",
                   "--default-branch=r13-candidate", f"--state-dir={state}", f"--repo={repo}",
                   str(app), str(manifest)]
        with (output / "build.log").open("w") as log:
            subprocess.run(command, cwd=source, stdout=log, stderr=subprocess.STDOUT, check=True)
        if commits != {ref: run("flatpak", "info", "--user", "--show-commit", ref) for ref in refs}:
            raise ValueError("Flatpak build inputs changed during build")
        binary = app / "files/bin/snitchwatch-kirigami"
        unstripped = state / "build/snitchwatch-kirigami/target/release/snitchwatch-kirigami"
        comment = run("readelf", "-p", ".comment", str(unstripped))
        if "mold" not in comment.lower():
            raise ValueError("freshly linked GUI does not record mold")
        header = run("readelf", "-h", str(binary))
        segments = run("readelf", "-W", "-l", str(binary))
        dynamic = run("readelf", "-W", "-d", str(binary))
        if "DYN (Position-Independent Executable file)" not in header or "GNU_RELRO" not in segments or "BIND_NOW" not in dynamic:
            raise ValueError("exported executable lacks PIE/RELRO/NOW")
        stack = next(line for line in segments.splitlines() if "GNU_STACK" in line)
        if "E" in stack.split()[-2]:
            raise ValueError("executable stack")
        if (app / "files/libexec/snitchwatch-build-tools").exists():
            raise ValueError("build-only protoc exported")
        metadata = (app / "metadata").read_bytes()
        config = configparser.ConfigParser(interpolation=None)
        config.read_string(metadata.decode())
        if config["Environment"].get("SNITCHWATCH_SYSTEM_BRIDGE") != "1" or "network" in config["Context"].get("shared", "").split(";"):
            raise ValueError("wrong system profile or network grant")
        for socket in ["/run/snitchwatch:ro", "/run/snitchwatch-auth:ro"]:
            if socket not in config["Context"].get("filesystems", "").split(";"):
                raise ValueError("missing read-only system transport permission")
        bundle = output / "snitchwatch-system-r13-candidate.flatpak"
        subprocess.run(["flatpak", "build-bundle", "--arch=x86_64", str(repo), str(bundle), APP,
                        "r13-candidate"], check=True)
        shutil.copyfile(manifest, output / "system-manifest.yml")
        (output / "metadata.ini").write_bytes(metadata)
        shutil.copyfile(app / "files/manifest.json", output / "resolved-manifest.json")
        generated = source / "packaging/flatpak/generated-cargo-sources.json"
        provenance = source / "packaging/flatpak/generated-cargo-sources.provenance.json"
        shutil.copyfile(generated, output / "generated-cargo-sources.json")
        shutil.copyfile(provenance, output / "generator-provenance.json")
        sdk_tools = {}
        for tool in ["rustc", "cargo", "mold", "gcc"]:
            executable = f"/usr/lib/sdk/rust-stable/bin/{tool}" if tool != "gcc" else "gcc"
            sdk_tools[tool] = run("flatpak-builder", "--run", str(app), str(manifest), executable, "--version")
        evidence = {
            "schema": 1, "profile": "system", "source_commit": sha,
            "source_tree": run("git", "rev-parse", "HEAD^{tree}", cwd=root),
            "submodule_commit": submodule, "source_archive_sha256": digest(work / "source.tar"),
            "submodule_archive_sha256": digest(work / "submodule.tar"),
            "cargo_lock_sha256": digest(source / "Cargo.lock"), "generator": pin,
            "flatpak_inputs": commits, "manifest_sha256": digest(manifest),
            "permissions_sha256": hashlib.sha256(metadata).hexdigest(),
            "binary_path": "/app/bin/snitchwatch-kirigami", "binary_sha256": digest(binary),
            "unstripped_binary_sha256": digest(unstripped), "linker_comment": comment,
            "hardening": {"header": header, "segments": segments, "dynamic": dynamic},
            "flatpak_commit": run("ostree", f"--repo={repo}", "rev-parse", f"app/{APP}/x86_64/r13-candidate"),
            "tools": {tool: run(tool, "--version") for tool in ["flatpak", "flatpak-builder", "readelf", "python3"]},
            "sdk_tools": sdk_tools,
            "generator_environment": run("python3", "-m", "pip", "freeze", "--all"),
            "build_script_sha256": digest(source / "packaging/flatpak/build-candidate.py"),
            "build_command": command, "workflow": WORKFLOW,
            "workflow_sha256": digest(root / WORKFLOW), "workflow_sha": os.environ["GITHUB_WORKFLOW_SHA"],
            "workflow_ref": os.environ["GITHUB_WORKFLOW_REF"], "run_id": os.environ["GITHUB_RUN_ID"],
            "run_attempt": os.environ["GITHUB_RUN_ATTEMPT"],
        }
        (output / "provenance.json").write_text(canonical(evidence))
        assets = {path.name: {"sha256": digest(path), "size": path.stat().st_size}
                  for path in sorted(output.iterdir())}
        packet = {"schema": 1, "repository": "bearyjd/snitchwatch", "target": "r13-candidate",
                  "ref": os.environ["GITHUB_REF"], "source_commit": sha,
                  "workflow_sha": os.environ["GITHUB_WORKFLOW_SHA"],
                  "workflow_ref": os.environ["GITHUB_WORKFLOW_REF"],
                  "run_id": os.environ["GITHUB_RUN_ID"], "run_attempt": os.environ["GITHUB_RUN_ATTEMPT"],
                  "profile": "system", "assets": assets}
        packet_file = output / "candidate-packet.json"
        packet_file.write_text(canonical(packet))
        with open(os.environ["GITHUB_OUTPUT"], "a") as step_output:
            step_output.write(f"packet_sha256={digest(packet_file)}\n")


if __name__ == "__main__":
    main()

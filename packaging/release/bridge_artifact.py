#!/usr/bin/env python3
"""Build info, deterministic packing and verification of the bridge release tarball.

Subcommands (contract: docs/superpowers/plans/2026-10-03-bridge-release-artifact.md,
"Consumer contract" and "Interfaces"):

  version    --repo DIR
  buildinfo  --repo DIR --binary PATH --builder-image REF --rustflags-template STR
  thirdparty --repo DIR --out FILE
  pack       --stage DIR --name snitchwatch-bridge --version V --arch A
             --git-commit SHA [--git-tag T | --dirty] --source-date-epoch N
             --meta FILE --out DIR
  verify     --tarball PATH [--expect-version V] [--expect-sha256 HEX]
             [--check-ldd] [--run-flags] [--extract-to DIR]

Stdlib only, Python >= 3.11. `pack` never mutates the stage and never trusts its
modes, owners or mtimes: every tar header is built by hand.

`verify` never extracts with a tar implementation. It gunzips (bounded) into
memory, checks every member header, then requires the uncompressed tar stream to
be byte-identical to the canonical serialization of its own contents, so no
extractor can see anything the checks did not (appended members, members after a
corrupt header, pax/xattr headers, ...). `--extract-to` writes the verified
in-memory tree; `--check-ldd`/`--run-flags` execute the binary and so need
`--expect-sha256` from a trusted source.
"""

from __future__ import annotations

import sys

if sys.version_info < (3, 11):
    sys.exit("bridge_artifact.py needs Python >= 3.11 (tomllib)")

import argparse  # noqa: E402
import gzip  # noqa: E402
import hashlib  # noqa: E402
import io  # noqa: E402
import json  # noqa: E402
import os  # noqa: E402
import re  # noqa: E402
import shutil  # noqa: E402
import stat  # noqa: E402
import subprocess  # noqa: E402
import tarfile  # noqa: E402
import tempfile  # noqa: E402
import tomllib  # noqa: E402
import zlib  # noqa: E402
from pathlib import Path, PurePosixPath  # noqa: E402
from typing import NamedTuple  # noqa: E402

NAME = "snitchwatch-bridge"
BIN_PATH = "usr/bin/snitchwatch-bridge-cli"
UNIT_PATH = "usr/lib/systemd/user/snitchwatch-bridge.service"
LICENSE_DIR = f"usr/share/licenses/{NAME}"
DATA_DIR = f"usr/share/{NAME}"
MANIFEST_PATH = f"{DATA_DIR}/MANIFEST.json"
SUMS_PATH = f"{DATA_DIR}/SHA256SUMS"
GENERATED = frozenset({MANIFEST_PATH, SUMS_PATH})
# Every directory below the top dir; the top dir itself is "".
DIRS = (
    "usr",
    "usr/bin",
    "usr/lib",
    "usr/lib/systemd",
    "usr/lib/systemd/user",
    "usr/share",
    "usr/share/licenses",
    LICENSE_DIR,
    DATA_DIR,
)
ALLOWED_NEEDED = frozenset({"libgcc_s.so.1", "libm.so.6", "libc.so.6", "ld-linux-x86-64.so.2"})
INSTALL = {
    "binary": f"/{BIN_PATH}",
    "unit": f"/{UNIT_PATH}",
    "unit_name": "snitchwatch-bridge.service",
    "unit_scope": "user",
}
# --no-default-features drops the `web-ui` feature: the vendored web/ frontend is
# Objective Development's GPL-2.0-only code and must not ship in this binary
# (plan decision I). WEB_UI_MARKER is the copyright line every web/ file carries;
# verify fails if it appears in the binary at all.
RELEASE_PACKAGE_ARGS = ("-p", "snitchwatch-bridge-cli", "--no-default-features")
BUILD_COMMAND = "cargo build --release --locked " + " ".join(RELEASE_PACKAGE_ARGS)
WEB_UI_MARKER = b"Objective Development Software GmbH"
# The toolchain RPMs whose NEVRAs are recorded by name. The image digest pins
# only the base layer (dnf pulls live updates), so `rpm -qa` is recorded too.
RPMS = (
    "rust",
    "cargo",
    "gcc",
    "glibc",
    "protobuf-compiler",
    "binutils",
    "glibc-devel",
    "libgcc",
    "llvm-libs",
    "zlib-ng-compat",
    "python3",
)
META_KEYS = ("build", "upstream", "runtime")
MANIFEST_KEYS = frozenset(
    {"schema_version", "name", "version", "arch", "source", "install", "files"} | set(META_KEYS)
)
SOURCE_KEYS = frozenset({"git_commit", "git_tag", "dirty", "source_date_epoch"})
BUILD_STR_KEYS = ("builder_image", "command", "rustflags", "rustc_vv", "cargo_version")
BUILD_KEYS = frozenset(
    {*BUILD_STR_KEYS, "protoc_version", "rpms", "rpm_qa", "rpm_qa_sha256", "cargo_lock_sha256"}
)
UPSTREAM_KEYS = frozenset({"opensnitch_commit", "opensnitch_tag", "ui_proto_sha256"})
RUNTIME_KEYS = frozenset({"needed", "glibc_min"})
FILE_KEYS = frozenset({"path", "mode", "size", "sha256"})
USTAR_MAX_MTIME = 8**11 - 1
RUN_FLAGS_TIMEOUT_S = 5
MAX_COMPRESSED = 64 * 1024 * 1024
MAX_UNCOMPRESSED = 256 * 1024 * 1024
MAX_SIDECAR = 4096

THIRDPARTY_ROOT = "snitchwatch-bridge-cli"
THIRDPARTY_TARGET = "x86_64-unknown-linux-gnu"
LICENSE_PREFIXES = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE")
THIRDPARTY_NAME = "THIRD-PARTY-LICENSES.md"

# All validation uses fullmatch: `$` would also accept a trailing newline.
VERSION_RE = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.+-]+)?")
ARCH_RE = re.compile(r"[a-z0-9_]+")
SHA1_RE = re.compile(r"[0-9a-f]{40}")
SHA256_RE = re.compile(r"[0-9a-f]{64}")
GLIBC_MIN_RE = re.compile(r"[0-9]+\.[0-9]+")
IMAGE_RE = re.compile(r"[^\s@]+@sha256:[0-9a-f]{64}")
LICENSE_NAME_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*")
TARBALL_RE = re.compile(rf"{NAME}-(?P<version>.+)-(?P<arch>[a-z0-9_]+)\.tar\.gz")


class Refusal(Exception):
    """A user-facing failure, printed as one `<command>: FAIL: <reason>` line."""


class Verified(NamedTuple):
    """A tarball that passed every check, held in memory."""

    top: str
    contents: dict[str, bytes]
    mtime: int


# --------------------------------------------------------------------------- helpers


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def canonical_json(obj: object) -> bytes:
    """Sorted keys, 2-space indent, trailing newline (the MANIFEST format)."""
    return (json.dumps(obj, indent=2, sort_keys=True) + "\n").encode()


def mode_for(rel: str) -> int:
    return 0o755 if rel == BIN_PATH else 0o644


def top_dir(version: str, arch: str) -> str:
    return f"{NAME}-{version}-{arch}"


def is_license(rel: str) -> bool:
    path = PurePosixPath(rel)
    return str(path.parent) == LICENSE_DIR and bool(LICENSE_NAME_RE.fullmatch(path.name))


def is_hex(value: object, length: int) -> bool:
    regex = SHA1_RE if length == 40 else SHA256_RE
    return isinstance(value, str) and regex.fullmatch(value) is not None


def nonempty_str(value: object) -> bool:
    return isinstance(value, str) and value != ""


def as_dict(value: object) -> dict:
    return value if isinstance(value, dict) else {}


def run(cmd: list[str]) -> str:
    """Run `cmd` with LC_ALL=C and return stdout; raise Refusal on any failure."""
    env = {**os.environ, "LC_ALL": "C"}
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, env=env, check=False)
    except OSError as exc:
        raise Refusal(f"cannot run {cmd[0]}: {exc}") from exc
    if proc.returncode != 0:
        detail = proc.stderr.strip() or proc.stdout.strip()
        raise Refusal(f"`{' '.join(cmd)}` exited {proc.returncode}: {detail}")
    return proc.stdout


def load_toml(path: Path) -> dict:
    try:
        with path.open("rb") as fh:
            return tomllib.load(fh)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise Refusal(f"cannot read {path}: {exc}") from exc


def read_bytes(path: Path) -> bytes:
    try:
        return path.read_bytes()
    except OSError as exc:
        raise Refusal(f"cannot read {path}: {exc}") from exc


def read_capped(path: Path, limit: int) -> bytes:
    """Read at most `limit` bytes; refuse a larger file instead of reading it all."""
    try:
        with path.open("rb") as fh:
            data = fh.read(limit + 1)
    except OSError as exc:
        raise Refusal(f"cannot read {path}: {exc}") from exc
    if len(data) > limit:
        raise Refusal(f"{path.name} is larger than {limit} bytes; refusing to read it")
    return data


def write_atomic(path: Path, data: bytes) -> None:
    tmp = path.with_name(f".{path.name}.tmp")
    tmp.write_bytes(data)
    os.replace(tmp, path)


# --------------------------------------------------------------------------- version


def read_version(repo: Path) -> str:
    """The snitchwatch-bridge-cli crate version (resolving `version.workspace`)."""
    crate = load_toml(repo / "crates/snitchwatch-bridge-cli/Cargo.toml")
    version = as_dict(crate.get("package")).get("version")
    if isinstance(version, dict) and version.get("workspace") is True:
        workspace = load_toml(repo / "Cargo.toml")
        version = as_dict(as_dict(workspace.get("workspace")).get("package")).get("version")
    if not isinstance(version, str) or not VERSION_RE.fullmatch(version):
        raise Refusal(f"cannot determine a valid snitchwatch-bridge-cli version (got {version!r})")
    return version


def cmd_version(args: argparse.Namespace) -> int:
    print(read_version(Path(args.repo)))
    return 0


# --------------------------------------------------------------------------- buildinfo


def read_pins(repo: Path) -> dict[str, str]:
    path = repo / "packaging/release/pins.env"
    try:
        lines = path.read_text().splitlines()
    except OSError as exc:
        raise Refusal(f"cannot read {path}: {exc}") from exc
    pins = {}
    for line in (raw.strip() for raw in lines):
        key, sep, value = line.partition("=")
        if sep and not line.startswith("#"):
            pins[key] = value
    return pins


def rpm_nevra(name: str) -> str:
    found = run(["rpm", "-q", "--qf", "%{NEVRA}\n", name]).split()
    if len(found) != 1:
        raise Refusal(f"expected exactly one installed {name} RPM, found {found}")
    return found[0]


def rpm_qa() -> list[str]:
    """Every installed RPM's NEVRA, sorted (the whole build environment)."""
    return sorted(line for line in run(["rpm", "-qa", "--qf", "%{NEVRA}\n"]).splitlines() if line)


def rpm_qa_digest(nevras: list[str]) -> str:
    return sha256_hex(("\n".join(nevras) + "\n").encode())


def elf_needed(binary: Path) -> list[str]:
    out = run(["readelf", "-d", str(binary)])
    return sorted(re.findall(r"\(NEEDED\)\s+Shared library: \[([^\]]+)\]", out))


def glibc_min(binary: Path) -> str:
    """Highest GLIBC_x.y symbol version the binary references (compared numerically)."""
    out = run(["objdump", "-T", str(binary)])
    versions = {
        tuple(int(part) for part in match.split("."))
        for match in re.findall(r"GLIBC_([0-9]+(?:\.[0-9]+)+)", out)
    }
    if not versions:
        raise Refusal(f"no GLIBC_ symbol versions found in {binary}")
    return ".".join(str(part) for part in max(versions))


def upstream_info(repo: Path) -> dict:
    pins = read_pins(repo)
    commit = run(["git", "-C", str(repo / "vendor/opensnitch"), "rev-parse", "HEAD"]).strip()
    if commit != pins.get("SW_OPENSNITCH_COMMIT"):
        raise Refusal(
            f"vendor/opensnitch is at {commit}, pins.env SW_OPENSNITCH_COMMIT is "
            f"{pins.get('SW_OPENSNITCH_COMMIT')}"
        )
    return {
        "opensnitch_commit": commit,
        "opensnitch_tag": pins.get("SW_OPENSNITCH_TAG"),
        "ui_proto_sha256": sha256_hex(read_bytes(repo / "vendor/opensnitch/proto/ui.proto")),
    }


def cmd_buildinfo(args: argparse.Namespace) -> int:
    repo, binary = Path(args.repo), Path(args.binary)
    installed = rpm_qa()
    meta = {
        "build": {
            "builder_image": args.builder_image,
            "command": BUILD_COMMAND,
            "rustflags": args.rustflags_template,
            "rustc_vv": run(["rustc", "-vV"]).rstrip("\n"),
            "cargo_version": run(["cargo", "-V"]).rstrip("\n"),
            "protoc_version": run(["protoc", "--version"]).strip(),
            "rpms": {name: rpm_nevra(name) for name in RPMS},
            "rpm_qa": installed,
            "rpm_qa_sha256": rpm_qa_digest(installed),
            "cargo_lock_sha256": sha256_hex(read_bytes(repo / "Cargo.lock")),
        },
        "upstream": upstream_info(repo),
        "runtime": {"needed": elf_needed(binary), "glibc_min": glibc_min(binary)},
    }
    fails = check_meta_sections(meta)
    if fails:
        raise Refusal("the recorded build info is invalid: " + "; ".join(fails))
    sys.stdout.buffer.write(canonical_json(meta))
    return 0


# --------------------------------------------------------------------------- thirdparty


def cargo_metadata(repo: Path) -> dict:
    out = run(
        [
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--offline",
            "--filter-platform",
            THIRDPARTY_TARGET,
            "--manifest-path",
            str(repo / "Cargo.toml"),
        ]
    )
    try:
        return as_dict(json.loads(out))
    except ValueError as exc:
        raise Refusal(f"cargo metadata printed invalid JSON: {exc}") from exc


def normal_closure(metadata: dict) -> list[dict]:
    """Third-party packages in the NORMAL (non-dev, non-build) dependency closure of
    snitchwatch-bridge-cli, sorted by (name, version). Workspace crates are walked
    through but not listed."""
    try:
        packages = {pkg["id"]: pkg for pkg in metadata["packages"]}
        members = set(metadata["workspace_members"])
        nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
        roots = [pid for pid in members if packages[pid]["name"] == THIRDPARTY_ROOT]
        if len(roots) != 1:
            raise Refusal(f"expected one workspace package {THIRDPARTY_ROOT!r}, found {roots}")
        seen: set[str] = set()
        pending = roots
        while pending:
            pid = pending.pop()
            if pid in seen:
                continue
            seen.add(pid)
            for dep in nodes[pid]["deps"]:
                if any(kind["kind"] is None for kind in dep["dep_kinds"]):
                    pending.append(dep["pkg"])
        return sorted((packages[pid] for pid in seen - members), key=crate_key)
    except (KeyError, TypeError) as exc:
        raise Refusal(f"unexpected `cargo metadata` shape: {exc!r}") from exc


def release_crate_set(repo: Path) -> set[tuple[str, str]]:
    """(name, version) of every crate in the release build's normal dependency graph.

    `cargo metadata`'s resolve is feature-unified across the whole workspace, so
    `normal_closure` alone over-includes crates that only another member (the
    Tauri/Kirigami shells) or the default `web-ui` feature enables. `cargo tree`
    for exactly the release package and features (RELEASE_PACKAGE_ARGS, the same
    as BUILD_COMMAND) resolves what is actually built."""
    out = run(
        [
            "cargo",
            "tree",
            *RELEASE_PACKAGE_ARGS,
            "-e",
            "normal",
            "--target",
            THIRDPARTY_TARGET,
            "--locked",
            "--offline",
            "--prefix",
            "none",
            "--format",
            "{p}",
            "--manifest-path",
            str(repo / "Cargo.toml"),
        ]
    )
    crates = set()
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[1].startswith("v"):
            crates.add((parts[0], parts[1][1:]))
    if not crates:
        raise Refusal("`cargo tree` listed no crates for the release build")
    return crates


def shipped_crates(repo: Path) -> list[dict]:
    """Third-party packages actually compiled into the release binary, sorted."""
    exact = release_crate_set(repo)
    return [pkg for pkg in normal_closure(cargo_metadata(repo)) if crate_key(pkg) in exact]


def crate_key(pkg: dict) -> tuple[str, str]:
    return (pkg["name"], pkg["version"])


def is_license_name(name: str) -> bool:
    return name.upper().startswith(LICENSE_PREFIXES)


def regular_files(path: Path, label: str) -> list[tuple[str, bytes]]:
    """(label, bytes) for a regular file, or for each regular file directly inside a
    directory (REUSE-style LICENSES/). Symlinks and anything else are not followed."""
    mode = path.lstat().st_mode
    if stat.S_ISREG(mode):
        return [(label, read_bytes(path))]
    if not stat.S_ISDIR(mode):
        return []
    found = []
    for name in sorted(os.listdir(path)):
        if stat.S_ISREG((path / name).lstat().st_mode):
            found.append((f"{label}/{name}", read_bytes(path / name)))
    return found


def license_texts(pkg: dict) -> list[tuple[str, bytes]]:
    """Every LICENSE*/LICENCE*/COPYING*/NOTICE*/UNLICENSE* file (case-insensitive) in
    the crate dir, plus a declared `license-file`, sorted by relative name."""
    crate_dir = Path(pkg["manifest_path"]).parent
    found: dict[str, bytes] = {}
    for name in sorted(os.listdir(crate_dir)):
        if is_license_name(name):
            found.update(regular_files(crate_dir / name, name))
    declared = pkg.get("license_file")
    if isinstance(declared, str):
        rel = PurePosixPath(declared)
        if not rel.is_absolute() and ".." not in rel.parts and (crate_dir / rel).exists():
            found.update(regular_files(crate_dir / rel, rel.as_posix()))
    return sorted(found.items())


def fenced(text: str) -> list[str]:
    """`text` verbatim in a code fence longer than any backtick run inside it."""
    longest = max((len(run_) for run_ in re.findall(r"`+", text)), default=0)
    fence = "`" * max(3, longest + 1)
    body = text if text.endswith("\n") else text + "\n"
    return [f"{fence}text", body.rstrip("\n"), fence]


def render_crate(pkg: dict, files: list[tuple[str, bytes]]) -> list[str]:
    lines = [f"## {pkg['name']} {pkg['version']}", ""]
    lines.append(f"- License (SPDX): {pkg.get('license') or 'not declared in Cargo.toml'}")
    if pkg.get("license_file"):
        lines.append(f"- License file (declared): `{pkg['license_file']}`")
    if pkg.get("repository"):
        lines.append(f"- Repository: <{pkg['repository']}>")
    if not files:
        lines += [
            "",
            "This crate's source package ships no LICENSE*, LICENCE*, COPYING*, NOTICE* "
            "or UNLICENSE* file; its license is the SPDX expression above.",
        ]
    for name, data in files:
        lines += ["", f"### {pkg['name']} {pkg['version']}: {name}", ""]
        lines += fenced(data.decode("utf-8", "replace"))
    return lines + [""]


def render_thirdparty(crates: list[dict]) -> str:
    lines = [
        "# Third-party licenses: snitchwatch-bridge-cli",
        "",
        f"`snitchwatch-bridge-cli` statically links the {len(crates)} third-party Rust crates "
        f"below (the normal dependency closure for {THIRDPARTY_TARGET}, resolved from "
        "Cargo.lock). Each entry gives the crate's SPDX license expression and the verbatim "
        "text of every license/notice file in its source package.",
        "",
        "Generated by `packaging/release/bridge_artifact.py thirdparty`; do not edit.",
        "",
    ]
    for pkg in crates:
        lines += render_crate(pkg, license_texts(pkg))
    return "\n".join(lines)


def cmd_thirdparty(args: argparse.Namespace) -> int:
    repo = Path(args.repo).resolve()
    crates = shipped_crates(repo)
    if not crates:
        raise Refusal(f"{THIRDPARTY_ROOT} has no third-party dependencies; refusing an empty file")
    text = render_thirdparty(crates)
    local = {str(repo)} | {str(Path(pkg["manifest_path"]).parent.parent) for pkg in crates}
    leaked = sorted(path for path in local if path in text)
    if leaked:
        raise Refusal(f"{THIRDPARTY_NAME} would contain build-machine paths: {leaked}")
    write_atomic(Path(args.out), text.encode())
    print(f"thirdparty: {len(crates)} crates -> {args.out}", file=sys.stderr)
    return 0


# --------------------------------------------------------------------------- MANIFEST checks
# Shared by `pack` (the --meta sections) and `verify` (the whole MANIFEST). Every
# check returns a list of failure strings and never raises on malformed input.


def check_keys(value: object, keys: frozenset[str], where: str) -> list[str]:
    if not isinstance(value, dict):
        return [f"MANIFEST {where} is {type(value).__name__}, want an object"]
    if set(value) != keys:
        return [f"MANIFEST {where} keys {sorted(value)} != {sorted(keys)}"]
    return []


def check_rpms(rpms: object, installed: object) -> list[str]:
    fails = check_keys(rpms, frozenset(RPMS), "build.rpms")
    if fails:
        return fails
    for name in RPMS:
        nevra = rpms[name]
        if not nonempty_str(nevra) or not nevra.startswith(f"{name}-"):
            fails.append(f"MANIFEST build.rpms.{name} {nevra!r} is not a {name} NEVRA")
        elif isinstance(installed, list) and nevra not in installed:
            fails.append(f"MANIFEST build.rpms.{name} {nevra!r} is not in build.rpm_qa")
    return fails


def check_rpm_qa(installed: object, digest: object) -> list[str]:
    if not isinstance(installed, list) or not installed:
        return ["MANIFEST build.rpm_qa is not a non-empty list"]
    if not all(nonempty_str(nevra) and "\n" not in nevra for nevra in installed):
        return ["MANIFEST build.rpm_qa entries must be non-empty single-line strings"]
    if installed != sorted(installed):
        return ["MANIFEST build.rpm_qa is not sorted"]
    if digest != rpm_qa_digest(installed):
        return [f"MANIFEST build.rpm_qa_sha256 {digest!r} != sha256 of build.rpm_qa"]
    return []


def check_build(build: object) -> list[str]:
    fails = check_keys(build, BUILD_KEYS, "build")
    if fails:
        return fails
    for key in (*BUILD_STR_KEYS, "protoc_version"):
        if not nonempty_str(build[key]):
            fails.append(f"MANIFEST build.{key} is not a non-empty string")
    if build["command"] != BUILD_COMMAND:
        fails.append(f"MANIFEST build.command {build['command']!r} != {BUILD_COMMAND!r}")
    if isinstance(build["builder_image"], str) and not IMAGE_RE.fullmatch(build["builder_image"]):
        fails.append(
            f"MANIFEST build.builder_image {build['builder_image']!r} is not digest-pinned"
        )
    for key in ("cargo_lock_sha256", "rpm_qa_sha256"):
        if not is_hex(build[key], 64):
            fails.append(f"MANIFEST build.{key} {build[key]!r} is not 64 lowercase hex")
    fails += check_rpm_qa(build["rpm_qa"], build["rpm_qa_sha256"])
    return fails + check_rpms(build["rpms"], build["rpm_qa"])


def check_upstream(upstream: object) -> list[str]:
    fails = check_keys(upstream, UPSTREAM_KEYS, "upstream")
    if fails:
        return fails
    if not is_hex(upstream["opensnitch_commit"], 40):
        fails.append(
            f"MANIFEST upstream.opensnitch_commit {upstream['opensnitch_commit']!r} "
            "is not 40 lowercase hex"
        )
    if not nonempty_str(upstream["opensnitch_tag"]):
        fails.append("MANIFEST upstream.opensnitch_tag is not a non-empty string")
    if not is_hex(upstream["ui_proto_sha256"], 64):
        fails.append(
            f"MANIFEST upstream.ui_proto_sha256 {upstream['ui_proto_sha256']!r} "
            "is not 64 lowercase hex"
        )
    return fails


def check_runtime(runtime: object) -> list[str]:
    fails = check_keys(runtime, RUNTIME_KEYS, "runtime")
    if fails:
        return fails
    needed = runtime["needed"]
    if not isinstance(needed, list) or not all(isinstance(lib, str) for lib in needed):
        fails.append(f"MANIFEST runtime.needed {needed!r} is not a list of strings")
    elif needed != sorted(set(needed)):
        fails.append(f"MANIFEST runtime.needed {needed} is not sorted and unique")
    elif not set(needed) <= ALLOWED_NEEDED:
        extra = sorted(set(needed) - ALLOWED_NEEDED)
        fails.append(
            f"MANIFEST runtime.needed lists libraries outside {sorted(ALLOWED_NEEDED)}: {extra}"
        )
    glibc = runtime["glibc_min"]
    if not isinstance(glibc, str) or not GLIBC_MIN_RE.fullmatch(glibc):
        fails.append(f"MANIFEST runtime.glibc_min {glibc!r} is not <major>.<minor>")
    return fails


def check_meta_sections(obj: dict) -> list[str]:
    return (
        check_build(obj.get("build"))
        + check_upstream(obj.get("upstream"))
        + check_runtime(obj.get("runtime"))
    )


def check_source(source: object, version: str) -> list[str]:
    fails = check_keys(source, SOURCE_KEYS, "source")
    if fails:
        return fails
    if not is_hex(source["git_commit"], 40):
        fails.append(f"MANIFEST source.git_commit {source['git_commit']!r} is not 40 hex")
    tag, dirty = source["git_tag"], source["dirty"]
    if tag is not None and tag != f"v{version}":
        fails.append(f"MANIFEST source.git_tag {tag!r} is neither null nor 'v{version}'")
    if not isinstance(dirty, bool):
        fails.append(f"MANIFEST source.dirty {dirty!r} is not a boolean")
    elif tag is not None and dirty:
        fails.append(f"MANIFEST source.git_tag {tag!r} on a dirty tree (a tag means clean)")
    sde = source["source_date_epoch"]
    if type(sde) is not int or not 0 <= sde <= USTAR_MAX_MTIME:
        fails.append(f"MANIFEST source.source_date_epoch {sde!r} is not an integer in range")
    return fails


def check_manifest_header(manifest: dict, version: str, arch: str) -> list[str]:
    fails = []
    if set(manifest) != MANIFEST_KEYS:
        fails.append(f"MANIFEST keys {sorted(manifest)} != {sorted(MANIFEST_KEYS)}")
    expected = {
        "schema_version": 1,
        "name": NAME,
        "version": version,
        "arch": arch,
        "install": INSTALL,
    }
    for key, want in expected.items():
        if manifest.get(key) != want or type(manifest.get(key)) is not type(want):
            fails.append(f"MANIFEST {key} is {manifest.get(key)!r}, want {want!r}")
    return fails


def check_manifest(manifest: dict, version: str, arch: str) -> list[str]:
    return (
        check_manifest_header(manifest, version, arch)
        + check_source(manifest.get("source"), version)
        + check_meta_sections(manifest)
    )


# --------------------------------------------------------------------------- pack


def validate_pack_args(args: argparse.Namespace) -> None:
    if args.name != NAME:
        raise Refusal(f"--name must be {NAME!r}, got {args.name!r}")
    if not VERSION_RE.fullmatch(args.version):
        raise Refusal(f"--version {args.version!r} is not a semantic version")
    if not ARCH_RE.fullmatch(args.arch):
        raise Refusal(f"--arch {args.arch!r} is not a plain architecture name")
    if not SHA1_RE.fullmatch(args.git_commit):
        raise Refusal(f"--git-commit {args.git_commit!r} is not 40 lowercase hex characters")
    if args.git_tag is not None and args.git_tag != f"v{args.version}":
        raise Refusal(f"--git-tag {args.git_tag!r} does not match version (want 'v{args.version}')")
    if args.git_tag is not None and args.dirty:
        raise Refusal("--git-tag and --dirty are mutually exclusive: a tagged release is clean")
    if not 0 <= args.source_date_epoch <= USTAR_MAX_MTIME:
        raise Refusal(f"--source-date-epoch {args.source_date_epoch} is out of range")
    stage, out = Path(args.stage).resolve(), Path(args.out).resolve()
    if out.is_relative_to(stage):
        raise Refusal(f"--out {out} must not be inside --stage {stage}")


def load_meta(path: Path) -> dict:
    try:
        meta = json.loads(read_bytes(path))
    except ValueError as exc:
        raise Refusal(f"--meta {path} is not valid JSON: {exc}") from exc
    if not isinstance(meta, dict) or set(meta) != set(META_KEYS):
        raise Refusal(f"--meta must be an object with exactly the keys {list(META_KEYS)}")
    fails = check_meta_sections(meta)
    if fails:
        raise Refusal(f"--meta {path} is invalid: " + "; ".join(fails))
    return meta


def walk_stage(stage: Path):
    """Yield (relative posix path, lstat result) for every entry, never following links."""

    def fail(exc: OSError) -> None:
        raise Refusal(f"cannot read stage: {exc}")

    for dirpath, dirnames, filenames in os.walk(stage, onerror=fail):
        for name in sorted(dirnames + filenames):
            full = Path(dirpath, name)
            yield full.relative_to(stage).as_posix(), full.lstat()


def collect_stage(stage: Path) -> dict[str, bytes]:
    """Read the stage into memory, refusing anything outside the allowed layout."""
    if not stage.is_dir():
        raise Refusal(f"--stage {stage} is not a directory")
    files: dict[str, bytes] = {}
    for rel, st in walk_stage(stage):
        if stat.S_ISDIR(st.st_mode):
            if rel not in DIRS:
                raise Refusal(f"unexpected directory in stage: {rel}/")
        elif not stat.S_ISREG(st.st_mode):
            raise Refusal(
                f"stage entry {rel} is not a regular file (symlinks/special files refused)"
            )
        elif rel in GENERATED:
            raise Refusal(f"stage must not contain {rel}; pack generates it")
        elif rel not in (BIN_PATH, UNIT_PATH) and not is_license(rel):
            raise Refusal(f"unexpected file in stage: {rel}")
        else:
            files[rel] = read_bytes(stage / rel)
    for required in (BIN_PATH, UNIT_PATH):
        if required not in files:
            raise Refusal(f"stage is missing {required}")
    if not any(is_license(rel) for rel in files):
        raise Refusal(f"stage has no license files under {LICENSE_DIR}/")
    return files


def build_manifest(args: argparse.Namespace, meta: dict, files: dict[str, bytes]) -> bytes:
    entries = [
        {"path": rel, "mode": f"{mode_for(rel):04o}", "size": len(data), "sha256": sha256_hex(data)}
        for rel, data in sorted(files.items())
    ]
    manifest = {
        "schema_version": 1,
        "name": NAME,
        "version": args.version,
        "arch": args.arch,
        "source": {
            "git_commit": args.git_commit,
            "git_tag": args.git_tag,
            "dirty": args.dirty,
            "source_date_epoch": args.source_date_epoch,
        },
        **{key: meta[key] for key in META_KEYS},
        "install": INSTALL,
        "files": entries,
    }
    return canonical_json(manifest)


def sums_text(contents: dict[str, bytes]) -> bytes:
    """`<sha256>  <path>` for every file except SHA256SUMS itself, sorted by path."""
    return "".join(
        f"{sha256_hex(data)}  {rel}\n" for rel, data in sorted(contents.items()) if rel != SUMS_PATH
    ).encode()


def tar_member(name: str, data: bytes | None, mode: int, mtime: int) -> tarfile.TarInfo:
    info = tarfile.TarInfo(name)
    info.type = tarfile.DIRTYPE if data is None else tarfile.REGTYPE
    info.mode = mode
    info.size = 0 if data is None else len(data)
    info.mtime = mtime
    info.uid = info.gid = 0
    info.uname = info.gname = "root"
    return info


def tar_bytes(top: str, contents: dict[str, bytes], mtime: int) -> bytes:
    """The canonical uncompressed USTAR stream: sorted members (dirs included),
    root-owned, fixed mtime, contract modes. `verify` requires a tarball's tar stream
    to be byte-identical to this function applied to its own contents."""
    members: dict[str, bytes | None] = {top: None}
    members.update({f"{top}/{rel}": None for rel in DIRS})
    members.update({f"{top}/{rel}": data for rel, data in contents.items()})
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.USTAR_FORMAT) as tar:
        for name in sorted(members):
            data = members[name]
            mode = 0o755 if data is None else mode_for(name[len(top) + 1 :])
            payload = None if data is None else io.BytesIO(data)
            tar.addfile(tar_member(name, data, mode, mtime), payload)
    return raw.getvalue()


def gzip_bytes(data: bytes) -> bytes:
    """gzip with mtime 0 and no file name in the header."""
    raw = io.BytesIO()
    with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0, compresslevel=9) as gz:
        gz.write(data)
    return raw.getvalue()


def cmd_pack(args: argparse.Namespace) -> int:
    validate_pack_args(args)
    meta = load_meta(Path(args.meta))
    files = collect_stage(Path(args.stage))
    contents = {**files, MANIFEST_PATH: build_manifest(args, meta, files)}
    contents[SUMS_PATH] = sums_text(contents)
    top = top_dir(args.version, args.arch)
    blob = gzip_bytes(tar_bytes(top, contents, args.source_date_epoch))
    if len(blob) > MAX_COMPRESSED:
        raise Refusal(f"the tarball would be {len(blob)} bytes; verify refuses > {MAX_COMPRESSED}")
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    tarball = out / f"{top}.tar.gz"
    write_atomic(tarball, blob)
    write_atomic(out / f"{tarball.name}.sha256", f"{sha256_hex(blob)}  {tarball.name}\n".encode())
    print(tarball)
    return 0


# --------------------------------------------------------------------------- verify


def validate_verify_args(args: argparse.Namespace) -> None:
    if args.expect_sha256 is not None and not SHA256_RE.fullmatch(args.expect_sha256):
        raise Refusal("--expect-sha256 must be 64 lowercase hex characters")
    if (args.check_ldd or args.run_flags) and args.expect_sha256 is None:
        raise Refusal(
            "--check-ldd/--run-flags run the tarball's binary (ldd loads it, --run-flags "
            "executes it); pass --expect-sha256 with the digest from a trusted source"
        )


def parse_tarball_name(path: Path, expect_version: str | None) -> tuple[str, str]:
    match = TARBALL_RE.fullmatch(path.name)
    if not match or not VERSION_RE.fullmatch(match["version"]):
        raise Refusal(f"tarball name {path.name!r} is not {NAME}-<version>-<arch>.tar.gz")
    version, arch = match["version"], match["arch"]
    if expect_version is not None and version != expect_version:
        raise Refusal(f"tarball version {version!r} != --expect-version {expect_version!r}")
    return version, arch


def check_sidecar(path: Path, blob: bytes) -> list[str]:
    sidecar = path.with_name(f"{path.name}.sha256")
    try:
        text = read_capped(sidecar, MAX_SIDECAR).decode()
    except (Refusal, UnicodeDecodeError) as exc:
        return [f"missing or unreadable {sidecar.name}: {exc}"]
    expected = f"{sha256_hex(blob)}  {path.name}\n"
    if text != expected:
        return [f"{sidecar.name} is {text!r}, want {expected!r}"]
    return []


def gunzip_bounded(blob: bytes) -> bytes:
    """Decompress exactly one gzip member, never more than MAX_UNCOMPRESSED bytes.
    Trailing bytes (incl. a second gzip member, which GNU gzip would concatenate but
    Python's single-member decoder would not) are refused."""
    decoder = zlib.decompressobj(wbits=31)
    try:
        raw = decoder.decompress(blob, MAX_UNCOMPRESSED + 1)
    except zlib.error as exc:
        raise Refusal(f"not a valid gzip stream: {exc}") from exc
    if len(raw) > MAX_UNCOMPRESSED:
        raise Refusal(f"decompresses to more than {MAX_UNCOMPRESSED} bytes; refusing it")
    if not decoder.eof:
        raise Refusal("the gzip stream is truncated")
    if decoder.unused_data:
        raise Refusal(
            f"{len(decoder.unused_data)} bytes of trailing data after the gzip member "
            "(concatenated gzip members/appended data are refused)"
        )
    return raw


def check_member(member: tarfile.TarInfo, top: str) -> list[str]:
    name = member.name
    if name.startswith("/") or ".." in PurePosixPath(name).parts:
        return [f"unsafe member path {name!r}"]
    if name != top and not name.startswith(f"{top}/"):
        return [f"member {name!r} is outside the single top dir {top}/"]
    if member.type not in (tarfile.REGTYPE, tarfile.DIRTYPE):
        return [f"member {name!r} is not a regular file or directory (type {member.type!r})"]
    if member.pax_headers:
        return [f"member {name!r} carries pax extended headers {sorted(member.pax_headers)}"]
    owner = (member.uid, member.gid, member.uname, member.gname)
    if owner != (0, 0, "root", "root"):
        return [f"member {name!r} is owned by {owner}, want (0, 0, 'root', 'root')"]
    return []


def check_members(members: list[tarfile.TarInfo], top: str) -> list[str]:
    """Validate every member header before any member's data is read."""
    fails: list[str] = []
    seen: set[str] = set()
    for member in members:
        fails += check_member(member, top)
        if member.name in seen:
            fails.append(f"duplicate member {member.name!r}")
        seen.add(member.name)
    return fails


def check_layout(by_rel: dict[str, tarfile.TarInfo], top: str) -> list[str]:
    fails = []
    dirs = {rel for rel, m in by_rel.items() if m.isdir()}
    files = set(by_rel) - dirs
    want_dirs = {"", *DIRS}
    if dirs != want_dirs:
        missing, extra = sorted(want_dirs - dirs), sorted(dirs - want_dirs)
        fails.append(f"directories: missing {missing}, unexpected {extra}")
    required = {BIN_PATH, UNIT_PATH, *GENERATED}
    if required - files:
        fails.append(f"missing files: {sorted(required - files)}")
    unexpected = sorted(rel for rel in files - required if not is_license(rel))
    if unexpected:
        fails.append(f"unexpected files: {unexpected}")
    if not any(is_license(rel) for rel in files):
        fails.append(f"no license files under {LICENSE_DIR}/")
    for rel, member in sorted(by_rel.items()):
        want = 0o755 if member.isdir() else mode_for(rel)
        if member.mode != want:
            fails.append(f"{rel or top + '/'}: mode {member.mode:04o}, want {want:04o}")
    return fails


def load_manifest(raw: bytes) -> tuple[dict, list[str]]:
    try:
        manifest = json.loads(raw)
    except ValueError as exc:
        return {}, [f"MANIFEST.json is not valid JSON: {exc}"]
    if not isinstance(manifest, dict):
        return {}, ["MANIFEST.json is not a JSON object"]
    if raw != canonical_json(manifest):
        return manifest, [
            "MANIFEST.json is not canonical (sorted keys, 2-space indent, final newline)"
        ]
    return manifest, []


def check_file_entry(entry: object, actual: dict[str, dict]) -> list[str]:
    fails = check_keys(entry, FILE_KEYS, "files[] entry")
    if fails:
        return fails
    path = entry["path"]
    if not isinstance(path, str):
        return [f"MANIFEST files[] path {path!r} is not a string"]
    if type(entry["size"]) is not int:
        fails.append(f"{path}: MANIFEST size {entry['size']!r} is not an integer")
    if not is_hex(entry["sha256"], 64):
        fails.append(f"{path}: MANIFEST sha256 {entry['sha256']!r} is not 64 lowercase hex")
    want = actual.get(path)
    if want is None or path in GENERATED:
        return fails  # reported by the path-list comparison
    for key in sorted(FILE_KEYS):
        if entry[key] != want[key]:
            fails.append(f"{path}: MANIFEST {key} {entry[key]!r} != actual {want[key]!r}")
    return fails


def check_manifest_files(manifest: dict, actual: dict[str, dict]) -> list[str]:
    entries = manifest.get("files")
    if not isinstance(entries, list):
        return ["MANIFEST files is not a list"]
    fails = []
    listed = [as_dict(entry).get("path") for entry in entries]
    expected = sorted(rel for rel in actual if rel not in GENERATED)
    if listed != expected:
        fails.append(f"MANIFEST files[] paths {listed} != tarball files {expected} (sorted)")
    for entry in entries:
        fails += check_file_entry(entry, actual)
    return fails


def check_sums(raw: bytes, actual: dict[str, dict]) -> list[str]:
    text = raw.decode("utf-8", "replace")
    listed_rels = sorted(rel for rel in actual if rel != SUMS_PATH)
    expected = "".join(f"{actual[rel]['sha256']}  {rel}\n" for rel in listed_rels)
    if text == expected:
        return []
    pairs = (line.split("  ", 1) for line in text.splitlines() if "  " in line)
    listed = {rel: digest for digest, rel in pairs}
    bad = sorted(
        {rel for rel in actual if rel != SUMS_PATH and listed.get(rel) != actual[rel]["sha256"]}
        | {rel for rel in listed if rel not in actual or rel == SUMS_PATH}
    )
    return [f"SHA256SUMS does not match the tarball (mismatched: {bad or 'formatting/order'})"]


def check_mtimes(members: list[tarfile.TarInfo], sde: object) -> list[str]:
    wrong = [member.name for member in members if member.mtime != sde or type(sde) is not int]
    if wrong:
        return [f"member mtimes != MANIFEST source.source_date_epoch {sde!r}: {wrong}"]
    return []


def describe_noncanonical(raw: bytes, canonical: bytes) -> str:
    if len(raw) > len(canonical) and raw.startswith(canonical):
        return (
            f"{len(raw) - len(canonical)} bytes after the canonical end-of-archive "
            "(appended members/data that some extractors would still read)"
        )
    block = next(
        (
            i
            for i in range(0, min(len(raw), len(canonical)), 512)
            if raw[i : i + 512] != canonical[i : i + 512]
        ),
        min(len(raw), len(canonical)) // 512 * 512,
    )
    return (
        f"first differing 512-byte block at offset {block} ({len(raw)} vs {len(canonical)} bytes)"
    )


def check_canonical(raw: bytes, top: str, contents: dict[str, bytes], sde: object) -> list[str]:
    """The backstop against parser differentials: what any extractor reads must be
    exactly what these checks saw."""
    if type(sde) is not int or not 0 <= sde <= USTAR_MAX_MTIME:
        return ["cannot re-serialize the tar stream: invalid MANIFEST source.source_date_epoch"]
    canonical = tar_bytes(top, contents, sde)
    if raw == canonical:
        return []
    return [
        "the uncompressed tar stream is not the canonical serialization of its own "
        f"contents: {describe_noncanonical(raw, canonical)}"
    ]


def file_facts(by_rel: dict[str, tarfile.TarInfo], contents: dict[str, bytes]) -> dict[str, dict]:
    """path -> {path, mode (from the tar header), size, sha256 (of the member bytes)}."""
    return {
        rel: {
            "path": rel,
            "mode": f"{by_rel[rel].mode:04o}",
            "size": len(data),
            "sha256": sha256_hex(data),
        }
        for rel, data in contents.items()
    }


def check_ldd(binary: Path, manifest: dict) -> list[str]:
    try:
        ldd_out = run(["ldd", str(binary)])
        needed = elf_needed(binary)
        glibc = glibc_min(binary)
    except Refusal as exc:
        return [f"--check-ldd: {exc}"]
    fails = []
    missing = [line.strip() for line in ldd_out.splitlines() if "not found" in line]
    if missing:
        fails.append(f"ldd reports missing libraries: {missing}")
    extra = sorted(set(needed) - ALLOWED_NEEDED)
    if extra:
        fails.append(f"binary NEEDs libraries outside {sorted(ALLOWED_NEEDED)}: {extra}")
    runtime = as_dict(manifest.get("runtime"))
    if needed != runtime.get("needed"):
        fails.append(f"readelf NEEDED {needed} != MANIFEST runtime.needed {runtime.get('needed')}")
    if glibc != runtime.get("glibc_min"):
        fails.append(
            f"binary needs GLIBC_{glibc} but MANIFEST runtime.glibc_min is "
            f"{runtime.get('glibc_min')!r}"
        )
    return fails


def isolated_env(tmp: str) -> dict[str, str]:
    """A minimal env that points every state/runtime path the binary might use into `tmp`."""
    return {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "LC_ALL": "C",
        "HOME": tmp,
        "XDG_RUNTIME_DIR": tmp,
        "XDG_STATE_HOME": tmp,
        "XDG_DATA_HOME": tmp,
        "XDG_CONFIG_HOME": tmp,
        "XDG_CACHE_HOME": tmp,
        "SNITCHWATCH_WS_SOCKET": f"{tmp}/bridge.sock",
        "SNITCHWATCH_GRPC_BIND": "127.0.0.1:0",
    }


def run_flag(binary: Path, flag: str, tmp: str) -> tuple[str, list[str]]:
    try:
        proc = subprocess.run(
            [str(binary), flag],
            env=isolated_env(tmp),
            cwd=tmp,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            timeout=RUN_FLAGS_TIMEOUT_S,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return "", [f"{flag} did not exit within {RUN_FLAGS_TIMEOUT_S}s (did it start a bridge?)"]
    except OSError as exc:
        return "", [f"cannot run {binary.name} {flag}: {exc}"]
    stdout = proc.stdout.decode("utf-8", "replace")
    return stdout, [f"{flag} exited {proc.returncode}"] if proc.returncode != 0 else []


def check_run_flags(binary: Path, version: str) -> list[str]:
    tmp = tempfile.mkdtemp(prefix="swv", dir="/tmp")
    try:
        out, fails = run_flag(binary, "--version", tmp)
        want = f"snitchwatch-bridge-cli {version}\n"
        if out != want:
            fails.append(f"--version printed {out[:200]!r}, want {want!r}")
        out, help_fails = run_flag(binary, "--help", tmp)
        fails += help_fails
        if "SNITCHWATCH_GRPC_BIND" not in out:
            fails.append(f"--help output does not mention SNITCHWATCH_GRPC_BIND: {out[:200]!r}")
        leftovers = sorted(os.listdir(tmp))
        if leftovers:
            fails.append(f"--version/--help did I/O: temp dir not empty afterwards: {leftovers}")
        return fails
    finally:
        shutil.rmtree(tmp)


def check_execution(binary_bytes: bytes, manifest: dict, version: str, args) -> list[str]:
    """ldd/--version/--help on the verified binary bytes, written to a private dir."""
    tmp = tempfile.mkdtemp(prefix="swbin", dir="/tmp")
    try:
        binary = Path(tmp, "snitchwatch-bridge-cli")
        binary.write_bytes(binary_bytes)
        binary.chmod(0o755)
        fails = check_ldd(binary, manifest) if args.check_ldd else []
        return fails + (check_run_flags(binary, version) if args.run_flags else [])
    finally:
        shutil.rmtree(tmp)


def read_tar(raw: bytes, top: str) -> tuple[list[str], list[tarfile.TarInfo], dict[str, bytes]]:
    """Parse member headers from memory, check them, then read regular-file bytes
    (keyed by path relative to the top dir). Nothing touches the filesystem."""
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:") as tar:
        members = tar.getmembers()
        fails = check_members(members, top)
        if fails:
            return fails, members, {}
        contents = {}
        for member in members:
            if member.isreg():
                handle = tar.extractfile(member)
                contents[member.name[len(top) + 1 :]] = handle.read() if handle else b""
    return [], members, contents


def check_contents(
    raw: bytes, top: str, members: list[tarfile.TarInfo], contents: dict[str, bytes], name
) -> tuple[list[str], dict]:
    version, arch = name
    by_rel = {("" if m.name == top else m.name[len(top) + 1 :]): m for m in members}
    fails = check_layout(by_rel, top)
    if fails:
        return fails, {}
    manifest, fails = load_manifest(contents[MANIFEST_PATH])
    actual = file_facts(by_rel, contents)
    sde = as_dict(manifest.get("source")).get("source_date_epoch")
    fails += check_manifest(manifest, version, arch)
    fails += check_manifest_files(manifest, actual)
    fails += check_sums(contents[SUMS_PATH], actual)
    fails += check_mtimes(members, sde)
    fails += check_canonical(raw, top, contents, sde)
    fails += check_no_web_ui(contents[BIN_PATH])
    return fails, manifest


def check_no_web_ui(binary_bytes: bytes) -> list[str]:
    """The release binary must not embed the GPL-2.0-only web/ frontend."""
    if WEB_UI_MARKER in binary_bytes:
        return [
            "the binary embeds the vendored web/ UI (GPL-2.0-only); release builds must use"
            " --no-default-features"
        ]
    return []


def verify_tarball(args: argparse.Namespace) -> tuple[list[str], Verified | None]:
    path = Path(args.tarball)
    blob = read_capped(path, MAX_COMPRESSED)
    if args.expect_sha256 is not None and sha256_hex(blob) != args.expect_sha256:
        return [
            f"{path.name} has sha256 {sha256_hex(blob)}, --expect-sha256 is {args.expect_sha256}"
        ], None
    version, arch = parse_tarball_name(path, args.expect_version)
    top = top_dir(version, arch)
    fails = check_sidecar(path, blob)
    try:
        raw = gunzip_bounded(blob)
        tar_fails, members, contents = read_tar(raw, top)
    except Refusal as exc:
        return fails + [str(exc)], None
    except (tarfile.TarError, EOFError, OSError) as exc:
        return fails + [f"cannot read {path.name} as a tar stream: {exc}"], None
    if tar_fails:
        return fails + tar_fails, None
    content_fails, manifest = check_contents(raw, top, members, contents, (version, arch))
    fails += content_fails
    if fails:  # never execute or extract anything that failed a check
        return fails, None
    if args.check_ldd or args.run_flags:
        fails = check_execution(contents[BIN_PATH], manifest, version, args)
    return fails, None if fails else Verified(
        top, contents, manifest["source"]["source_date_epoch"]
    )


def extract_tree(parent: Path, tree: Verified) -> Path:
    """Write the verified in-memory tree to <parent>/<top> with the contract modes and
    mtimes, via a private sibling dir renamed into place (never merged into an
    existing directory)."""
    final = parent / tree.top
    try:
        parent.mkdir(parents=True, exist_ok=True)
        if final.exists() or final.is_symlink():
            raise Refusal(f"--extract-to: {final} already exists; refusing to merge into it")
        staging = Path(tempfile.mkdtemp(prefix=f".{tree.top}.", dir=parent))
    except OSError as exc:
        raise Refusal(f"--extract-to: {exc}") from exc
    try:
        write_tree(staging, tree)
        os.rename(staging, final)
    except OSError as exc:
        shutil.rmtree(staging, ignore_errors=True)
        raise Refusal(f"--extract-to: cannot write {final}: {exc}") from exc
    return final


def write_tree(root: Path, tree: Verified) -> None:
    for rel in sorted(DIRS):
        (root / rel).mkdir()
    for rel, data in sorted(tree.contents.items()):
        with open(root / rel, "xb") as fh:
            fh.write(data)
        os.chmod(root / rel, mode_for(rel))
        os.utime(root / rel, (tree.mtime, tree.mtime))
    for rel in [*sorted(DIRS, reverse=True), ""]:
        os.chmod(root / rel, 0o755)
        os.utime(root / rel, (tree.mtime, tree.mtime))


def cmd_verify(args: argparse.Namespace) -> int:
    validate_verify_args(args)
    fails, tree = verify_tarball(args)
    for reason in fails:
        print(f"verify: FAIL: {reason}", file=sys.stderr)
    if fails or tree is None:
        return 1
    if args.extract_to:
        print(f"verify: extracted: {extract_tree(Path(args.extract_to), tree)}")
    print(f"verify: OK: {args.tarball}")
    return 0


# --------------------------------------------------------------------------- cli


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)

    version = sub.add_parser("version", help="print the snitchwatch-bridge-cli version")
    version.add_argument("--repo", required=True)
    version.set_defaults(func=cmd_version)

    info = sub.add_parser("buildinfo", help="print the build/upstream/runtime meta JSON")
    for flag in ("--repo", "--binary", "--builder-image", "--rustflags-template"):
        info.add_argument(flag, required=True)
    info.set_defaults(func=cmd_buildinfo)

    third = sub.add_parser("thirdparty", help=f"write {THIRDPARTY_NAME} for the bridge binary")
    third.add_argument("--repo", required=True)
    third.add_argument("--out", required=True)
    third.set_defaults(func=cmd_thirdparty)

    pack = sub.add_parser("pack", help="write the deterministic tarball + .sha256")
    for flag in ("--stage", "--name", "--version", "--arch", "--git-commit", "--meta", "--out"):
        pack.add_argument(flag, required=True)
    pack.add_argument("--git-tag")
    pack.add_argument("--dirty", action="store_true")
    pack.add_argument("--source-date-epoch", type=int, required=True)
    pack.set_defaults(func=cmd_pack)

    verify = sub.add_parser("verify", help="verify a tarball against the consumer contract")
    verify.add_argument("--tarball", required=True)
    verify.add_argument("--expect-version")
    verify.add_argument("--expect-sha256", help="trusted sha256 of the tarball (checked first)")
    verify.add_argument("--check-ldd", action="store_true", help="needs --expect-sha256")
    verify.add_argument("--run-flags", action="store_true", help="needs --expect-sha256")
    verify.add_argument("--extract-to", help="after every check passes, write DIR/<top>")
    verify.set_defaults(func=cmd_verify)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return args.func(args)
    except (Refusal, OSError) as exc:
        print(f"{args.command}: FAIL: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())

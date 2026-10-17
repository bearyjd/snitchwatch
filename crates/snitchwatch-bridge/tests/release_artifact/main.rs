//! Contract tests for the bridge release tarball tooling.
//!
//! These drive `packaging/release/bridge_artifact.py` (`pack` / `verify`)
//! against a fixture stage built in a tempdir — no container and no real
//! binary (the "binary" is a few fake bytes), so they run in plain
//! `cargo test`. They pin the consumer contract from
//! `docs/superpowers/plans/2026-10-03-bridge-release-artifact.md`:
//! byte-for-byte determinism, the exact tar layout / modes / owners / mtime,
//! a MANIFEST and SHA256SUMS that match the shipped bytes, the `.sha256`
//! sidecar format, and that `verify` catches a modified binary even when the
//! outer `.sha256` has been regenerated to match.
//!
//! The rejection tests craft hostile tarballs with `MUTATE_PY` (an inline
//! `python3 -c` script that reuses `bridge_artifact.py`'s own serializer) and
//! always regenerate a *matching* `.sha256`, so each one proves that a
//! specific contract check — not the sidecar — catches it.
//!
//! Needs `python3` (>= 3.11), GNU `tar` and `sha256sum` on PATH. A missing
//! tool FAILS the test with a message saying so — these tests never skip.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

mod pack_layout;
mod tar_rejections;
mod verify_contract;
mod version_thirdparty;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const ARCH: &str = "x86_64";
const GIT_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const SOURCE_DATE_EPOCH: u64 = 1_700_000_000;
/// `SOURCE_DATE_EPOCH` as GNU tar's `--full-time` renders it under `TZ=UTC`.
const SOURCE_DATE_UTC: &str = "2023-11-14 22:13:20";

const BIN: &str = "usr/bin/snitchwatch-bridge-cli";
const UNIT: &str = "usr/lib/systemd/user/snitchwatch-bridge.service";
const LICENSE: &str = "usr/share/licenses/snitchwatch-bridge/LICENSE.fixture";
const MANIFEST: &str = "usr/share/snitchwatch-bridge/MANIFEST.json";
const SUMS: &str = "usr/share/snitchwatch-bridge/SHA256SUMS";
const STAGE_DIRS: [&str; 8] = [
    "usr",
    "usr/bin",
    "usr/lib",
    "usr/lib/systemd",
    "usr/lib/systemd/user",
    "usr/share",
    "usr/share/licenses",
    "usr/share/licenses/snitchwatch-bridge",
];
const FAKE_BINARY: &[u8] = b"\x7fELF fixture bytes, not a real snitchwatch-bridge-cli\n";

fn workspace_file(rel: &str) -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/snitchwatch-bridge; go up two to the root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn top_dir() -> String {
    format!("snitchwatch-bridge-{VERSION}-{ARCH}")
}

fn tarball_name() -> String {
    format!("{}.tar.gz", top_dir())
}

fn sidecar(tarball: &Path) -> PathBuf {
    tarball.with_file_name(format!("{}.sha256", tarball_name()))
}

/// Run a command to completion. A tool that cannot be spawned is a hard
/// failure, never a skip.
fn run(cmd: &mut Command) -> Output {
    let program = cmd.get_program().to_string_lossy().into_owned();
    cmd.output().unwrap_or_else(|e| {
        panic!(
            "`{program}` is required by the release_artifact tests but could not be \
             run ({e}); install it — these tests deliberately never skip"
        )
    })
}

fn describe(what: &str, out: &Output) -> String {
    format!(
        "{what} exited {}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn run_ok(cmd: &mut Command) -> String {
    let what = format!("{cmd:?}");
    let out = run(cmd);
    assert!(out.status.success(), "{}", describe(&what, &out));
    String::from_utf8(out.stdout).expect("command stdout is UTF-8")
}

fn artifact_tool() -> Command {
    let mut cmd = Command::new("python3");
    cmd.arg(workspace_file("packaging/release/bridge_artifact.py"))
        .env("PYTHONDONTWRITEBYTECODE", "1");
    cmd
}

fn sha256sum(path: &Path) -> String {
    let out = run_ok(Command::new("sha256sum").arg(path));
    out.split_whitespace()
        .next()
        .expect("sha256sum printed a digest")
        .to_string()
}

fn write_file(path: &Path, bytes: &[u8], mode: u32) {
    fs::create_dir_all(path.parent().expect("file has a parent")).expect("create parent dirs");
    fs::write(path, bytes).expect("write fixture file");
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod fixture file");
}

fn set_mtime(path: &Path, secs: u64) {
    fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)))
        .expect("set fixture mtime");
}

/// `build.rpm_qa` of the fixture meta, sorted (Python/byte order).
const RPM_QA: [&str; 12] = [
    "bash-5.3.9-3.fc44.x86_64",
    "binutils-2.46.1-1.fc44.x86_64",
    "cargo-1.98.1-1.fc44.x86_64",
    "gcc-16.2.1-2.fc44.x86_64",
    "glibc-2.43-9.fc44.x86_64",
    "glibc-devel-2.43-9.fc44.x86_64",
    "libgcc-16.2.1-2.fc44.x86_64",
    "llvm-libs-22.1.8-4.fc44.x86_64",
    "protobuf-compiler-3.19.6-20.fc44.x86_64",
    "python3-3.14.7-1.fc44.x86_64",
    "rust-1.98.1-1.fc44.x86_64",
    "zlib-ng-compat-2.3.3-3.fc44.x86_64",
];
/// sha256 of `RPM_QA` joined with "\n" plus a trailing "\n" (`pack` re-checks it).
const RPM_QA_SHA256: &str = "1cef6733c2324088977cf8966dfe67fad88c9968ec4e42c04bb8c2910c92369c";

/// A complete, valid `--meta` (pack validates it exactly like verify does).
fn meta_json() -> serde_json::Value {
    serde_json::json!({
        "build": {
            "builder_image": format!("registry.example/fixture@sha256:{}", "0".repeat(64)),
            "command": "cargo build --release --locked -p snitchwatch-bridge-cli --no-default-features",
            "rustflags": "--remap-path-prefix=<src>=/snitchwatch",
            "rustc_vv": "rustc 1.98.1 (fixture)\nhost: x86_64-unknown-linux-gnu",
            "cargo_version": "cargo 1.98.1 (fixture)",
            "protoc_version": "libprotoc 3.19.6",
            "rpms": {
                "binutils": "binutils-2.46.1-1.fc44.x86_64",
                "cargo": "cargo-1.98.1-1.fc44.x86_64",
                "gcc": "gcc-16.2.1-2.fc44.x86_64",
                "glibc": "glibc-2.43-9.fc44.x86_64",
                "glibc-devel": "glibc-devel-2.43-9.fc44.x86_64",
                "libgcc": "libgcc-16.2.1-2.fc44.x86_64",
                "llvm-libs": "llvm-libs-22.1.8-4.fc44.x86_64",
                "protobuf-compiler": "protobuf-compiler-3.19.6-20.fc44.x86_64",
                "python3": "python3-3.14.7-1.fc44.x86_64",
                "rust": "rust-1.98.1-1.fc44.x86_64",
                "zlib-ng-compat": "zlib-ng-compat-2.3.3-3.fc44.x86_64"
            },
            "rpm_qa": RPM_QA,
            "rpm_qa_sha256": RPM_QA_SHA256,
            "cargo_lock_sha256": "1".repeat(64)
        },
        "upstream": {
            "opensnitch_commit": "b404c4c6316760fa7bc415509d3f8d747f7dc9cc",
            "opensnitch_tag": "v1.8.0",
            "ui_proto_sha256": "2".repeat(64)
        },
        "runtime": {
            "needed": ["ld-linux-x86-64.so.2", "libc.so.6", "libgcc_s.so.1", "libm.so.6"],
            "glibc_min": "2.34"
        }
    })
}

/// A fixture stage (the top-dir contents) plus a `--meta` file in a tempdir.
struct Fixture {
    root: tempfile::TempDir,
    stage: PathBuf,
    meta: PathBuf,
}

impl Fixture {
    /// `bin_mode` / `file_mode` / `dir_mode` are what the stage has on disk;
    /// pack must ignore them and emit the contract modes regardless.
    fn new(bin_mode: u32, file_mode: u32, dir_mode: u32) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let stage = root.path().join("stage");
        let unit = fs::read(workspace_file(
            "packaging/systemd/snitchwatch-bridge.service",
        ))
        .expect("read the real packaging/systemd unit");
        write_file(&stage.join(BIN), FAKE_BINARY, bin_mode);
        write_file(&stage.join(UNIT), &unit, file_mode);
        write_file(&stage.join(LICENSE), b"fixture license text\n", file_mode);
        for dir in STAGE_DIRS.iter().rev() {
            fs::set_permissions(stage.join(dir), fs::Permissions::from_mode(dir_mode))
                .expect("chmod fixture dir");
        }
        let meta = root.path().join("meta.json");
        fs::write(&meta, serde_json::to_vec_pretty(&meta_json()).unwrap()).expect("write meta");
        Self { root, stage, meta }
    }

    fn standard() -> Self {
        Self::new(0o755, 0o644, 0o755)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    fn pack(&self, out: &Path, extra: &[&str]) -> Output {
        run(artifact_tool()
            .arg("pack")
            .arg("--stage")
            .arg(&self.stage)
            .args(["--name", "snitchwatch-bridge", "--version", VERSION])
            .args(["--arch", ARCH, "--git-commit", GIT_COMMIT])
            .args(["--source-date-epoch", &SOURCE_DATE_EPOCH.to_string()])
            .arg("--meta")
            .arg(&self.meta)
            .arg("--out")
            .arg(out)
            .args(extra))
    }

    /// Pack into `<root>/<out_name>/` and return the tarball path.
    fn pack_ok(&self, out_name: &str, extra: &[&str]) -> PathBuf {
        let out = self.path(out_name);
        let res = self.pack(&out, extra);
        assert!(res.status.success(), "{}", describe("pack", &res));
        out.join(tarball_name())
    }
}

/// Extract with the system tar into `into`; returns the top dir.
fn extract(tarball: &Path, into: &Path) -> PathBuf {
    fs::create_dir_all(into).expect("create extract dir");
    run_ok(
        Command::new("tar")
            .arg("-xzf")
            .arg(tarball)
            .arg("-C")
            .arg(into),
    );
    into.join(top_dir())
}

fn verify(tarball: &Path) -> Output {
    run(artifact_tool()
        .args(["verify", "--tarball"])
        .arg(tarball)
        .args(["--expect-version", VERSION]))
}

fn read_manifest(top: &Path) -> (String, serde_json::Value) {
    let raw = fs::read_to_string(top.join(MANIFEST)).expect("read MANIFEST.json");
    let value = serde_json::from_str(&raw).expect("MANIFEST.json is valid JSON");
    (raw, value)
}

/// (relative path, mode, mtime, bytes) for everything under `dir`, sorted.
fn snapshot(dir: &Path) -> Vec<(PathBuf, u32, SystemTime, Vec<u8>)> {
    let mut entries = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current).expect("read_dir") {
            let path = entry.expect("dir entry").path();
            let meta = fs::symlink_metadata(&path).expect("lstat");
            let bytes = if meta.is_dir() {
                pending.push(path.clone());
                Vec::new()
            } else {
                fs::read(&path).expect("read file")
            };
            let rel = path.strip_prefix(dir).unwrap().to_path_buf();
            entries.push((
                rel,
                meta.permissions().mode(),
                meta.modified().unwrap(),
                bytes,
            ));
        }
    }
    entries.sort();
    entries
}

fn assert_keys_sorted(value: &serde_json::Value, at: &str) {
    match value {
        serde_json::Value::Object(map) => {
            let keys: Vec<&String> = map.keys().collect();
            let mut sorted = keys.clone();
            sorted.sort();
            assert_eq!(keys, sorted, "MANIFEST keys at {at} must be sorted");
            for (key, child) in map {
                assert_keys_sorted(child, &format!("{at}.{key}"));
            }
        }
        serde_json::Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                assert_keys_sorted(child, &format!("{at}[{i}]"));
            }
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ hostile tarballs

/// Rewrites a tarball (argv: tool dir, src, dst, kind, arg) and writes a
/// *matching* `<dst>.sha256`. It reuses bridge_artifact.py's own serializer, and
/// asserts that its header rewriter reproduces the canonical tar byte-for-byte,
/// so a mutation changes exactly what it says and nothing else.
const MUTATE_PY: &str = r##"
import io, json, sys, tarfile
sys.dont_write_bytecode = True
tool_dir, src, dst, kind, arg = sys.argv[1:6]
sys.path.insert(0, tool_dir)
import bridge_artifact as ba

raw = ba.gunzip_bounded(open(src, "rb").read())
with tarfile.open(fileobj=io.BytesIO(raw), mode="r:") as tar:
    members = tar.getmembers()
    data = {m.name: tar.extractfile(m).read() for m in members if m.isreg()}
top, mtime = members[0].name, members[0].mtime
contents = {name[len(top) + 1:]: blob for name, blob in data.items()}

def write_members(items, fmt=tarfile.USTAR_FORMAT):
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode="w", format=fmt) as tar:
        for info, blob in items:
            tar.addfile(info, None if blob is None else io.BytesIO(blob))
    return out.getvalue()

def items():
    return [(m, data.get(m.name)) for m in members]

def member(rel):
    return next(m for m in members if m.name == f"{top}/{rel}")

def extra(name, blob=b"#!/bin/sh\necho smuggled\n", **fields):
    info = tarfile.TarInfo(name)
    info.size, info.mode, info.mtime = len(blob), 0o755, mtime
    info.uname = info.gname = "root"
    for key, value in fields.items():
        setattr(info, key, value)
    return info, blob

def repack_contents(manifest=None):
    if manifest is not None:
        contents[ba.MANIFEST_PATH] = ba.canonical_json(manifest)
    contents[ba.SUMS_PATH] = ba.sums_text(contents)
    return ba.tar_bytes(top, contents, mtime)

assert write_members(items()) == raw, "header rewriter must reproduce the canonical tar"
evil = f"{top}/usr/bin/evil"
blob = None
if kind == "identity":
    tar_raw = raw
elif kind == "manifest":
    m = json.loads(contents[ba.MANIFEST_PATH])
    exec(arg, {"m": m})
    tar_raw = repack_contents(m)
elif kind == "tamper":
    binary = bytearray(contents[ba.BIN_PATH])
    binary[len(binary) // 2] ^= 1
    contents[ba.BIN_PATH] = bytes(binary)
    tar_raw = ba.tar_bytes(top, contents, mtime)
elif kind == "drop-license":
    gone = [rel for rel in contents if ba.is_license(rel)]
    for rel in gone:
        del contents[rel]
    m = json.loads(contents[ba.MANIFEST_PATH])
    m["files"] = [e for e in m["files"] if e["path"] not in gone]
    tar_raw = repack_contents(m)
elif kind == "header":
    rel, field, value = json.loads(arg)
    setattr(member(rel), field, value)
    tar_raw = write_members(items())
elif kind == "hardlink":
    link = extra(f"{top}/{ba.LICENSE_DIR}/LICENSE.hardlink", b"", mode=0o644,
                 type=tarfile.LNKTYPE, linkname=f"{top}/{ba.BIN_PATH}")
    tar_raw = write_members(items() + [link])
elif kind == "dotdot":
    tar_raw = write_members(items() + [extra("../evil")])
elif kind == "pax-xattr":
    member(ba.BIN_PATH).pax_headers = {"SCHILY.xattr.security.capability": "smuggled"}
    tar_raw = write_members(items(), tarfile.PAX_FORMAT)
elif kind == "append-after-eof":
    tar_raw = raw + write_members([extra(evil)])
elif kind == "after-corrupt-header":
    last = members[-1]
    end = last.offset_data + -(-last.size // 512) * 512
    tar_raw = raw[:end] + b"\xff" * 512 + write_members([extra(evil)])
elif kind == "gzip-second-member":
    blob = ba.gzip_bytes(raw) + ba.gzip_bytes(write_members([extra(evil)]))
else:
    sys.exit(f"unknown mutation {kind!r}")
blob = ba.gzip_bytes(tar_raw) if blob is None else blob
open(dst, "wb").write(blob)
name = dst.rsplit("/", 1)[-1]
open(dst + ".sha256", "w").write(f"{ba.sha256_hex(blob)}  {name}\n")
"##;

/// Apply a `MUTATE_PY` mutation to `tarball`; returns the new tarball (with a
/// matching `.sha256`) under `<fixture>/<label>-out/`.
fn mutate(fx: &Fixture, tarball: &Path, label: &str, kind: &str, arg: &str) -> PathBuf {
    let out_dir = fx.path(&format!("{label}-out"));
    fs::create_dir_all(&out_dir).expect("create mutation out dir");
    let dst = out_dir.join(tarball_name());
    run_ok(
        Command::new("python3")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .arg("-c")
            .arg(MUTATE_PY)
            .arg(workspace_file("packaging/release"))
            .arg(tarball)
            .arg(&dst)
            .args([kind, arg]),
    );
    dst
}

fn verify_with(tarball: &Path, extra: &[&str]) -> Output {
    run(artifact_tool()
        .args(["verify", "--tarball"])
        .arg(tarball)
        .args(["--expect-version", VERSION])
        .args(extra))
}

fn fail_lines(out: &Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|l| l.starts_with("verify: FAIL: "))
        .map(str::to_string)
        .collect()
}

/// `verify` must exit non-zero with a `verify: FAIL:` line containing `needle`,
/// without a Python traceback, and not blame the (matching) outer `.sha256`.
fn assert_rejected(tarball: &Path, needle: &str, case: &str) -> Vec<String> {
    let res = verify(tarball);
    let stderr = String::from_utf8_lossy(&res.stderr).into_owned();
    assert!(
        !res.status.success(),
        "{case}: verify must reject it\n{}",
        describe("verify", &res)
    );
    assert!(
        !stderr.contains("Traceback"),
        "{case}: verify crashed instead of failing cleanly\n{stderr}"
    );
    let fails = fail_lines(&res);
    assert!(
        fails.iter().any(|l| l.contains(needle)),
        "{case}: want a `verify: FAIL:` line containing {needle:?}\n{stderr}"
    );
    assert!(
        !stderr.contains(".tar.gz.sha256 is"),
        "{case}: the regenerated .sha256 matches; it must not be the failure\n{stderr}"
    );
    fails
}

fn rejects_header_change(case: &str, rel: &str, field: &str, value: u64, needle: &str) {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let arg = serde_json::json!([rel, field, value]).to_string();
    assert_rejected(&mutate(&fx, &tarball, case, "header", &arg), needle, case);
}

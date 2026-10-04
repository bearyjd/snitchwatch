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

#[test]
fn pack_is_byte_identical_and_ignores_stage_modes_and_mtimes() {
    let a = Fixture::standard();
    // A second stage at another path, with a hostile umask and other mtimes.
    let b = Fixture::new(0o700, 0o600, 0o700);
    for rel in [BIN, UNIT, LICENSE] {
        set_mtime(&b.stage.join(rel), 12_345);
    }
    let b_before = snapshot(&b.stage);

    let first = a.pack_ok("out", &[]);
    let again = a.pack_ok("out-again", &[]);
    let other = b.pack_ok("out", &[]);

    let bytes = fs::read(&first).expect("read tarball");
    assert_eq!(
        bytes,
        fs::read(&again).unwrap(),
        "packing twice must be byte-identical"
    );
    assert_eq!(
        bytes,
        fs::read(&other).unwrap(),
        "stage modes/mtimes/path must not leak into the tarball"
    );
    let first_sidecar = fs::read(sidecar(&first)).expect("read .sha256");
    assert_eq!(first_sidecar, fs::read(sidecar(&again)).unwrap());
    assert_eq!(first_sidecar, fs::read(sidecar(&other)).unwrap());
    assert_eq!(
        snapshot(&b.stage),
        b_before,
        "pack must not mutate the stage"
    );
}

#[test]
fn tarball_has_exact_members_modes_owners_and_mtime() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let top = top_dir();
    let listing = run_ok(
        Command::new("tar")
            .args(["--numeric-owner", "--full-time", "-tvzf"])
            .arg(&tarball)
            .env("TZ", "UTC")
            .env("LC_ALL", "C"),
    );

    let actual: Vec<(String, String)> = listing
        .lines()
        .map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            assert_eq!(fields.len(), 6, "unexpected `tar -tv` line: {line}");
            assert_eq!(fields[1], "0/0", "uid/gid must be 0/0: {line}");
            assert_eq!(
                format!("{} {}", fields[3], fields[4]),
                SOURCE_DATE_UTC,
                "mtime must be SOURCE_DATE_EPOCH: {line}"
            );
            (fields[0].to_string(), fields[5].to_string())
        })
        .collect();
    let expected: Vec<(String, String)> = [
        ("drwxr-xr-x", ""),
        ("drwxr-xr-x", "usr/"),
        ("drwxr-xr-x", "usr/bin/"),
        ("-rwxr-xr-x", BIN),
        ("drwxr-xr-x", "usr/lib/"),
        ("drwxr-xr-x", "usr/lib/systemd/"),
        ("drwxr-xr-x", "usr/lib/systemd/user/"),
        ("-rw-r--r--", UNIT),
        ("drwxr-xr-x", "usr/share/"),
        ("drwxr-xr-x", "usr/share/licenses/"),
        ("drwxr-xr-x", "usr/share/licenses/snitchwatch-bridge/"),
        ("-rw-r--r--", LICENSE),
        ("drwxr-xr-x", "usr/share/snitchwatch-bridge/"),
        ("-rw-r--r--", MANIFEST),
        ("-rw-r--r--", SUMS),
    ]
    .iter()
    .map(|(mode, rel)| (mode.to_string(), format!("{top}/{rel}")))
    .collect();
    assert_eq!(actual, expected, "tarball members (sorted, dirs included)");

    // The owner *names* are part of the contract too (root/root, not blank).
    let named = run_ok(
        Command::new("tar")
            .arg("-tvzf")
            .arg(&tarball)
            .env("LC_ALL", "C"),
    );
    for line in named.lines() {
        assert_eq!(
            line.split_whitespace().nth(1),
            Some("root/root"),
            "owner: {line}"
        );
    }
}

#[test]
fn manifest_is_canonical_and_its_files_match_the_extracted_bytes() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let top = extract(&tarball, &fx.path("x"));
    let (raw, manifest) = read_manifest(&top);

    assert_keys_sorted(&manifest, "$");
    assert_eq!(
        raw,
        serde_json::to_string_pretty(&manifest).unwrap() + "\n",
        "MANIFEST.json must be 2-space indented with sorted keys and a trailing newline"
    );
    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(manifest["name"], "snitchwatch-bridge");
    assert_eq!(manifest["version"], VERSION);
    assert_eq!(manifest["arch"], ARCH);
    assert_eq!(
        manifest["source"],
        serde_json::json!({
            "git_commit": GIT_COMMIT,
            "git_tag": null,
            "dirty": false,
            "source_date_epoch": SOURCE_DATE_EPOCH
        })
    );
    assert_eq!(
        manifest["install"],
        serde_json::json!({
            "binary": "/usr/bin/snitchwatch-bridge-cli",
            "unit": "/usr/lib/systemd/user/snitchwatch-bridge.service",
            "unit_name": "snitchwatch-bridge.service",
            "unit_scope": "user"
        })
    );
    for key in ["build", "upstream", "runtime"] {
        assert_eq!(
            manifest[key],
            meta_json()[key],
            "MANIFEST {key} comes from --meta"
        );
    }

    let files = manifest["files"].as_array().expect("files[] is an array");
    let paths: Vec<&str> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert_eq!(
        paths,
        [BIN, UNIT, LICENSE],
        "files[]: every file but MANIFEST/SHA256SUMS"
    );
    for entry in files {
        let rel = entry["path"].as_str().unwrap();
        let path = top.join(rel);
        assert_eq!(entry["sha256"], sha256sum(&path), "sha256 of {rel}");
        assert_eq!(
            entry["size"],
            fs::metadata(&path).unwrap().len(),
            "size of {rel}"
        );
        let mode = if rel == BIN { "0755" } else { "0644" };
        assert_eq!(entry["mode"], mode, "mode of {rel}");
    }
}

#[test]
fn sha256sums_passes_sha256sum_check_from_the_top_dir() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let top = extract(&tarball, &fx.path("x"));

    let checked = run_ok(
        Command::new("sha256sum")
            .args(["-c", SUMS])
            .current_dir(&top),
    );
    assert_eq!(
        checked.lines().count(),
        4,
        "sha256sum -c output:\n{checked}"
    );
    assert!(checked.lines().all(|l| l.ends_with(": OK")), "{checked}");

    let sums = fs::read_to_string(top.join(SUMS)).unwrap();
    let listed: Vec<&str> = sums
        .lines()
        .map(|line| line.split_once("  ").expect("`<sha256>  <path>` line").1)
        .collect();
    assert_eq!(
        listed,
        [BIN, UNIT, LICENSE, MANIFEST],
        "SHA256SUMS: every file except itself (MANIFEST included), sorted by path"
    );
}

#[test]
fn sha256_sidecar_is_in_sha256sum_format() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let text = fs::read_to_string(sidecar(&tarball)).expect("read .sha256");

    assert!(
        text.len() > 64 && text.is_char_boundary(64),
        "sidecar too short: {text:?}"
    );
    let (digest, rest) = text.split_at(64);
    assert!(
        digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "digest must be 64 lowercase hex: {text:?}"
    );
    assert_eq!(rest, format!("  {}\n", tarball_name()), "sidecar: {text:?}");
    assert_eq!(
        digest,
        sha256sum(&tarball),
        "sidecar digest must match the tarball"
    );
    let out_dir = tarball.parent().unwrap();
    run_ok(
        Command::new("sha256sum")
            .arg("-c")
            .arg(format!("{}.sha256", tarball_name()))
            .current_dir(out_dir),
    );
}

#[test]
fn verify_accepts_a_freshly_packed_tarball() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let res = verify(&tarball);
    assert!(res.status.success(), "{}", describe("verify", &res));
    assert!(String::from_utf8_lossy(&res.stdout).contains("verify: OK"));
}

/// Extract `tarball`, optionally flip one byte of the binary (same length, so
/// only hashes can notice), repack with the system tar honouring every other
/// part of the contract, and write a *matching* outer `.sha256`.
fn repack(fx: &Fixture, tarball: &Path, label: &str, tamper: bool) -> PathBuf {
    let extracted = fx.path(&format!("{label}-x"));
    let top = extract(tarball, &extracted);
    if tamper {
        let bin = top.join(BIN);
        let mut bytes = fs::read(&bin).expect("read extracted binary");
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0x01;
        fs::write(&bin, bytes).expect("write tampered binary");
    }
    let out_dir = fx.path(&format!("{label}-out"));
    fs::create_dir_all(&out_dir).expect("create repack out dir");
    let repacked = out_dir.join(tarball_name());
    run_ok(
        Command::new("tar")
            .args([
                "--format=ustar",
                "--sort=name",
                "--owner=root:0",
                "--group=root:0",
            ])
            .arg(format!("--mtime=@{SOURCE_DATE_EPOCH}"))
            .arg("--mode=a+rX,u+w,go-w")
            .arg("-czf")
            .arg(&repacked)
            .arg("-C")
            .arg(&extracted)
            .arg(top_dir()),
    );
    let digest_line = run_ok(
        Command::new("sha256sum")
            .arg(tarball_name())
            .current_dir(&out_dir),
    );
    fs::write(sidecar(&repacked), digest_line).expect("write regenerated .sha256");
    repacked
}

#[test]
fn verify_rejects_a_repacked_tarball_with_a_modified_binary() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);

    // Control: the same repack *without* tampering passes, so the failure
    // below is caused by the binary bytes, not by how we repacked. GNU gzip's
    // bytes differ from Python's, yet verify accepts it: it compares the
    // uncompressed tar stream (byte-identical with GNU tar's USTAR output),
    // never the zlib-implementation-dependent gzip bytes.
    let control = repack(&fx, &tarball, "control", false);
    assert_ne!(
        fs::read(&control).unwrap(),
        fs::read(&tarball).unwrap(),
        "the control must exercise a different gzip encoding"
    );
    let res = verify(&control);
    assert!(
        res.status.success(),
        "{}",
        describe("verify (control repack)", &res)
    );

    let tampered = repack(&fx, &tarball, "tampered", true);
    let res = verify(&tampered);
    assert!(
        !res.status.success(),
        "verify must reject a modified binary:\n{}",
        describe("verify", &res)
    );
    let stderr = String::from_utf8_lossy(&res.stderr);
    let fails: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("verify: FAIL: "))
        .collect();
    assert!(
        fails
            .iter()
            .any(|l| l.contains("MANIFEST") && l.contains(BIN) && l.contains("sha256")),
        "MANIFEST files[] must catch the modified binary:\n{stderr}"
    );
    assert!(
        fails
            .iter()
            .any(|l| l.contains("SHA256SUMS") && l.contains(BIN)),
        "SHA256SUMS must catch the modified binary:\n{stderr}"
    );
    assert!(
        !stderr.contains(".tar.gz.sha256 is"),
        "the regenerated outer .sha256 matches, so it must not be the failure:\n{stderr}"
    );
}

/// Plan decision I: the release binary is built with `--no-default-features`,
/// so it must not embed the GPL-2.0-only `web/` frontend. A binary carrying the
/// copyright line every `web/` file has is rejected by plain `verify`.
#[test]
fn verify_rejects_a_binary_that_embeds_the_web_ui() {
    let fx = Fixture::standard();
    let mut bytes = FAKE_BINARY.to_vec();
    bytes.extend_from_slice(b"/* Copyright (C) 2026 Objective Development Software GmbH */");
    write_file(&fx.stage.join(BIN), &bytes, 0o755);
    let tarball = fx.pack_ok("out-webui", &[]);
    assert_rejected(&tarball, "embeds the vendored web/ UI", "web UI embedded");
}

#[test]
fn pack_refuses_a_git_tag_that_does_not_match_the_version() {
    let fx = Fixture::standard();
    let out = fx.path("out");
    let res = fx.pack(&out, &["--git-tag", "v9.9.9"]);
    assert!(!res.status.success(), "{}", describe("pack", &res));
    assert!(
        String::from_utf8_lossy(&res.stderr).contains("--git-tag"),
        "{}",
        describe("pack", &res)
    );
    assert!(!out.exists(), "a refused pack must not write anything");

    // Control: the matching tag is accepted and recorded.
    let tag = format!("v{VERSION}");
    let tarball = fx.pack_ok("out-tagged", &["--git-tag", &tag]);
    let (_, manifest) = read_manifest(&extract(&tarball, &fx.path("x")));
    assert_eq!(manifest["source"]["git_tag"], tag);
}

#[test]
fn pack_refuses_an_unexpected_file_in_the_stage() {
    let fx = Fixture::standard();
    write_file(&fx.stage.join("usr/bin/extra-tool"), b"#!/bin/sh\n", 0o755);
    let out = fx.path("out");
    let res = fx.pack(&out, &[]);
    assert!(!res.status.success(), "{}", describe("pack", &res));
    assert!(
        String::from_utf8_lossy(&res.stderr).contains("usr/bin/extra-tool"),
        "{}",
        describe("pack", &res)
    );
    assert!(!out.exists(), "a refused pack must not write anything");
}

#[test]
fn pack_refuses_a_symlink_in_the_stage() {
    let fx = Fixture::standard();
    std::os::unix::fs::symlink(
        "/etc/passwd",
        fx.stage
            .join("usr/share/licenses/snitchwatch-bridge/LICENSE.link"),
    )
    .expect("create symlink");
    let out = fx.path("out");
    let res = fx.pack(&out, &[]);
    assert!(!res.status.success(), "{}", describe("pack", &res));
    assert!(
        String::from_utf8_lossy(&res.stderr).contains("LICENSE.link"),
        "{}",
        describe("pack", &res)
    );
    assert!(!out.exists(), "a refused pack must not write anything");
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

#[test]
fn mutate_helper_identity_still_verifies() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let same = mutate(&fx, &tarball, "identity", "identity", "");
    assert_eq!(fs::read(&same).unwrap(), fs::read(&tarball).unwrap());
    let res = verify(&same);
    assert!(res.status.success(), "{}", describe("verify", &res));
}

#[test]
fn verify_rejects_a_setuid_binary() {
    let case = "setuid";
    rejects_header_change(case, BIN, "mode", 0o4755, &format!("{BIN}: mode 4755"));
}

#[test]
fn verify_rejects_a_group_writable_directory() {
    rejects_header_change("dir0775", "usr/bin", "mode", 0o775, "usr/bin: mode 0775");
}

#[test]
fn verify_rejects_a_member_owned_by_uid_1000() {
    rejects_header_change("uid1000", UNIT, "uid", 1000, "is owned by (1000, 0,");
}

#[test]
fn verify_rejects_an_mtime_off_by_one() {
    let mtime = SOURCE_DATE_EPOCH + 1;
    rejects_header_change("mtime", BIN, "mtime", mtime, "member mtimes");
}

#[test]
fn verify_rejects_a_hardlink_member() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let bad = mutate(&fx, &tarball, "hardlink", "hardlink", "");
    assert_rejected(&bad, "is not a regular file or directory", "hardlink");
}

#[test]
fn verify_rejects_a_dotdot_member() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let bad = mutate(&fx, &tarball, "dotdot", "dotdot", "");
    assert_rejected(&bad, "unsafe member path '../evil'", "../evil");
}

#[test]
fn verify_rejects_a_tarball_without_license_files() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let bad = mutate(&fx, &tarball, "nolicense", "drop-license", "");
    assert_rejected(&bad, "no license files under", "no license");
}

#[test]
fn verify_rejects_a_bad_sha256_sidecar() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let path = sidecar(&tarball);
    let good = fs::read_to_string(&path).unwrap();
    let wrong_digest = format!("{}{}", "0".repeat(64), &good[64..]);
    let one_space = good.replacen("  ", " ", 1);
    for (case, text) in [
        ("wrong digest", Some(wrong_digest)),
        ("format", Some(one_space)),
        ("missing", None),
    ] {
        match &text {
            Some(text) => fs::write(&path, text).unwrap(),
            None => fs::remove_file(&path).unwrap(),
        }
        let res = verify(&tarball);
        assert!(
            !res.status.success(),
            "{case}: {}",
            describe("verify", &res)
        );
        let fails = fail_lines(&res);
        assert!(
            fails.iter().any(|l| l.contains(".tar.gz.sha256")),
            "{case}: the sidecar must be the failure:\n{}",
            describe("verify", &res)
        );
    }
}

#[test]
fn verify_rejects_a_member_appended_after_the_end_of_archive() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let bad = mutate(&fx, &tarball, "trailing", "append-after-eof", "");
    // The differential: GNU tar --ignore-zeros (and other lenient readers)
    // still see the smuggled member that Python's tarfile stops before.
    let listing = run(Command::new("tar")
        .args(["--ignore-zeros", "-tzf"])
        .arg(&bad));
    assert!(String::from_utf8_lossy(&listing.stdout).contains("usr/bin/evil"));
    let fails = assert_rejected(&bad, "bytes after the canonical end-of-archive", "trailing");
    assert_eq!(
        fails.len(),
        1,
        "only the canonical-stream check can see it: {fails:?}"
    );
}

#[test]
fn verify_rejects_a_member_after_a_corrupt_header() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let bad = mutate(&fx, &tarball, "corrupt", "after-corrupt-header", "");
    // GNU tar skips the bad block ("Skipping to next header") and lists the
    // member behind it; Python's tarfile silently stops at the bad block.
    let listing = run(Command::new("tar").arg("-tzf").arg(&bad));
    assert!(String::from_utf8_lossy(&listing.stdout).contains("usr/bin/evil"));
    let fails = assert_rejected(&bad, "not the canonical serialization", "corrupt header");
    assert_eq!(
        fails.len(),
        1,
        "only the canonical-stream check can see it: {fails:?}"
    );
}

#[test]
fn verify_rejects_a_pax_xattr_header() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let bad = mutate(&fx, &tarball, "pax", "pax-xattr", "");
    assert_rejected(&bad, "SCHILY.xattr.security.capability", "pax xattr");
}

#[test]
fn verify_rejects_a_second_gzip_member() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let bad = mutate(&fx, &tarball, "gzip2", "gzip-second-member", "");
    assert_rejected(
        &bad,
        "trailing data after the gzip member",
        "second gzip member",
    );
}

#[test]
fn verify_rejects_a_modified_binary_in_a_canonical_tarball() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let bad = mutate(&fx, &tarball, "tamper", "tamper", "");
    let fails = assert_rejected(&bad, "SHA256SUMS does not match", "tampered binary");
    assert!(
        fails.iter().all(|l| !l.contains("canonical")),
        "the tar stream itself is canonical here: {fails:?}"
    );
}

#[test]
fn verify_rejects_malformed_manifest_values() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let tag = format!("v{VERSION}");
    let cases: Vec<(String, &str)> = vec![
        (r#"m["build"] = {}"#.into(), "MANIFEST build keys []"),
        (r#"m["upstream"] = {}"#.into(), "MANIFEST upstream keys []"),
        (
            r#"m["runtime"]["glibc_min"] = "banana""#.into(),
            "glibc_min 'banana'",
        ),
        (
            r#"m["files"][0]["size"] = float(m["files"][0]["size"])"#.into(),
            "is not an integer",
        ),
        (
            format!(r#"m["source"].update(dirty=True, git_tag="{tag}")"#),
            "on a dirty tree",
        ),
        (
            r#"m["runtime"]["needed"] = ["libssl.so.3"]"#.into(),
            "libssl.so.3",
        ),
        (
            r#"m["build"]["rpm_qa_sha256"] = "0" * 64"#.into(),
            "rpm_qa_sha256",
        ),
        (
            r#"m["build"]["rpm_qa"].reverse()"#.into(),
            "rpm_qa is not sorted",
        ),
        (
            r#"m["build"]["rpms"]["rust"] = "rust-9-9""#.into(),
            "not in build.rpm_qa",
        ),
        (
            r#"m["build"]["cargo_lock_sha256"] = "XYZ""#.into(),
            "cargo_lock_sha256",
        ),
        (
            r#"m["build"]["builder_image"] = "fedora:44""#.into(),
            "not digest-pinned",
        ),
        (
            r#"m["upstream"]["opensnitch_commit"] = "b404""#.into(),
            "opensnitch_commit",
        ),
        (r#"m["source"]["git_commit"] += "\n""#.into(), "git_commit"),
        (
            r#"m["source"]["dirty"] = 0"#.into(),
            "dirty 0 is not a boolean",
        ),
        (
            r#"m["files"][0]["path"] = ["x"]"#.into(),
            "path ['x'] is not a string",
        ),
        (r#"m["files"][0]["extra"] = 1"#.into(), "files[] entry keys"),
        (
            r#"m["schema_version"] = True"#.into(),
            "schema_version is True",
        ),
    ];
    for (i, (statement, needle)) in cases.iter().enumerate() {
        let bad = mutate(&fx, &tarball, &format!("m{i}"), "manifest", statement);
        assert_rejected(&bad, needle, statement);
    }
}

#[test]
fn verify_never_tracebacks_on_wrongly_typed_manifest_sections() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let sections = ["source", "build", "upstream", "runtime", "install", "files"];
    let mut i = 0;
    for section in sections {
        for value in ["[]", "'x'", "1", "None", "[1]", "{'a': []}"] {
            let statement = format!("m[{section:?}] = {value}");
            let bad = mutate(&fx, &tarball, &format!("t{i}"), "manifest", &statement);
            assert_rejected(&bad, "MANIFEST", &statement);
            i += 1;
        }
    }
    let statement = "m['build']['rpms'] = []; m['runtime']['needed'] = 'libc.so.6'";
    let bad = mutate(&fx, &tarball, "nested", "manifest", statement);
    assert_rejected(&bad, "build.rpms", statement);
}

#[test]
fn pack_refuses_git_tag_together_with_dirty() {
    let fx = Fixture::standard();
    let out = fx.path("out");
    let tag = format!("v{VERSION}");
    let res = fx.pack(&out, &["--git-tag", &tag, "--dirty"]);
    assert!(!res.status.success(), "{}", describe("pack", &res));
    assert!(
        String::from_utf8_lossy(&res.stderr).contains("--git-tag and --dirty"),
        "{}",
        describe("pack", &res)
    );
    assert!(!out.exists(), "a refused pack must not write anything");

    // Control: --dirty alone is accepted and recorded.
    let tarball = fx.pack_ok("out-dirty", &["--dirty"]);
    let (_, manifest) = read_manifest(&extract(&tarball, &fx.path("x")));
    assert_eq!(manifest["source"]["dirty"], true);
}

#[test]
fn verify_refuses_to_run_or_ldd_the_binary_without_expect_sha256() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    for flag in ["--run-flags", "--check-ldd"] {
        let res = verify_with(&tarball, &[flag]);
        assert!(
            !res.status.success(),
            "{flag}: {}",
            describe("verify", &res)
        );
        assert!(
            fail_lines(&res)
                .iter()
                .any(|l| l.contains("pass --expect-sha256")),
            "{flag}: {}",
            describe("verify", &res)
        );
    }
}

#[test]
fn verify_checks_expect_sha256_before_anything_else() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let digest = sha256sum(&tarball);
    let res = verify_with(&tarball, &["--expect-sha256", &digest]);
    assert!(
        res.status.success(),
        "{}",
        describe("verify (control)", &res)
    );

    // With the sidecar gone too, the mismatch must be the ONLY failure: the
    // untrusted bytes are not even parsed.
    fs::remove_file(sidecar(&tarball)).unwrap();
    let wrong = format!(
        "{}{}",
        &digest[..63],
        if digest.ends_with('0') { "1" } else { "0" }
    );
    let res = verify_with(&tarball, &["--expect-sha256", &wrong]);
    assert!(!res.status.success(), "{}", describe("verify", &res));
    let fails = fail_lines(&res);
    assert_eq!(fails.len(), 1, "{fails:?}");
    assert!(fails[0].contains("--expect-sha256"), "{fails:?}");

    let res = verify_with(&tarball, &["--expect-sha256", &digest.to_uppercase()]);
    assert!(!res.status.success(), "{}", describe("verify", &res));
    assert!(fail_lines(&res)[0].contains("64 lowercase hex"));
}

#[test]
fn verify_extract_to_writes_exactly_the_verified_tree_with_contract_modes() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let digest = sha256sum(&tarball);
    let dest = fx.path("installed");
    let dest_arg = dest.to_str().unwrap();
    let res = verify_with(
        &tarball,
        &["--expect-sha256", &digest, "--extract-to", dest_arg],
    );
    assert!(res.status.success(), "{}", describe("verify", &res));
    let entries: Vec<String> = fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        entries,
        [top_dir()],
        "only the top dir, no staging leftovers"
    );

    let reference = snapshot(&extract(&tarball, &fx.path("reference")));
    let got = snapshot(&dest.join(top_dir()));
    let names = |s: &[(PathBuf, u32, SystemTime, Vec<u8>)]| -> Vec<PathBuf> {
        s.iter().map(|e| e.0.clone()).collect()
    };
    assert_eq!(names(&got), names(&reference), "same paths as the tarball");
    let epoch = SystemTime::UNIX_EPOCH + Duration::from_secs(SOURCE_DATE_EPOCH);
    for ((rel, mode, mtime, bytes), (_, _, _, want_bytes)) in got.iter().zip(&reference) {
        let is_file = top_dir_relative_is_file(rel);
        let want_mode = if !is_file || rel.ends_with(BIN) {
            0o755
        } else {
            0o644
        };
        assert_eq!(mode & 0o7777, want_mode, "mode of {}", rel.display());
        assert_eq!(*mtime, epoch, "mtime of {}", rel.display());
        assert_eq!(bytes, want_bytes, "bytes of {}", rel.display());
    }
    assert_eq!(
        fs::metadata(dest.join(top_dir()))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o755
    );

    // Never merges into an existing tree.
    let res = verify_with(
        &tarball,
        &["--expect-sha256", &digest, "--extract-to", dest_arg],
    );
    assert!(
        !res.status.success(),
        "{}",
        describe("verify (again)", &res)
    );
    assert!(String::from_utf8_lossy(&res.stderr).contains("already exists"));

    // A tarball that fails verification writes nothing.
    let bad = mutate(&fx, &tarball, "tamper", "tamper", "");
    let bad_digest = sha256sum(&bad);
    let nowhere = fx.path("never");
    let res = verify_with(
        &bad,
        &[
            "--expect-sha256",
            &bad_digest,
            "--extract-to",
            nowhere.to_str().unwrap(),
        ],
    );
    assert!(
        !res.status.success(),
        "{}",
        describe("verify (tampered)", &res)
    );
    assert!(
        !nowhere.exists(),
        "a failed verify must not extract anything"
    );
}

/// `snapshot` paths are relative to the top dir; files are the contract files.
fn top_dir_relative_is_file(rel: &Path) -> bool {
    let rel = rel.to_string_lossy();
    rel == BIN || rel == UNIT || rel == LICENSE || rel == MANIFEST || rel == SUMS
}

// ------------------------------------------------------------------ version / thirdparty

#[test]
fn version_prints_the_bridge_cli_version_resolving_the_workspace_version() {
    let out = run_ok(
        artifact_tool()
            .args(["version", "--repo"])
            .arg(workspace_file("")),
    );
    assert_eq!(out, format!("{VERSION}\n"), "the real workspace");

    let repo = tempfile::tempdir().expect("tempdir");
    let crate_toml = repo.path().join("crates/snitchwatch-bridge-cli/Cargo.toml");
    write_file(
        &crate_toml,
        b"[package]\nname = \"snitchwatch-bridge-cli\"\nversion.workspace = true\n",
        0o644,
    );
    let workspace_toml = repo.path().join("Cargo.toml");
    for (version, ok) in [("1.2.3", true), ("1.2", false), ("1.2.3\\n", false)] {
        let body = format!("[workspace.package]\nversion = \"{version}\"\n");
        write_file(&workspace_toml, body.as_bytes(), 0o644);
        let res = run(artifact_tool().args(["version", "--repo"]).arg(repo.path()));
        assert_eq!(
            res.status.success(),
            ok,
            "{version}: {}",
            describe("version", &res)
        );
        if ok {
            assert_eq!(String::from_utf8_lossy(&res.stdout), format!("{version}\n"));
        } else {
            assert!(String::from_utf8_lossy(&res.stderr).contains("version"));
        }
    }
}

fn write_text(path: &Path, text: &str) {
    write_file(path, text.as_bytes(), 0o644);
}

fn path_crate(dir: &Path, name: &str, version: &str, extra: &str) {
    write_text(
        &dir.join("Cargo.toml"),
        &format!(
            "[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2021\"\n{extra}"
        ),
    );
    write_text(&dir.join("src/lib.rs"), "");
}

/// A workspace (app = `snitchwatch-bridge-cli` + a second member) whose
/// third-party deps are path crates OUTSIDE the workspace root (inside it they
/// would become members). Needs no registry and no network.
fn thirdparty_fixture(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    let third = root.join("third");
    write_text(
        &ws.join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"proto\", \"shell\"]\nresolver = \"2\"\n",
    );
    path_crate(
        &ws.join("app"),
        "snitchwatch-bridge-cli",
        "0.1.0",
        "[dependencies]\nsnitchwatch-proto = { path = \"../proto\" }\n\
         alpha = { path = \"../../third/alpha\" }\n\
         epsilon = { path = \"../../third/epsilon\", optional = true }\n\
         [features]\ndefault = [\"web-ui\"]\nweb-ui = [\"dep:epsilon\"]\n\
         [dev-dependencies]\ndevonly = { path = \"../../third/devonly\" }\n\
         [build-dependencies]\nbuildonly = { path = \"../../third/buildonly\" }\n",
    );
    // Another member turns on alpha's `extra` feature. Feature unification puts
    // delta into `cargo metadata`'s resolve, but the release build never has it.
    path_crate(
        &ws.join("shell"),
        "shell",
        "0.1.0",
        "[dependencies]\nalpha = { path = \"../../third/alpha\", features = [\"extra\"] }\n",
    );
    write_text(&ws.join("app/build.rs"), "fn main() {}\n");
    path_crate(
        &ws.join("proto"),
        "snitchwatch-proto",
        "0.1.0",
        "[dependencies]\nbeta = { path = \"../../third/beta\" }\n",
    );
    path_crate(
        &third.join("alpha"),
        "alpha",
        "1.0.0",
        "license = \"MIT OR Apache-2.0\"\nrepository = \"https://example.invalid/alpha\"\n\
         [dependencies]\ngamma = { path = \"../gamma\" }\n\
         delta = { path = \"../delta\", optional = true }\n\
         [features]\nextra = [\"dep:delta\"]\n",
    );
    write_text(
        &third.join("alpha/LICENSE-MIT"),
        "ALPHA-MIT-TEXT\n```\nfenced\n```\n",
    );
    write_text(&third.join("alpha/NOTICE"), "ALPHA-NOTICE-TEXT");
    write_text(
        &third.join("alpha/license-extra.txt"),
        "ALPHA-LOWERCASE-LICENSE\n",
    );
    write_text(
        &third.join("alpha/README.md"),
        "ALPHA-README-NOT-A-LICENSE\n",
    );
    path_crate(
        &third.join("beta"),
        "beta",
        "2.0.0",
        "license-file = \"COPYING\"\n",
    );
    write_text(&third.join("beta/COPYING"), "BETA-COPYING-TEXT\n");
    path_crate(
        &third.join("gamma"),
        "gamma",
        "0.3.0",
        "license = \"Unicode-3.0\"\n",
    );
    for name in ["devonly", "buildonly", "delta", "epsilon"] {
        path_crate(&third.join(name), name, "1.0.0", "license = \"MIT\"\n");
        write_text(&third.join(name).join("LICENSE"), &format!("{name}-TEXT\n"));
    }
    run_ok(
        Command::new(env!("CARGO"))
            .args(["generate-lockfile", "--offline", "--manifest-path"])
            .arg(ws.join("Cargo.toml")),
    );
    ws
}

fn thirdparty(ws: &Path, out: &Path) -> String {
    let cargo_dir = Path::new(env!("CARGO"))
        .parent()
        .expect("cargo has a parent dir");
    let path = format!(
        "{}:{}",
        cargo_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    run_ok(
        artifact_tool()
            .env("PATH", path)
            .args(["thirdparty", "--repo"])
            .arg(ws)
            .arg("--out")
            .arg(out),
    );
    fs::read_to_string(out).expect("read THIRD-PARTY-LICENSES.md")
}

#[test]
fn thirdparty_lists_the_normal_dependency_closure_deterministically() {
    let a = tempfile::tempdir().expect("tempdir");
    let b = tempfile::tempdir().expect("tempdir");
    let text = thirdparty(&thirdparty_fixture(a.path()), &a.path().join("out.md"));
    let again = thirdparty(&thirdparty_fixture(b.path()), &b.path().join("out.md"));
    assert_eq!(text, again, "output must not depend on the checkout path");

    let headings: Vec<&str> = text.lines().filter(|l| l.starts_with("## ")).collect();
    assert_eq!(
        headings,
        ["## alpha 1.0.0", "## beta 2.0.0", "## gamma 0.3.0"],
        "normal closure only (through workspace members, which are not listed)\n{text}"
    );
    // delta: only another member's feature enables it; epsilon: only the
    // default feature that the release build's --no-default-features drops.
    for absent in [
        "devonly",
        "buildonly",
        "delta",
        "epsilon",
        "README-NOT-A-LICENSE",
    ] {
        assert!(
            !text.contains(absent),
            "{absent} must not be listed\n{text}"
        );
    }
    for present in [
        "- License (SPDX): MIT OR Apache-2.0",
        "- Repository: <https://example.invalid/alpha>",
        "ALPHA-MIT-TEXT\n```\nfenced\n```\n````",
        "ALPHA-NOTICE-TEXT",
        "ALPHA-LOWERCASE-LICENSE",
        "- License (SPDX): not declared in Cargo.toml",
        "- License file (declared): `COPYING`",
        "BETA-COPYING-TEXT",
        "- License (SPDX): Unicode-3.0",
        "ships no LICENSE*",
    ] {
        assert!(text.contains(present), "missing {present:?}\n{text}");
    }
    for root in [a.path(), b.path()] {
        let root = root.to_string_lossy();
        assert!(
            !text.contains(&*root),
            "absolute path {root} leaked\n{text}"
        );
    }
}

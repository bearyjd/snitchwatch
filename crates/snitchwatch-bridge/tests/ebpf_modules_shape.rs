//! Shape + behaviour tests for `packaging/ebpf/build-modules.sh` and
//! `packaging/ebpf/pins.env` (PR-E1: build the opensnitch eBPF modules from
//! the pinned vendor commit). No clang, kernel headers, sudo, podman or
//! daemon is needed: only `--help`, `--dry-run` and the argument/pin failure
//! paths are executed. The real build is exercised on the tower.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SCRIPT: &str = "packaging/ebpf/build-modules.sh";
const PINS: &str = "packaging/ebpf/pins.env";
const RELEASE_PINS: &str = "packaging/release/pins.env";

fn workspace_file(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn read(rel: &str) -> String {
    let path = workspace_file(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e))
}

fn parse_env(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.trim_matches('"').to_string()))
        .collect()
}

fn run(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new("bash");
    cmd.arg(workspace_file(SCRIPT)).args(args);
    cmd.env_remove("KERNEL_DIR").env_remove("KERNEL_HEADERS");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn bash")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sw-ebpf-shape-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn opensnitch_commit_pin_equals_release_pin() {
    let ebpf = parse_env(&read(PINS));
    let release = parse_env(&read(RELEASE_PINS));
    let pin = ebpf
        .get("SW_OPENSNITCH_COMMIT")
        .expect("SW_OPENSNITCH_COMMIT");
    assert_eq!(pin.len(), 40);
    assert!(pin
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    assert_eq!(Some(pin), release.get("SW_OPENSNITCH_COMMIT"));
}

#[test]
fn toolchain_pins_present() {
    let ebpf = parse_env(&read(PINS));
    for k in [
        "SW_EBPF_CLANG_MAJOR",
        "SW_EBPF_LLVM_MAJOR",
        "SW_EBPF_LLC_MAJOR",
        "SW_EBPF_KERNEL_DEVEL_VERSION",
    ] {
        assert!(
            ebpf.get(k).is_some_and(|v| !v.is_empty()),
            "{k} missing in {PINS}"
        );
    }
}

#[test]
fn script_shape() {
    let s = read(SCRIPT);
    assert!(s.starts_with("#!/usr/bin/env bash"));
    assert!(s.contains("set -euo pipefail"));
    for needle in [
        "--help",
        "--dry-run",
        "KERNEL_DIR",
        "KERNEL_HEADERS",
        "llvm-strip -g",
        "SHA256SUMS",
        "usr/lib/opensnitchd/ebpf",
        "opensnitch.o",
        "opensnitch-procs.o",
        "opensnitch-dns.o",
        "mktemp -d",
        "mkdir -p --",
        "install -m 0644 --",
        "SW_OPENSNITCH_COMMIT",
        "git --no-replace-objects -C \"$OS_DIR\" archive \"$SW_OPENSNITCH_COMMIT\" ebpf_prog",
        "git --no-replace-objects -C \"$OS_DIR\" archive",
        "git --no-replace-objects -C \"$OS_DIR\" rev-parse",
        "realpath -e --",
        "CC=\"$CLANG_BIN\"",
        "LLC=\"$LLC_BIN\"",
        "-i PATH=\"$SAFE_PATH\" HOME=/nonexistent LC_ALL=C",
        "GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null",
        "clean tar -x --no-same-owner --no-same-permissions",
        "clean git --no-replace-objects -C \"$OS_DIR\" archive",
        "clean sha256sum",
        "clean install",
        "clean mktemp",
        "clean realpath",
        "export PATH=/usr/bin:/bin",
        "ENV_BIN=/usr/bin/env",
        "\"$ENV_BIN\" -i PATH=",
        "only x86_64 is supported",
        "clean \"$STRIP_BIN\" -g",
        "-R -r -C",
        "KERNEL_ARCH=\"$KARCH\"",
        "invalid KERNEL_ARCH",
        "\"$LLC_BIN\" --version",
        "release pins not found",
    ] {
        assert!(s.contains(needle), "script lacks {needle:?}");
    }
    for banned in [
        "sudo",
        "podman",
        "cp -R",
        "env -u",
        "make -C \"$WORK\" clean",
        "\n. \"$PINS_FILE\"",
        "make -C vendor",
        "git -C vendor/opensnitch checkout",
    ] {
        assert!(!s.contains(banned), "script must not contain {banned:?}");
    }
}

#[test]
fn help_exits_zero() {
    let o = run(&["--help"], &[]);
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&o);
    assert!(t.contains("--dry-run") && t.contains("--staging"), "{t}");
}

#[test]
fn dry_run_needs_no_toolchain_and_writes_nothing() {
    let stage = scratch("dry");
    let stage_s = stage.to_str().unwrap();
    // Empty PATH dir: no clang/llvm-strip/make visible.
    let o = run(
        &[
            "--dry-run",
            "--staging",
            stage_s,
            "--kernel-dir",
            "/nonexistent",
            "--kernel-headers",
            "/nonexistent",
        ],
        &[],
    );
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("dry-run"), "{t}");
    assert!(t.contains("usr/lib/opensnitchd/ebpf"), "{t}");
    assert_eq!(
        std::fs::read_dir(&stage).unwrap().count(),
        0,
        "dry-run wrote into staging"
    );
}

#[test]
fn commit_pin_mismatch_fails_clearly() {
    let d = scratch("pins");
    let mut pins = read(PINS);
    pins = pins
        .lines()
        .map(|l| {
            if l.starts_with("SW_OPENSNITCH_COMMIT=") {
                "SW_OPENSNITCH_COMMIT=0000000000000000000000000000000000000000".to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let f = d.join("pins.env");
    std::fs::write(&f, pins).unwrap();
    let o = run(
        &["--dry-run", "--staging", d.to_str().unwrap()],
        &[("SW_EBPF_PINS_FILE", f.to_str().unwrap())],
    );
    assert!(!o.status.success());
    assert!(text(&o).contains("commit pin mismatch"), "{}", text(&o));
}

#[test]
fn real_run_without_kernel_dirs_fails_clearly() {
    let d = scratch("nokdir");
    let o = run(&["--staging", d.to_str().unwrap()], &[]);
    assert!(!o.status.success());
    assert!(text(&o).contains("KERNEL_DIR"), "{}", text(&o));
}

#[test]
fn unknown_flag_fails() {
    let o = run(&["--bogus"], &[]);
    assert!(!o.status.success());
    assert!(text(&o).contains("unknown"), "{}", text(&o));
}

fn real_run_args<'a>(stage: &'a str, kdir: &'a str, kver: &'a str) -> Vec<&'a str> {
    vec![
        "--staging",
        stage,
        "--kernel-dir",
        kdir,
        "--kernel-headers",
        kdir,
        "--kernel-ver",
        kver,
    ]
}

#[test]
fn make_expression_in_kernel_ver_is_rejected() {
    let d = scratch("kver");
    let (st, k) = (d.to_str().unwrap(), d.to_str().unwrap());
    for bad in ["$(shell touch pwned)", "6.1;x", "6 1", "`id`"] {
        let o = run(&real_run_args(st, k, bad), &[]);
        assert!(!o.status.success(), "{bad}");
        assert!(
            text(&o).contains("invalid KERNEL_VER"),
            "{bad}: {}",
            text(&o)
        );
    }
    assert!(!d.join("pwned").exists());
}

#[test]
fn make_expression_in_kernel_paths_is_rejected() {
    let d = scratch("kpath");
    let evil = d.join("a$(shell touch pwned)");
    std::fs::create_dir_all(&evil).unwrap();
    let o = run(
        &real_run_args(d.to_str().unwrap(), evil.to_str().unwrap(), "6.1"),
        &[],
    );
    assert!(!o.status.success());
    assert!(text(&o).contains("invalid KERNEL_DIR"), "{}", text(&o));
    // Same through the environment variable.
    let o = run(
        &["--staging", d.to_str().unwrap()],
        &[("KERNEL_DIR", evil.to_str().unwrap())],
    );
    assert!(!o.status.success());
    assert!(text(&o).contains("invalid KERNEL_DIR"), "{}", text(&o));
}

#[test]
fn relative_kernel_dir_is_canonicalised_not_rejected_as_relative() {
    // A relative path that exists is made absolute first; a missing one fails
    // with a clear message instead of reaching make.
    let d = scratch("krel");
    let o = run(
        &real_run_args(d.to_str().unwrap(), "no/such/dir", "6.1"),
        &[],
    );
    assert!(!o.status.success());
    assert!(
        text(&o).contains("KERNEL_DIR does not exist"),
        "{}",
        text(&o)
    );
}

#[test]
fn make_expression_in_kernel_arch_is_rejected() {
    let d = scratch("karch");
    let k = d.to_str().unwrap();
    for bad in ["$(shell touch pwned)", "x86;y", "a b"] {
        let o = run(
            &["--staging", k, "--kernel-dir", k],
            &[("KERNEL_ARCH", bad)],
        );
        assert!(!o.status.success(), "{bad}");
        assert!(
            text(&o).contains("invalid KERNEL_ARCH"),
            "{bad}: {}",
            text(&o)
        );
    }
    assert!(!d.join("pwned").exists());
}

#[test]
fn non_x86_64_kernel_arch_is_rejected() {
    let d = scratch("aarch64");
    let k = d.to_str().unwrap();
    let o = run(
        &["--staging", k, "--kernel-dir", k],
        &[("KERNEL_ARCH", "aarch64")],
    );
    assert!(!o.status.success());
    assert!(
        text(&o).contains("only x86_64 is supported"),
        "{}",
        text(&o)
    );
}

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
        "git -C \"$OS_DIR\" archive \"$SW_OPENSNITCH_COMMIT\" ebpf_prog",
        "make -C \"$WORK\" clean",
        "llc --version",
        "release pins not found",
    ] {
        assert!(s.contains(needle), "script lacks {needle:?}");
    }
    for banned in [
        "sudo",
        "podman",
        "cp -R",
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

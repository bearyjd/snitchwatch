//! Guards the "no persistent privileged process" invariant (baseline spec, privileged tier):
//! the scanner runs on demand via polkit/D-Bus activation only, never as a timer, socket unit,
//! long-running systemd service, or self-opened listening socket.

use std::fs;
use std::path::{Path, PathBuf};

const PACKAGING_DIRS: [&str; 2] = ["packaging/scanner", "packaging/polkit"];
const BLUEBUILD_DIR: &str = "packaging/bluebuild";
const SOURCE_DIRS: [&str; 2] = ["crates/scanner-privileged/src", "crates/scanner-broker/src"];

/// Socket-creating tokens forbidden in non-test scanner/broker code. The broker's session-bus
/// name ownership goes through `zbus::connection::Builder::session()...serve_at(..)`, which none
/// of these match, so it is the allowed case.
const FORBIDDEN_TOKENS: [&str; 8] = [
    "TcpListener",
    "UnixListener",
    "UdpSocket",
    "UnixDatagram",
    ".bind(",
    "ListenStream",
    "ListenDatagram",
    "ListenSequentialPacket",
];

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            files_under(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn ext(p: &Path) -> &str {
    p.extension().and_then(|e| e.to_str()).unwrap_or("")
}

fn has_type_dbus(text: &str) -> bool {
    text.lines().any(|l| l.trim() == "Type=dbus")
}

fn first_section(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|l| l.starts_with('[') && l.ends_with(']'))
}

fn audit_packaging_file(path: &Path, all: &[PathBuf], out: &mut Vec<String>) {
    let name = path.display();
    match ext(path) {
        "timer" | "socket" => out.push(format!("{name}: timer/socket unit is persistent")),
        "service" => {
            let text = fs::read_to_string(path).unwrap_or_default();
            match first_section(&text) {
                Some("[Service]") | Some("[Unit]") if !has_type_dbus(&text) => {
                    out.push(format!("{name}: systemd service without Type=dbus"));
                }
                Some("[D-BUS Service]") => {
                    for unit in text
                        .lines()
                        .filter_map(|l| l.trim().strip_prefix("SystemdService="))
                    {
                        let ok = all.iter().any(|p| {
                            p.file_name().and_then(|n| n.to_str()) == Some(unit.trim())
                                && has_type_dbus(&fs::read_to_string(p).unwrap_or_default())
                        });
                        if !ok {
                            out.push(format!(
                                "{name}: SystemdService={unit} missing or not Type=dbus"
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

fn audit_source_file(path: &Path, out: &mut Vec<String>) {
    let text = fs::read_to_string(path).unwrap_or_default();
    for (i, line) in text.lines().enumerate() {
        if line.starts_with("mod tests") {
            break; // non-test code only; unit-test modules sit at the end of each file
        }
        if let Some(tok) = FORBIDDEN_TOKENS.iter().find(|t| line.contains(**t)) {
            out.push(format!(
                "{}:{}: socket listener `{tok}`",
                path.display(),
                i + 1
            ));
        }
    }
}

fn audit(root: &Path) -> Vec<String> {
    let mut packaging = Vec::new();
    for d in PACKAGING_DIRS {
        files_under(&root.join(d), &mut packaging);
    }
    let mut bluebuild = Vec::new();
    files_under(&root.join(BLUEBUILD_DIR), &mut bluebuild);
    packaging.extend(bluebuild.into_iter().filter(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains("scanner"))
    }));

    let mut out = Vec::new();
    for p in &packaging {
        audit_packaging_file(p, &packaging, &mut out);
    }
    let mut sources = Vec::new();
    for d in SOURCE_DIRS {
        files_under(&root.join(d), &mut sources);
    }
    for p in sources.iter().filter(|p| ext(p) == "rs") {
        audit_source_file(p, &mut out);
    }
    out
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn copy_tree(src: &Path, dst: &Path) {
    let mut files = Vec::new();
    files_under(src, &mut files);
    for f in files {
        let target = dst.join(f.strip_prefix(src).unwrap());
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(&f, &target).unwrap();
    }
}

/// Copies every audited tree into a tempdir so mutations never touch the real repo.
fn audited_copy() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = repo_root();
    for d in PACKAGING_DIRS
        .iter()
        .chain([&BLUEBUILD_DIR])
        .chain(SOURCE_DIRS.iter())
    {
        copy_tree(&root.join(d), &tmp.path().join(d));
    }
    tmp
}

fn assert_one_violation(tmp: &tempfile::TempDir) {
    let v = audit(tmp.path());
    assert_eq!(v.len(), 1, "expected exactly one violation, got {v:?}");
}

#[test]
fn repo_tree_has_no_persistent_service() {
    let root = repo_root();
    // A renamed or moved directory would make `files_under` return nothing and the audit
    // pass vacuously, so pin that the known inputs are really there.
    assert!(
        root.join("packaging/scanner/org.snitchwatch.Scanner.service")
            .is_file(),
        "audited D-Bus activation file moved: update this guard"
    );
    let mut sources = Vec::new();
    files_under(&root.join("crates/scanner-privileged/src"), &mut sources);
    assert!(
        sources.iter().any(|p| ext(p) == "rs"),
        "no scanner-privileged sources found to audit"
    );
    let v = audit(&root);
    assert!(v.is_empty(), "persistent-service violations: {v:#?}");
}

#[test]
fn copied_tree_is_clean_baseline() {
    assert!(audit(audited_copy().path()).is_empty());
}

#[test]
fn timer_is_a_violation() {
    let tmp = audited_copy();
    fs::write(tmp.path().join("packaging/scanner/scan.timer"), "[Timer]\n").unwrap();
    assert_one_violation(&tmp);
}

#[test]
fn simple_service_is_a_violation() {
    let tmp = audited_copy();
    let p = tmp.path().join("packaging/scanner/x.service");
    fs::write(
        p,
        "[Unit]\nDescription=x\n[Service]\nType=simple\nExecStart=/bin/true\n",
    )
    .unwrap();
    assert_one_violation(&tmp);
}

#[test]
fn dangling_systemd_service_is_a_violation() {
    let tmp = audited_copy();
    let p = tmp.path().join("packaging/scanner/org.example.service");
    fs::write(
        p,
        "[D-BUS Service]\nName=org.example\nExec=/bin/true\nSystemdService=gone.service\n",
    )
    .unwrap();
    assert_one_violation(&tmp);
}

#[test]
fn tcp_listener_in_source_is_a_violation() {
    let tmp = audited_copy();
    let p = tmp.path().join("crates/scanner-broker/src/evil.rs");
    fs::write(
        p,
        "fn f() { let _ = std::net::TcpListener::bind(\"0.0.0.0:1\"); }\n",
    )
    .unwrap();
    assert_one_violation(&tmp);
}

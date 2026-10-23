//! The closed dual-use list, one test per class of entry. The lists below
//! are written out from the plan (`2026-10-22-gui-learning-mode.md`, "DUAL-USE
//! list"), not read from the module under test.

use super::*;

/// A name for a glob entry `stem*`: the stem itself, and with a suffix.
fn variants(entry: &str) -> Vec<String> {
    match entry.strip_suffix('*') {
        Some(stem) => vec![
            stem.to_string(),
            format!("{stem}3"),
            format!("{stem}-x86_64"),
            format!("{stem}3.12"),
        ],
        None => vec![entry.to_string()],
    }
}

fn check_class(entries: &[&str]) {
    for entry in entries {
        for name in variants(entry) {
            // By NAME: the program's own name is the entry; its path is not.
            let by_name = prog(&name, "/opt/vendor/bin/launcher", &[("a.example", 2)]);
            assert!(is_dual_use(&by_name), "{name} by name");
            assert!(one(by_name).dual_use, "{name} by name, proposal");
            // By PATH: the name is innocent; the file is the entry.
            for dir in ["/usr/bin", "/opt/vendor/bin", "/home/u/bin"] {
                let by_path = prog("worker", &format!("{dir}/{name}"), &[("a.example", 2)]);
                assert!(is_dual_use(&by_path), "{dir}/{name} by path");
                assert!(one(by_path).dual_use, "{dir}/{name} by path, proposal");
            }
            // Case does not hide it.
            let shouting = prog(&name.to_uppercase(), "/opt/vendor/bin/launcher", &[]);
            assert!(is_dual_use(&shouting), "{name} upper-cased");
        }
    }
}

#[test]
fn interpreters() {
    check_class(&[
        "python*", "perl", "ruby", "node*", "bun", "deno", "java", "lua*", "php*", "dotnet",
        "tclsh", "awk", "gawk",
    ]);
}

#[test]
fn toolchains() {
    check_class(&["go", "cargo", "rustc"]);
}

#[test]
fn shells_and_runners() {
    check_class(&[
        "bash", "sh", "zsh", "fish", "dash", "busybox", "env", "xargs",
    ]);
}

#[test]
fn privilege_tools() {
    check_class(&["sudo", "pkexec"]);
}

#[test]
fn editors() {
    check_class(&["vim", "vi", "emacs"]);
}

#[test]
fn network_and_transfer_tools() {
    check_class(&[
        "curl", "wget", "aria2c", "ssh", "scp", "sftp", "rsync", "telnet", "ftp", "lftp", "nc",
        "ncat", "netcat", "socat", "nmap", "openssl",
    ]);
}

#[test]
fn debuggers_and_package_tools() {
    check_class(&["gdb", "pip*", "pipx", "uv", "git"]);
}

#[test]
fn containers_emulators_and_system_package_tools() {
    check_class(&[
        "podman",
        "docker",
        "qemu*",
        "wine*",
        "flatpak",
        "rpm-ostree",
    ]);
}

#[test]
fn a_name_that_only_starts_like_an_exact_entry_is_not_on_the_list() {
    for name in [
        "gopls",
        "godot",
        "shotwell",
        "ncdu",
        "virt-manager",
        "gitg",
        "sshd",
        "ssh-agent",
        "rubyx",
        "perlbrew",
        "cargo-watch",
        "firefox",
        "NetworkManager",
        "pytho",
        "p",
        "",
        "uvicorn",
    ] {
        let by_name = prog(name, "/opt/vendor/bin/launcher", &[]);
        assert!(!is_dual_use(&by_name), "{name:?}");
    }
    assert!(!is_dual_use(&prog("worker", "/opt/vendor/bin/gopls", &[])));
    assert!(!is_dual_use(&nameless("app", &[])));
}

#[test]
fn a_directory_called_like_an_entry_does_not_make_its_files_dual_use() {
    // Only the file name counts, not a folder it sits in.
    assert!(!is_dual_use(&prog("app", "/opt/python/bin/app", &[])));
    assert!(!is_dual_use(&prog("app", "/home/u/git/app", &[])));
}

#[test]
fn a_nameless_program_is_matched_by_its_name() {
    assert!(is_dual_use(&nameless("python3", &[])));
    assert!(is_dual_use(&nameless("bash", &[])));
    assert!(!is_dual_use(&nameless("kworker", &[])));
}

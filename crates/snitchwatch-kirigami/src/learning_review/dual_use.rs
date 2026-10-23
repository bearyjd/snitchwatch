//! The closed list of dual-use programs: interpreters, shells, toolchains,
//! package tools, remote-access and general network tools. A program on it
//! can run other programs or reach anywhere, so it is NEVER suggested
//! ("Select suggested"). It is a second filter on top of the others, NOT a
//! list of programs known to be fine, and it is not claimed to be
//! exhaustive; adding to it is a reviewed change.
//!
//! An entry matches a program's NAME and the last component of its PATH,
//! ignoring case. A trailing `*` means "starts with"; any other entry must
//! be the whole name.

use super::paths::basename;
use crate::learned::Program;

/// The entries, by class (the tests have one test per class).
pub(super) const CLASSES: [(&str, &[&str]); 8] = [
    (
        "interpreters",
        &[
            "python*", "perl*", "ruby*", "node*", "bun", "deno", "java", "lua*", "php*", "dotnet",
            "tclsh", "awk", "gawk", "gjs", "mono", "julia", "pwsh",
        ],
    ),
    ("toolchains", &["go", "cargo", "rustc"]),
    (
        "shells and runners",
        &[
            "bash", "sh", "zsh", "fish", "dash", "ksh", "csh", "tcsh", "busybox", "env", "xargs",
        ],
    ),
    ("privilege tools", &["sudo", "pkexec"]),
    ("editors", &["vim", "vi", "emacs"]),
    (
        "network and transfer tools",
        &[
            "curl", "wget", "aria2c", "ssh", "scp", "sftp", "rsync", "telnet", "ftp", "lftp", "nc",
            "ncat", "netcat", "socat", "nmap", "openssl",
        ],
    ),
    (
        "debuggers and package tools",
        &["gdb", "pip*", "pipx", "uv", "git"],
    ),
    (
        "containers, emulators and system package tools",
        &[
            "podman",
            "docker",
            "qemu*",
            "wine*",
            "flatpak",
            "rpm-ostree",
        ],
    ),
];

fn entry_matches(entry: &str, name: &str) -> bool {
    match entry.strip_suffix('*') {
        Some(stem) => name.starts_with(stem),
        None => name == entry,
    }
}

fn listed(name: &str) -> bool {
    let name = basename(name).to_ascii_lowercase();
    CLASSES
        .iter()
        .flat_map(|(_, entries)| entries.iter())
        .any(|entry| entry_matches(entry, &name))
}

/// Whether the program's name or its file name is on the list.
pub fn is_dual_use(program: &Program) -> bool {
    listed(&program.raw_name) || program.raw_path.as_deref().is_some_and(listed)
}

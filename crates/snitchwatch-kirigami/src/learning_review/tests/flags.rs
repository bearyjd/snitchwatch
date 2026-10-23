//! Each flag and the fact it is worded as.

use super::*;
use crate::learned::PathFlag;

fn flags_of(program: Program) -> Flags {
    one(program).flags
}

#[test]
fn an_unflagged_program_has_no_flags_and_no_facts() {
    let p = one(prog("app", "/usr/bin/app", &[("a.example", 5)]));
    assert!(!p.flags.any());
    assert_eq!(p.flags.bare_ip, (0, 5));
    assert_eq!(p.flags.uncommon_port, (0, 5));
    assert!(p.describe_flags().is_empty());
}

#[test]
fn a_user_writable_or_temporary_path_is_flagged_with_its_directory() {
    for (path, dir) in [
        ("/tmp/x", "/tmp"),
        ("/var/tmp/x", "/var/tmp"),
        ("/dev/shm/x", "/dev/shm"),
        ("/home/u/bin/x", "/home"),
        ("/var/home/u/bin/x", "/var/home"),
        ("/root/x", "/root"),
        ("/var/roothome/x", "/var/roothome"),
        ("/run/user/1000/x", "/run/user"),
        ("/run/media/u/stick/x", "/run/media"),
        ("/var/mnt/d/x", "/var/mnt"),
        ("/mnt/x", "/mnt"),
        ("/srv/x", "/srv"),
    ] {
        let p = one(prog("x", path, &[("a.example", 1)]));
        assert!(p.flags.user_writable_path, "{path}");
        assert_eq!(p.flags.path_flag, Some(PathFlag::Under(dir)), "{path}");
        assert_eq!(p.describe_flags(), [format!("runs from {dir}")], "{path}");
    }
}

#[test]
fn a_deleted_file_is_flagged_as_such() {
    let p = one(prog("x", "/tmp/x (deleted)", &[("a.example", 1)]));
    assert!(p.flags.user_writable_path);
    assert_eq!(p.flags.path_flag, Some(PathFlag::Deleted));
    assert_eq!(p.describe_flags(), ["its program file has been deleted"]);
}

#[test]
fn system_and_vendor_directories_are_not_flagged() {
    for path in [
        "/usr/bin/x",
        "/opt/x/x",
        "/usr/local/bin/x",
        "/homebrew/x",
        "/tmpfiles/x",
    ] {
        let f = flags_of(prog("x", path, &[("a.example", 1)]));
        assert!(!f.user_writable_path, "{path}");
        assert_eq!(f.path_flag, None);
    }
}

#[test]
fn bare_ip_connections_are_counted_out_of_all_connections() {
    let mut program = prog("x", "/opt/x/x", &[("a.example", 2), ("1.2.3.4", 3)]);
    program.bare_ip = 3;
    let p = one(program);
    assert_eq!(p.flags.bare_ip, (3, 5));
    assert!(p.flags.any());
    assert_eq!(p.describe_flags(), ["3 of 5 connections to bare IPs"]);
}

#[test]
fn the_bare_ip_fact_reads_singular_and_plural() {
    let mut one_of_five = prog("x", "/opt/x/x", &[("a.example", 4), ("1.2.3.4", 1)]);
    one_of_five.bare_ip = 1;
    assert_eq!(
        one(one_of_five).describe_flags(),
        ["1 of 5 connections to a bare IP"]
    );
    let mut only = prog("x", "/opt/x/x", &[("1.2.3.4", 1)]);
    only.bare_ip = 1;
    assert_eq!(
        one(only).describe_flags(),
        ["1 of 1 connection to a bare IP"]
    );
}

#[test]
fn an_uncommon_port_is_a_fact_with_its_count() {
    let mut program = prog("x", "/opt/x/x", &[("a.example", 5)]);
    program.uncommon_port = 2;
    let p = one(program);
    assert_eq!(p.flags.uncommon_port, (2, 5));
    assert_eq!(
        p.describe_flags(),
        ["uses an uncommon port (2 of 5 connections)"]
    );
}

#[test]
fn a_system_name_outside_the_system_directories_is_flagged() {
    for name in [
        "systemd",
        "sshd",
        "NetworkManager",
        "dbus-daemon",
        "chronyd",
        "crond",
        "init",
        "kworker",
    ] {
        for path in [
            "/tmp/x/sshd",
            "/opt/x/sshd",
            "/home/u/sshd",
            "/usr/local/bin/sshd",
        ] {
            let p = one(prog(name, path, &[("a.example", 1)]));
            assert!(p.flags.name_mimics_system, "{name} at {path}");
            assert_eq!(
                p.describe_flags().last().map(String::as_str),
                Some("named like a system program but not run from a system folder"),
                "{name} at {path}"
            );
        }
        // A name with no path at all is just as free to choose its name.
        assert!(
            one(nameless(name, &[("a.example", 1)]))
                .flags
                .name_mimics_system,
            "{name}"
        );
        // From the system's own directories it is the real thing.
        for path in [
            "/usr/sbin/sshd",
            "/usr/bin/sshd",
            "/usr/lib/systemd/systemd",
            "/bin/sshd",
        ] {
            let p = one(prog(name, path, &[("a.example", 1)]));
            assert!(!p.flags.name_mimics_system, "{name} at {path}");
        }
    }
    // Kernel threads name themselves `kworker/0:1`; a path-like name counts too.
    for name in [
        "kworker/0:1",
        "ksoftirqd/0",
        "/tmp/sshd",
        "systemd-resolved",
    ] {
        let at = one(prog(name, "/opt/x/w", &[("a.example", 1)]));
        assert!(at.flags.name_mimics_system, "{name}");
        assert!(
            one(nameless(name, &[("a.example", 1)]))
                .flags
                .name_mimics_system,
            "{name}"
        );
    }
    for name in ["sshd-agent", "my-init", "app", "Sshd"] {
        let p = one(prog(name, "/tmp/x", &[("a.example", 1)]));
        assert!(!p.flags.name_mimics_system, "{name}");
    }
}

#[test]
fn all_the_facts_come_in_a_fixed_order() {
    let mut program = prog("sshd", "/tmp/sshd", &[("1.2.3.4", 5)]);
    program.bare_ip = 5;
    program.uncommon_port = 5;
    assert_eq!(
        one(program).describe_flags(),
        [
            "runs from /tmp",
            "5 of 5 connections to bare IPs",
            "uses an uncommon port (5 of 5 connections)",
            "named like a system program but not run from a system folder",
        ]
    );
}

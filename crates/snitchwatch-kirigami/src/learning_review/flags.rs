//! The objective flags and their wording. Facts only: a path, a count, a
//! name. Nothing here says what a program IS.

use super::paths::{basename, is_system_path};
use super::types::Flags;
use crate::learned::{path_flag, PathFlag, Program};

/// Names of well-known system programs (a small closed list). A program
/// with one of these names that does not run from the system's own
/// directories is flagged: a program chooses its own name.
const SYSTEM_NAMES: [&str; 32] = [
    "systemd",
    "sshd",
    "NetworkManager",
    "dbus-daemon",
    "dbus-broker",
    "chronyd",
    "crond",
    "cron",
    "init",
    "kworker",
    "kthreadd",
    "ksoftirqd",
    "rpm-ostree",
    "rpm-ostreed",
    "polkitd",
    "avahi-daemon",
    "wpa_supplicant",
    "dhclient",
    "dnsmasq",
    "firewalld",
    "auditd",
    "rsyslogd",
    "bluetoothd",
    "udisksd",
    "upowerd",
    "ModemManager",
    "gdm",
    "sddm",
    "login",
    "agetty",
    "cupsd",
    "irqbalance",
];

/// Prefixes of families of system services.
const SYSTEM_PREFIXES: [&str; 2] = ["systemd-", "dbus-"];

/// Whether `name` is a known system program or service name. A kernel thread
/// names itself `kworker/0:1`, and a name may be a whole path, so the part
/// before the first `/` and the last component count too.
pub(super) fn is_system_service(name: &str) -> bool {
    let head = name.split('/').next().unwrap_or(name);
    [name, head, basename(name)].into_iter().any(|candidate| {
        SYSTEM_NAMES.contains(&candidate)
            || SYSTEM_PREFIXES.iter().any(|p| candidate.starts_with(p))
    })
}

/// Outside the system's own directories (`/usr/local` is not the system's).
fn outside_system(path: Option<&str>) -> bool {
    path.is_none_or(|p| !is_system_path(p) || p.starts_with("/usr/local/"))
}

fn name_mimics_system(program: &Program) -> bool {
    is_system_service(&program.raw_name) && outside_system(program.raw_path.as_deref())
}

fn connections_word(n: u64) -> &'static str {
    if n == 1 {
        "connection"
    } else {
        "connections"
    }
}

impl Flags {
    pub fn of(program: &Program) -> Self {
        let flag = program.raw_path.as_deref().and_then(path_flag);
        Flags {
            user_writable_path: flag.is_some(),
            path_flag: flag,
            bare_ip: (program.bare_ip, program.connections),
            uncommon_port: (program.uncommon_port, program.connections),
            name_mimics_system: name_mimics_system(program),
        }
    }

    /// Whether any flag is set.
    pub fn any(&self) -> bool {
        self.user_writable_path
            || self.bare_ip.0 > 0
            || self.uncommon_port.0 > 0
            || self.name_mimics_system
    }

    /// The flags as plain facts, one sentence each, in a fixed order.
    pub fn describe(&self) -> Vec<String> {
        let mut facts = Vec::new();
        match self.path_flag {
            Some(PathFlag::Under(dir)) => facts.push(format!("runs from {dir}")),
            Some(PathFlag::Deleted) => facts.push("its program file has been deleted".to_string()),
            None => {}
        }
        let (bare, of) = self.bare_ip;
        if bare > 0 {
            let what = if bare == 1 { "a bare IP" } else { "bare IPs" };
            facts.push(format!("{bare} of {of} {} to {what}", connections_word(of)));
        }
        let (uncommon, of) = self.uncommon_port;
        if uncommon > 0 {
            facts.push(format!(
                "uses an uncommon port ({uncommon} of {of} {})",
                connections_word(of)
            ));
        }
        if self.name_mimics_system {
            facts.push("named like a system program but not run from a system folder".to_string());
        }
        facts
    }
}

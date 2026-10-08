//! The raw text the Simulate sheet sends, and how it becomes a
//! [`SimulationInput`].
//!
//! Qt-free so the blank-means-unknown rules are unit-tested. The sheet sends
//! every field as typed (one JSON object, camelCase keys) instead of the
//! model taking fifteen positional arguments.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::SimulationInput;

/// Fields as typed in the Simulate sheet.
///
/// **A blank field means unknown**, so conditions that need it are reported as
/// not evaluated. The one exception is the destination host: a blank one is
/// the empty `DstHost` a bare-IP connection has. The port and protocol always
/// have a value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SimulationForm {
    pub process_path: String,
    pub dest_host: String,
    pub dest_port: i64,
    pub protocol: String,
    /// One ancestor path per line, nearest first.
    pub parent_paths: String,
    pub command: String,
    pub pid: String,
    pub uid: String,
    /// `NAME=value`, one per line. Variables not listed count as unset.
    pub env: String,
    pub src_ip: String,
    pub src_port: String,
    pub dest_ip: String,
    pub iface_in: String,
    pub iface_out: String,
    /// `"unknown"` (the default), `"off"`, `"on"` (the program's MD5 is in
    /// `md5`; blank means unknown) or `"on-none"` (checksums are on and the
    /// program has none recorded). There is no SHA1: v1.8.0 only computes MD5.
    pub checksums: String,
    pub md5: String,
}

impl SimulationForm {
    pub fn to_input(&self) -> SimulationInput {
        let (checksums_enabled, checksums) = self.checksum_state();
        SimulationInput {
            process_path: text(&self.process_path),
            dest_host: self.dest_host.trim().to_string(),
            dest_port: self.dest_port.clamp(0, i64::from(u16::MAX)) as u16,
            protocol: text(&self.protocol),
            parent_paths: self.ancestors(),
            command: text(&self.command),
            pid: number(&self.pid),
            uid: number(&self.uid),
            env: self.environment(),
            src_ip: text(&self.src_ip),
            src_port: number(&self.src_port),
            dest_ip: text(&self.dest_ip),
            iface_in: text(&self.iface_in),
            iface_out: text(&self.iface_out),
            checksums,
            checksums_enabled,
        }
    }

    /// One path per line; no path at all means unknown.
    fn ancestors(&self) -> Option<Vec<String>> {
        let paths: Vec<String> = self
            .parent_paths
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
        (!paths.is_empty()).then_some(paths)
    }

    /// `NAME=value` lines; text with no variable in it means unknown.
    fn environment(&self) -> Option<BTreeMap<String, String>> {
        let env: BTreeMap<String, String> = self
            .env
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(name, value)| (name.trim().to_string(), value.to_string()))
            .collect();
        (!env.is_empty()).then_some(env)
    }

    /// `(checksums enabled, the program's checksums)` for the chosen mode.
    fn checksum_state(&self) -> (Option<bool>, Option<BTreeMap<String, String>>) {
        match self.checksums.as_str() {
            "off" => (Some(false), None),
            "on" => (
                Some(true),
                // The daemon records lowercase hex and compares exactly.
                text(&self.md5)
                    .map(|md5| BTreeMap::from([("md5".to_string(), md5.to_lowercase())])),
            ),
            "on-none" => (Some(true), Some(BTreeMap::new())),
            _ => (None, None),
        }
    }
}

/// Blank (or whitespace) means unknown.
fn text(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Digits only: blank, a sign or anything out of range is unknown.
fn number<T: std::str::FromStr>(raw: &str) -> Option<T> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    trimmed.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(json: serde_json::Value) -> SimulationInput {
        serde_json::from_value::<SimulationForm>(json)
            .expect("form json")
            .to_input()
    }

    #[test]
    fn a_blank_form_is_all_unknown_except_the_basic_fields() {
        let input = form(serde_json::json!({}));
        assert_eq!(input, SimulationInput::default());
        assert_eq!(input.parent_paths, None);
        assert_eq!(input.command, None);
        assert_eq!(input.pid, None);
        assert_eq!(input.uid, None);
        assert_eq!(input.env, None);
        assert_eq!(input.src_ip, None);
        assert_eq!(input.src_port, None);
        assert_eq!(input.dest_ip, None);
        assert_eq!(input.iface_in, None);
        assert_eq!(input.iface_out, None);
        assert_eq!(input.checksums, None);
        assert_eq!(input.checksums_enabled, None);
    }

    #[test]
    fn whitespace_only_advanced_fields_are_still_unknown() {
        let input = form(serde_json::json!({
            "processPath": "  ", "parentPaths": "  \n \n", "command": "   ", "pid": " ", "uid": "\t",
            "env": "\n", "srcIp": " ", "srcPort": " ", "destIp": " ",
            "ifaceIn": " ", "ifaceOut": " ", "md5": " "
        }));
        assert_eq!(input, SimulationInput::default());
    }

    #[test]
    fn basic_fields_are_trimmed_and_a_blank_host_is_a_known_empty_host() {
        let input = form(serde_json::json!({
            "processPath": "  /usr/bin/curl ", "destHost": "", "destPort": 443, "protocol": " tcp "
        }));
        assert_eq!(input.process_path.as_deref(), Some("/usr/bin/curl"));
        assert_eq!(input.dest_host, "");
        assert_eq!(input.dest_port, 443);
        assert_eq!(input.protocol.as_deref(), Some("tcp"));
    }

    #[test]
    fn the_destination_port_is_clamped_to_a_port() {
        assert_eq!(form(serde_json::json!({"destPort": -5})).dest_port, 0);
        assert_eq!(
            form(serde_json::json!({"destPort": 70000})).dest_port,
            65535
        );
    }

    #[test]
    fn numbers_parse_and_anything_else_is_unknown() {
        let input = form(serde_json::json!({
            "pid": "1234", "uid": " 1000 ", "srcPort": "51000"
        }));
        assert_eq!(input.pid, Some(1234));
        assert_eq!(input.uid, Some(1000));
        assert_eq!(input.src_port, Some(51000));

        let input = form(serde_json::json!({
            "pid": "abc", "uid": "-1", "srcPort": "70000"
        }));
        assert_eq!(input.pid, None);
        assert_eq!(input.uid, None);
        assert_eq!(input.src_port, None);
        // `u32::from_str` accepts a leading plus; the daemon never prints one.
        assert_eq!(form(serde_json::json!({"uid": "+5"})).uid, None);
    }

    #[test]
    fn text_fields_are_trimmed() {
        let input = form(serde_json::json!({
            "command": " /usr/bin/curl -s ", "srcIp": " 10.0.0.1 ", "destIp": "10.0.0.2\n",
            "ifaceIn": " eth0 ", "ifaceOut": "wlan0 "
        }));
        assert_eq!(input.command.as_deref(), Some("/usr/bin/curl -s"));
        assert_eq!(input.src_ip.as_deref(), Some("10.0.0.1"));
        assert_eq!(input.dest_ip.as_deref(), Some("10.0.0.2"));
        assert_eq!(input.iface_in.as_deref(), Some("eth0"));
        assert_eq!(input.iface_out.as_deref(), Some("wlan0"));
    }

    #[test]
    fn parent_paths_are_one_per_line_and_blank_lines_are_skipped() {
        let input = form(serde_json::json!({
            "parentPaths": "/usr/bin/bash\r\n\n /usr/lib/systemd/systemd\n"
        }));
        assert_eq!(
            input.parent_paths,
            Some(vec![
                "/usr/bin/bash".to_string(),
                "/usr/lib/systemd/systemd".to_string()
            ])
        );
    }

    #[test]
    fn env_lines_are_name_equals_value_and_other_lines_are_ignored() {
        let input = form(serde_json::json!({
            "env": "HOME=/home/u\n PATH = /usr/bin:/bin\nnot a variable\nEMPTY=\nA=b=c\r\n"
        }));
        let env = input.env.expect("env known");
        assert_eq!(env.get("HOME").map(String::as_str), Some("/home/u"));
        // The name is trimmed; the value is kept exactly as typed.
        assert_eq!(env.get("PATH").map(String::as_str), Some(" /usr/bin:/bin"));
        assert_eq!(env.get("EMPTY").map(String::as_str), Some(""));
        assert_eq!(env.get("A").map(String::as_str), Some("b=c"));
        assert_eq!(env.len(), 4, "{env:?}");
    }

    #[test]
    fn text_with_no_variable_in_it_leaves_the_environment_unknown() {
        assert_eq!(form(serde_json::json!({"env": "just words"})).env, None);
    }

    #[test]
    fn checksum_modes_map_to_what_the_daemon_knows() {
        let input = form(serde_json::json!({"checksums": "unknown", "md5": "abc"}));
        assert_eq!((input.checksums_enabled, input.checksums), (None, None));

        let input = form(serde_json::json!({"checksums": "off", "md5": "abc"}));
        assert_eq!(
            (input.checksums_enabled, input.checksums),
            (Some(false), None)
        );

        // On, with the program's checksum typed in.
        let input = form(serde_json::json!({"checksums": "on", "md5": " ABC "}));
        assert_eq!(input.checksums_enabled, Some(true));
        let sums = input.checksums.expect("checksums known");
        assert_eq!(sums.get("md5").map(String::as_str), Some("abc"));

        // There is no SHA1 field: v1.8.0 only ever computes the MD5.
        let input = form(serde_json::json!({"checksums": "on", "sha1": "cd"}));
        assert_eq!(
            (input.checksums_enabled, input.checksums),
            (Some(true), None)
        );

        // On, checksum left blank: unknown, not "none recorded".
        let input = form(serde_json::json!({"checksums": "on"}));
        assert_eq!(
            (input.checksums_enabled, input.checksums),
            (Some(true), None)
        );

        // On, and the program has none recorded.
        let input = form(serde_json::json!({"checksums": "on-none", "md5": "ignored"}));
        assert_eq!(input.checksums_enabled, Some(true));
        assert_eq!(input.checksums, Some(Default::default()));
    }

    #[test]
    fn an_unrecognised_checksum_mode_is_unknown() {
        let input = form(serde_json::json!({"checksums": "banana"}));
        assert_eq!((input.checksums_enabled, input.checksums), (None, None));
    }
}

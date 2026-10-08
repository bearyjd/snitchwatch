//! The few settings the bridge reads from opensnitchd's own configuration,
//! the JSON string in `ClientConfig.config` that `subscribe` receives
//! (prompt-slot plan, item 10).
//!
//! Parsed the way the daemon parses it (`vendor:daemon/ui/config/config.go`
//! `Parse`, Go's `encoding/json`): into the daemon's whole `Config`, keys
//! matched case-insensitively, `null` ignored, unknown keys ignored. If the
//! daemon's parse would fail, a value of the wrong type anywhere, it keeps
//! its *previous* default action, which the bridge can't know. So any parse
//! error, or two keys for one setting, gives an all-`None` view: the row
//! then says "the firewall's default action". The raw string is never
//! logged; it holds the daemon's server address and TLS paths.
//!
//! **What `default_action` means while a GUI is connected.** When an
//! `AskRule` fails, the daemon applies `clientConnectedRule.Action`
//! (`vendor:daemon/main.go` `applyDefaultAction`, `ui/client.go`
//! `DefaultAction`). The daemon sets that from the `DefaultAction` of the
//! config the GUI echoes back in its `Subscribe` reply
//! (`vendor:daemon/ui/notifications.go` `Subscribe`). The bridge echoes the
//! daemon's own config unchanged, so the daemon's `DefaultAction` is what an
//! unanswered prompt gets. The failed `Ask` stores no rule.

use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};

/// The settings the bridge uses. Each is `None` when the daemon's config
/// lacks it or gives it the wrong type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DaemonConfigView {
    /// `DefaultAction`: `"allow"`, `"deny"` or `"reject"` on a sane daemon.
    pub default_action: Option<String>,
    /// `Stats.MaxEvents`.
    pub max_events: Option<u32>,
    /// `Rules.EnableChecksums`.
    pub checksums_enabled: Option<bool>,
}

impl DaemonConfigView {
    pub fn parse(raw: &str) -> Self {
        let config = serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(fold_keys)
            .filter(structs_are_objects)
            .and_then(|folded| serde_json::from_value::<GoConfig>(folded).ok());
        let Some(config) = config else {
            return Self::default();
        };
        Self {
            default_action: config.defaultaction,
            max_events: config
                .stats
                .and_then(|stats| stats.maxevents)
                .and_then(|n| u32::try_from(n).ok()),
            checksums_enabled: config.rules.and_then(|rules| rules.enablechecksums),
        }
    }

    /// The `ConnectionRow.action` for a connection the daemon answered with
    /// its default action, when the bridge knows which one that is. `reject`
    /// drops the packet and kills the socket, so it shows as `"deny"`.
    /// Anything else, including an empty value (which the daemon also
    /// drops), is `None`: the row then names no action.
    pub fn default_row_action(&self) -> Option<&'static str> {
        match self.default_action.as_deref()? {
            "allow" => Some("allow"),
            "deny" | "reject" => Some("deny"),
            _ => None,
        }
    }
}

/// `value` with every object key case-folded as Go matches struct fields
/// (`ſ` folds to `s`; the Kelvin sign lowercases to `k`). `None` when two
/// keys of one object fold together: which one the daemon used is unclear.
fn fold_keys(value: Value) -> Option<Value> {
    match value {
        Value::Object(object) => {
            let mut folded = Map::new();
            for (key, value) in object {
                let key: String = key
                    .chars()
                    .flat_map(char::to_lowercase)
                    .map(|c| if c == '\u{17f}' { 's' } else { c })
                    .collect();
                if folded.insert(key, fold_keys(value)?).is_some() {
                    return None;
                }
            }
            Some(Value::Object(folded))
        }
        Value::Array(items) => items
            .into_iter()
            .map(fold_keys)
            .collect::<Option<Vec<_>>>()
            .map(Value::Array),
        other => Some(other),
    }
}

/// Whether every Go struct in `config` is a JSON object (or `null`). serde
/// would also read a struct from an array of its fields; Go wouldn't.
fn structs_are_objects(config: &Value) -> bool {
    fn object_or_null(value: Option<&Value>) -> bool {
        matches!(value, None | Some(Value::Null) | Some(Value::Object(_)))
    }
    let Value::Object(top) = config else {
        return false;
    };
    let nested = [
        "fwoptions",
        "audit",
        "ebpf",
        "server",
        "rules",
        "internal",
        "stats",
        "tasks",
    ];
    let server = top.get("server");
    let auth = server.and_then(|server| server.get("authentication"));
    let loggers_ok = match server.and_then(|server| server.get("loggers")) {
        None | Some(Value::Null) => true,
        Some(Value::Array(items)) => items.iter().all(|item| object_or_null(Some(item))),
        Some(_) => true, // not an array: the typed parse refuses it
    };
    nested.iter().all(|key| object_or_null(top.get(*key)))
        && object_or_null(auth)
        && object_or_null(auth.and_then(|auth| auth.get("tlsoptions")))
        && loggers_ok
}

// The daemon's `config.Config` and every type inside it, field for field
// (`vendor:daemon/ui/config/config.go`, `procmon/audit`, `procmon/ebpf`,
// `statistics`, `log/loggers`), with keys already folded. Go's `int` is
// 64-bit. `Option` lets `null` and absent keys through, as Go does.
// Most fields are read only to check their type, as the daemon's parse does.
mod go {
    #![allow(dead_code)]
    use serde::Deserialize;

    #[derive(Deserialize)]
    pub(super) struct GoConfig {
        loglevel: Option<i32>,
        firewall: Option<String>,
        pub(super) defaultaction: Option<String>,
        defaultduration: Option<String>,
        procmonitormethod: Option<String>,
        fwoptions: Option<GoFwOptions>,
        audit: Option<GoAudit>,
        ebpf: Option<GoEbpf>,
        server: Option<GoServer>,
        pub(super) rules: Option<GoRules>,
        internal: Option<GoInternal>,
        pub(super) stats: Option<GoStats>,
        tasks: Option<GoTasks>,
        interceptunknown: Option<bool>,
        logutc: Option<bool>,
        logmicro: Option<bool>,
    }

    #[derive(Deserialize)]
    pub(super) struct GoFwOptions {
        firewall: Option<String>,
        configpath: Option<String>,
        monitorinterval: Option<String>,
        queuenum: Option<u16>,
        queuebypass: Option<bool>,
    }

    #[derive(Deserialize)]
    pub(super) struct GoAudit {
        audispsocketpath: Option<String>,
    }

    #[derive(Deserialize)]
    pub(super) struct GoEbpf {
        modulespath: Option<String>,
        ringbuffsize: Option<i64>,
        eventsworkers: Option<i64>,
        queueeventssize: Option<i64>,
    }

    #[derive(Deserialize)]
    pub(super) struct GoServer {
        address: Option<String>,
        authentication: Option<GoAuth>,
        logfile: Option<String>,
        loggers: Option<Vec<Option<GoLogger>>>,
    }

    #[derive(Deserialize)]
    pub(super) struct GoAuth {
        r#type: Option<String>,
        tlsoptions: Option<GoTls>,
    }

    #[derive(Deserialize)]
    pub(super) struct GoTls {
        cacert: Option<String>,
        servercert: Option<String>,
        serverkey: Option<String>,
        clientcert: Option<String>,
        clientkey: Option<String>,
        clientauthtype: Option<String>,
        skipverify: Option<bool>,
    }

    #[derive(Deserialize)]
    pub(super) struct GoLogger {
        name: Option<String>,
        format: Option<String>,
        protocol: Option<String>,
        server: Option<String>,
        writetimeout: Option<String>,
        connecttimeout: Option<String>,
        tag: Option<String>,
        workers: Option<i64>,
        maxconnectattempts: Option<u16>,
    }

    #[derive(Deserialize)]
    pub(super) struct GoRules {
        path: Option<String>,
        pub(super) enablechecksums: Option<bool>,
    }

    #[derive(Deserialize)]
    pub(super) struct GoInternal {
        gcpercent: Option<i64>,
        flushconnsonstart: Option<bool>,
    }

    #[derive(Deserialize)]
    pub(super) struct GoStats {
        pub(super) maxevents: Option<i64>,
        maxstats: Option<i64>,
        workers: Option<i64>,
    }

    #[derive(Deserialize)]
    pub(super) struct GoTasks {
        configpath: Option<String>,
    }
}
use go::GoConfig;

/// The view from the latest `subscribe`, shared between the gRPC service
/// (which writes it) and whoever labels a default-action answer. `None`
/// until the daemon has subscribed.
#[derive(Debug, Clone, Default)]
pub struct SharedDaemonConfig(Arc<Mutex<Option<DaemonConfigView>>>);

impl SharedDaemonConfig {
    pub fn set(&self, view: DaemonConfigView) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(view);
    }

    pub fn get(&self) -> Option<DaemonConfigView> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// [`DaemonConfigView::default_row_action`] of the current view.
    pub fn default_row_action(&self) -> Option<&'static str> {
        self.get()?.default_row_action()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VENDOR_DEFAULT: &str =
        include_str!("../../../vendor/opensnitch/daemon/data/default-config.json");
    const PACKAGED: &str = include_str!(
        "../../../packaging/bluebuild/files/system/etc/opensnitchd/default-config.json"
    );

    #[test]
    fn the_vendor_default_config_allows_and_the_packaged_one_denies() {
        assert_eq!(
            DaemonConfigView::parse(VENDOR_DEFAULT),
            DaemonConfigView {
                default_action: Some("allow".into()),
                max_events: Some(250),
                checksums_enabled: Some(false),
            }
        );
        let packaged = DaemonConfigView::parse(PACKAGED);
        assert_eq!(packaged.default_action.as_deref(), Some("deny"));
        assert_eq!(packaged.default_row_action(), Some("deny"));
    }

    #[test]
    fn garbage_missing_keys_and_wrong_types_are_none() {
        for raw in ["", "not json", "[1, 2]", "null", "{}", "\"allow\""] {
            assert_eq!(
                DaemonConfigView::parse(raw),
                DaemonConfigView::default(),
                "{raw:?}"
            );
        }
        let wrong_types = r#"{
            "DefaultAction": 1,
            "Stats": { "MaxEvents": "250" },
            "Rules": { "EnableChecksums": "yes" }
        }"#;
        assert_eq!(
            DaemonConfigView::parse(wrong_types),
            DaemonConfigView::default()
        );
        let out_of_range = r#"{ "Stats": { "MaxEvents": 99999999999 } }"#;
        assert_eq!(DaemonConfigView::parse(out_of_range).max_events, None);
        let negative = r#"{ "Stats": { "MaxEvents": -1 } }"#;
        assert_eq!(DaemonConfigView::parse(negative).max_events, None);
        let not_objects = r#"{ "Stats": 5, "Rules": [true] }"#;
        assert_eq!(
            DaemonConfigView::parse(not_objects),
            DaemonConfigView::default()
        );
    }

    #[test]
    fn keys_match_case_insensitively_as_go_does() {
        let view = DaemonConfigView::parse(
            r#"{"defaultaction": "deny", "STATS": {"maxevents": 7}, "Rules": {"enableChecksums": true}}"#,
        );
        assert_eq!(view.default_action.as_deref(), Some("deny"));
        assert_eq!(view.max_events, Some(7));
        assert_eq!(view.checksums_enabled, Some(true));
        let long_s = DaemonConfigView::parse("{\"Stats\": {\"MaxEvent\u{17f}\": 9}}");
        assert_eq!(long_s.max_events, Some(9));
    }

    #[test]
    fn a_type_error_anywhere_means_nothing_is_known() {
        // The daemon's parse fails, so it keeps its previous default action.
        for raw in [
            r#"{"DefaultAction": "deny", "LogLevel": "verbose"}"#,
            r#"{"DefaultAction": "deny", "Server": {"Address": 5}}"#,
            r#"{"DefaultAction": "deny", "FwOptions": {"QueueNum": -1}}"#,
            r#"{"DefaultAction": "deny", "FwOptions": {"QueueNum": 70000}}"#,
            r#"{"DefaultAction": "deny", "Server": {"Loggers": [{"Workers": "4"}]}}"#,
            r#"{"DefaultAction": "deny", "Stats": {"MaxEvents": 1.5}}"#,
            r#"{"DefaultAction": "deny", "Ebpf": []}"#,
            r#"{"DefaultAction": "deny", "Ebpf": ["/lib", 1, 2, 3]}"#,
            r#"{"DefaultAction": "deny", "Server": {"Loggers": [["a"]]}}"#,
            r#"{"DefaultAction": "deny", "Server": {"Authentication": {"TLSOptions": [""]}}}"#,
            r#"{"DefaultAction": "deny", "LogLevel": 3000000000}"#,
        ] {
            assert_eq!(
                DaemonConfigView::parse(raw),
                DaemonConfigView::default(),
                "{raw}"
            );
        }
    }

    #[test]
    fn two_keys_for_one_setting_are_ambiguous() {
        for raw in [
            r#"{"DefaultAction": "allow", "defaultaction": "deny"}"#,
            r#"{"DefaultAction": "deny", "Stats": {"MaxEvents": 1, "maxEvents": 2}}"#,
        ] {
            assert_eq!(
                DaemonConfigView::parse(raw),
                DaemonConfigView::default(),
                "{raw}"
            );
        }
    }

    #[test]
    fn null_and_unknown_keys_are_ignored_as_go_does() {
        let view = DaemonConfigView::parse(
            r#"{"DefaultAction": "deny", "Stats": null, "Server": {"Loggers": [null]},
                "SomethingNewer": {"x": [1, "y"]}}"#,
        );
        assert_eq!(view.default_action.as_deref(), Some("deny"));
        assert_eq!(view.max_events, None);
    }

    #[test]
    fn only_known_actions_name_a_row_action() {
        let row_action = |action: &str| {
            DaemonConfigView {
                default_action: Some(action.into()),
                ..Default::default()
            }
            .default_row_action()
        };
        assert_eq!(row_action("allow"), Some("allow"));
        assert_eq!(row_action("deny"), Some("deny"));
        assert_eq!(row_action("reject"), Some("deny"));
        for unknown in ["", "Allow", "drop", "allow "] {
            assert_eq!(row_action(unknown), None, "{unknown:?}");
        }
        assert_eq!(DaemonConfigView::default().default_row_action(), None);
    }

    #[test]
    fn the_shared_view_is_empty_until_set() {
        let shared = SharedDaemonConfig::default();
        assert_eq!(shared.get(), None);
        assert_eq!(shared.default_row_action(), None);
        shared.set(DaemonConfigView::parse(PACKAGED));
        assert_eq!(shared.default_row_action(), Some("deny"));
        assert_eq!(shared.clone().get().unwrap().max_events, Some(250));
    }
}

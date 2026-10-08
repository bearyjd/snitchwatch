//! The few settings the bridge reads from opensnitchd's own configuration,
//! the JSON string in `ClientConfig.config` that `subscribe` receives
//! (prompt-slot plan, item 10).
//!
//! Parsed defensively: a field that is missing or has the wrong type is
//! `None`, and malformed JSON gives an all-`None` view. The raw string is
//! never logged; it holds the daemon's server address and TLS paths.
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

use serde_json::Value;

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
        let Ok(config) = serde_json::from_str::<Value>(raw) else {
            return Self::default();
        };
        Self {
            default_action: config
                .get("DefaultAction")
                .and_then(Value::as_str)
                .map(str::to_owned),
            max_events: config
                .pointer("/Stats/MaxEvents")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok()),
            checksums_enabled: config
                .pointer("/Rules/EnableChecksums")
                .and_then(Value::as_bool),
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

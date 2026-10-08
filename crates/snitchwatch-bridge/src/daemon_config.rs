//! What the bridge reads from opensnitchd's `ClientConfig.config` (the
//! daemon's `default-config.json`, as a string, sent on `Subscribe`).

use crate::cache::rule_hits::DEFAULT_MAX_EVENTS;

/// `Stats.MaxEvents`: how many matched connections one ping can carry. Like
/// the daemon (`stats.go`), a value that isn't positive, and a config that is
/// missing or can't be read, mean [`DEFAULT_MAX_EVENTS`].
pub fn stats_max_events(config: &str) -> usize {
    serde_json::from_str::<serde_json::Value>(config)
        .ok()
        .and_then(|config| config.get("Stats")?.get("MaxEvents")?.as_i64())
        .and_then(|max| usize::try_from(max).ok())
        .filter(|max| *max > 0)
        .unwrap_or(DEFAULT_MAX_EVENTS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_daemons_value() {
        assert_eq!(
            stats_max_events(r#"{"Stats":{"MaxEvents":250,"MaxStats":25}}"#),
            250
        );
        assert_eq!(
            stats_max_events(r#"{"DefaultAction":"deny","Stats":{"MaxEvents":10}}"#),
            10
        );
    }

    #[test]
    fn anything_else_is_the_daemons_default() {
        for config in [
            "",
            "not json",
            "{}",
            r#"{"Stats":{}}"#,
            r#"{"Stats":{"MaxEvents":0}}"#,
            r#"{"Stats":{"MaxEvents":-5}}"#,
            r#"{"Stats":{"MaxEvents":"250"}}"#,
            r#"{"Stats":[1]}"#,
        ] {
            assert_eq!(stats_max_events(config), DEFAULT_MAX_EVENTS, "{config:?}");
        }
    }
}

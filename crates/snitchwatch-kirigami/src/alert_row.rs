//! What the standalone alert (`qml/AlertWindow.qml`) shows: the oldest row
//! still waiting for an answer, as one JSON object. Qt-free, so the choice of
//! row and every field is unit-tested here; `ConnectionsModel.alertRowJson`
//! only supplies the store.

use snitchwatch_bridge::translator::process_binding::is_bindable_process_path;
use snitchwatch_bridge::ws_messages::ConnectionRow;

use crate::connections::outcome::is_pending;
use crate::connections::row_store::RowStore;

/// The oldest pending row (earliest `started_at_ms`; on a tie the row earlier
/// in the store, which `ConnectionsModel.oldestPendingRowIndex` also uses so
/// the alert and the inspector pick the same row, issue #146), as `{"rowId","process","processPath","appIcon","host","port",
/// "protocol","direction","answerDeadlineMs","startedAtMs","dstIp",
/// "bindableProcessPath"}`, or `"{}"` when nothing waits. `answerDeadlineMs`
/// is `-1` when the bridge will never answer it.
pub(crate) fn oldest_pending_json(store: &RowStore) -> String {
    let oldest = store
        .rows()
        .iter()
        .filter(|row| is_pending(row))
        .min_by_key(|row| row.started_at_ms);
    oldest.map_or_else(|| "{}".to_string(), row_json)
}

fn row_json(row: &ConnectionRow) -> String {
    let path = row.process_path.as_deref().unwrap_or("");
    serde_json::json!({
        "rowId": row.id,
        "process": row.process,
        "processPath": path,
        "appIcon": crate::app_icons::icon_for_path(path),
        "host": row.dst_host,
        "port": row.dst_port,
        "protocol": row.protocol,
        "direction": row.direction,
        "answerDeadlineMs": row.answer_deadline_ms.map_or(-1.0, |ms| ms as f64),
        "startedAtMs": row.started_at_ms as f64,
        "dstIp": row.dst_ip,
        "bindableProcessPath": is_bindable_process_path(path),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, started_at_ms: i64, action: Option<&str>) -> ConnectionRow {
        ConnectionRow {
            id: id.to_string(),
            process: "curl".into(),
            process_path: Some("/usr/bin/curl".into()),
            dst_host: "example.com".into(),
            dst_ip: "93.184.216.34".into(),
            dst_port: 443,
            protocol: "tcp".into(),
            direction: "outgoing".into(),
            action: action.map(str::to_string),
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms,
            matched_rule: None,
            auto_answer: None,
            answer_deadline_ms: Some(1_700_000_030_000),
            deferred: false,
            decided_by_default: false,
        }
    }

    fn json_of(rows: Vec<ConnectionRow>) -> serde_json::Value {
        let mut store = RowStore::new();
        store.insert_rows(rows);
        serde_json::from_str(&oldest_pending_json(&store)).unwrap()
    }

    #[test]
    fn nothing_pending_is_an_empty_object() {
        assert_eq!(json_of(vec![]), serde_json::json!({}));
        assert_eq!(
            json_of(vec![row("a", 1, Some("allow"))]),
            serde_json::json!({})
        );
    }

    #[test]
    fn the_oldest_pending_row_wins_and_decided_rows_do_not() {
        let json = json_of(vec![
            row("new", 3_000, None),
            row("decided", 500, Some("deny")),
            row("old", 1_000, None),
        ]);
        assert_eq!(json["rowId"], "old");
        assert_eq!(json["host"], "example.com");
        assert_eq!(json["port"], 443);
        assert_eq!(json["processPath"], "/usr/bin/curl");
        assert_eq!(json["direction"], "outgoing");
        assert_eq!(json["answerDeadlineMs"], 1_700_000_030_000.0);
        assert_eq!(json["bindableProcessPath"], true);
        assert!(json["appIcon"]
            .as_str()
            .is_some_and(|icon| !icon.is_empty()));
    }

    #[test]
    fn an_unknown_program_has_no_path_a_rule_could_bind() {
        let mut unknown = row("k", 1, None);
        unknown.process_path = None;
        unknown.answer_deadline_ms = None;
        let json = json_of(vec![unknown]);
        assert_eq!(json["processPath"], "");
        assert_eq!(json["bindableProcessPath"], false);
        assert_eq!(json["answerDeadlineMs"], -1.0);
    }
}

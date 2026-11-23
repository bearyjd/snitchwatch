//! A daemon ping can repeat the connection already represented by a prompt.
//!
//! Connection reports carry no socket identifier, network namespace, or
//! transport lifetime. Even a complete TCP tuple can be reused between a
//! prompt and a ping. Equal rule identity does not disambiguate that reuse.
//! The view preserves and flags every plausible pair. The flags are computed
//! from durable rows, so late-arriving candidates and rows in the same writer
//! transaction are treated alike. Both lookups use indexed time ranges.

pub(super) const VIEW_SQL: &str = "
CREATE INDEX events_prompt_decided ON events (decided_ts_ms)
    WHERE prompt_outcome IS NOT NULL;

CREATE VIEW history_candidates AS
SELECT p.id AS prompt_id, e.id AS ping_id
FROM events p
JOIN events e ON e.ts_ms BETWEEN p.decided_ts_ms - 2000 AND p.decided_ts_ms + 2000
             AND p.decided_ts_ms BETWEEN e.ts_ms - 2000 AND e.ts_ms + 2000
WHERE p.prompt_outcome IS NOT NULL
  AND e.prompt_outcome IS NULL AND e.decided_ts_ms IS NULL
  AND e.ts_ms >= p.ts_ms - CASE WHEN e.repeat_count > 1 THEN 1000 ELSE 0 END
  AND (e.repeat_count > 1 OR e.ts_ms BETWEEN p.decided_ts_ms - 1000
                                         AND p.decided_ts_ms + 1000)
  AND (p.process_path = e.process_path OR p.process_path IS NULL OR e.process_path IS NULL)
  AND (p.protocol = e.protocol OR p.protocol IS NULL OR e.protocol IS NULL)
  AND (p.dst_ip = e.dst_ip OR p.dst_ip IS NULL OR e.dst_ip IS NULL)
  AND (p.dst_port = e.dst_port OR p.dst_port IS NULL OR e.dst_port IS NULL)
  AND (p.src_ip = e.src_ip OR p.src_ip IS NULL OR e.src_ip IS NULL)
  AND (p.src_port = e.src_port OR p.src_port IS NULL OR e.src_port IS NULL);

CREATE VIEW history_events AS
SELECT e.*,
       CASE WHEN e.prompt_outcome IS NOT NULL THEN EXISTS (
           SELECT 1 FROM history_candidates c WHERE c.prompt_id = e.id
       ) ELSE EXISTS (
           SELECT 1 FROM history_candidates c WHERE c.ping_id = e.id
       ) END AS possible_duplicate
FROM events e;
";

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use crate::history::caps::{ProcessPath, Protocol};
    use crate::history::record::{insert_event, Action, AnsweredBy, EventRecord, PromptOutcome};
    use crate::history::schema::schema_sql;

    fn store() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&schema_sql()).unwrap();
        db
    }

    fn ping() -> EventRecord {
        let mut record = EventRecord::new(11_100, Action::Allow, AnsweredBy::Rule);
        record.process_path = Some(ProcessPath::new("/usr/bin/curl"));
        record.dst_ip = Some("203.0.113.1".parse().unwrap());
        record.dst_port = Some(443);
        record.src_ip = Some("192.0.2.1".parse().unwrap());
        record.src_port = Some(1234);
        record.protocol = Some(Protocol::new("tcp"));
        record.rule_hash = Some([1; 16]);
        record
    }

    fn prompt() -> EventRecord {
        let mut record = ping();
        record.ts_ms = 10_000;
        record.decided_ts_ms = Some(11_000);
        record.answered_by = AnsweredBy::Gui;
        record.prompt_outcome = Some(PromptOutcome::Replied);
        record
    }

    fn logical_rows(db: &Connection) -> Vec<(i64, bool)> {
        let mut statement = db
            .prepare("SELECT id, possible_duplicate FROM history_events ORDER BY id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn even_a_unique_complete_tcp_pair_lacks_a_socket_identity() {
        let mut db = store();
        let tx = db.transaction().unwrap();
        insert_event(&tx, &ping()).unwrap();
        insert_event(&tx, &prompt()).unwrap();
        tx.commit().unwrap();
        assert_eq!(logical_rows(&db), [(1, true), (2, true)]);
        // A second ping might arrive in a subsequent transaction.
        insert_event(&db, &ping()).unwrap();
        assert_eq!(logical_rows(&db), [(1, true), (2, true), (3, true)]);
    }

    #[test]
    fn a_ping_matching_multiple_prompt_rows_flags_each_candidate() {
        let db = store();
        insert_event(&db, &prompt()).unwrap();
        insert_event(&db, &prompt()).unwrap();
        insert_event(&db, &ping()).unwrap();
        assert_eq!(logical_rows(&db), [(1, true), (2, true), (3, true)]);
    }

    #[test]
    fn missing_source_components_and_udp_leave_both_candidates_flagged() {
        for field in ["src_port", "src_ip", "process_path", "udp", "repeat_count"] {
            let db = store();
            let mut ping = ping();
            let mut prompt = prompt();
            match field {
                "src_port" => prompt.src_port = None,
                "src_ip" => ping.src_ip = None,
                "process_path" => prompt.process_path = None,
                "udp" => {
                    prompt.protocol = Some(Protocol::new("udp"));
                    ping.protocol = prompt.protocol.clone();
                }
                "repeat_count" => ping.repeat_count = 2,
                _ => unreachable!(),
            }
            insert_event(&db, &prompt).unwrap();
            insert_event(&db, &ping).unwrap();
            assert_eq!(logical_rows(&db), [(1, true), (2, true)], "{field}");
        }
    }

    #[test]
    fn different_source_addresses_and_events_outside_the_window_are_independent() {
        let db = store();
        insert_event(&db, &prompt()).unwrap();
        let mut other = ping();
        other.src_ip = Some("192.0.2.2".parse().unwrap());
        insert_event(&db, &other).unwrap();
        let mut later = ping();
        later.ts_ms = 12_001;
        insert_event(&db, &later).unwrap();
        assert_eq!(logical_rows(&db), [(1, false), (2, false), (3, false)]);
    }
}

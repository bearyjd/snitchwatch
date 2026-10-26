//! The `retriedCount` role (0d part 2): how many desktop notices of a
//! flow the notice gate held back within the hour, on the flow's newest
//! pending row only. The count is copied from the gate by the notification
//! controller into `BridgeHandles::held_back_notices`; it is read here on
//! the pending-age tick (`refreshPendingAge`), so it lags up to a second.
//! That tick also runs while `heldBackShown` is true (`main.qml`), so a
//! count on a row unanswered past 120 s (no longer "actively" pending)
//! still hides when its hour ends. It only adds a label: no row, banner or
//! count depends on it.

use std::time::Instant;

use super::*;
use crate::bridge_runtime::HeldBackNotices;
use crate::notifier::{FlowKey, HeldBack};

/// Row id to count, for the newest pending row (greatest `started_at_ms`,
/// the later row in the store on a tie) of each flow with notices held
/// back at `now`. `session` is the model's: rows of no session key as 0,
/// which no bridge session is.
pub(super) fn held_back_by_row(
    rows: &[ConnectionRow],
    session: Option<u64>,
    notices: &HeldBackNotices,
    now: Instant,
) -> HashMap<String, u32> {
    let session = session.unwrap_or(0);
    let mut newest: HashMap<FlowKey, &ConnectionRow> = HashMap::new();
    for row in rows.iter().filter(|row| is_pending(row)) {
        let flow = FlowKey::of(
            session,
            row.process_path.as_deref(),
            &row.process,
            &row.dst_host,
        );
        newest
            .entry(flow)
            .and_modify(|seen| {
                if row.started_at_ms >= seen.started_at_ms {
                    *seen = row;
                }
            })
            .or_insert(row);
    }
    newest
        .into_iter()
        .filter_map(|(flow, row)| {
            let count = notices.count(&flow, now);
            (count > 0).then(|| (row.id.clone(), count))
        })
        .collect()
}

impl qobject::ConnectionsModel {
    pub(super) fn retried_role(&self, row: i32) -> QVariant {
        let id = self.row_id_at(row).to_string();
        let count = self.held_back_rows.get(&id).copied().unwrap_or(0);
        QVariant::from(&(count.min(i32::MAX as u32) as i32))
    }

    /// Recompute the role from the held-back counts, repainting only that
    /// role and only when a count moved. The bridge's counts always win: the
    /// test seam's are read only when no bridge runs.
    pub(super) fn refresh_held_back(mut self: Pin<&mut Self>) {
        let notices = crate::bridge_runtime::handles()
            .map(|handles| handles.held_back_notices().clone())
            .or_else(|| self.held_back_for_tests.clone());
        let counts = notices.map_or_else(HashMap::new, |notices| {
            held_back_by_row(
                self.store.rows(),
                self.source_session,
                &notices,
                Instant::now(),
            )
        });
        self.as_mut().set_held_back_shown(!counts.is_empty());
        if counts == self.held_back_rows {
            return;
        }
        self.as_mut().rust_mut().held_back_rows = counts;
        let len = self.row_count(&QModelIndex::default());
        if len == 0 {
            return;
        }
        let no_parent = QModelIndex::default();
        let (top, bottom) = (
            self.index(0, 0, &no_parent),
            self.index(len - 1, 0, &no_parent),
        );
        let mut roles = QList::<i32>::default();
        roles.append(ROLE_RETRIED_COUNT);
        self.data_changed(&top, &bottom, &roles);
    }

    /// Test seam: the probes have no bridge, so no notice gate. Sets the
    /// held-back count of the model session's flow `program` to `host` for
    /// `window_secs`. Read only while no bridge runs, so it can never stand
    /// in for, or hide, a running bridge's counts.
    pub(super) fn hold_back_notices_for_tests(
        mut self: Pin<&mut Self>,
        program: &QString,
        host: &QString,
        count: i32,
        window_secs: i32,
    ) {
        let notices = self.held_back_for_tests.clone().unwrap_or_default();
        let flow = FlowKey {
            session: self.source_session.unwrap_or(0),
            program: program.to_string(),
            host: host.to_string(),
        };
        let now = Instant::now();
        let secs = u64::try_from(window_secs).unwrap_or(0);
        let held = HeldBack {
            count: u32::try_from(count).unwrap_or(0),
            until: now + std::time::Duration::from_secs(secs),
            epoch: 0,
        };
        notices.record(&flow, Some(held), now);
        self.as_mut().rust_mut().held_back_for_tests = Some(notices);
        self.refresh_held_back();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ask(id: &str, path: Option<&str>, host: &str, started_at_ms: i64) -> ConnectionRow {
        ConnectionRow {
            id: id.to_string(),
            process: "curl".into(),
            process_path: path.map(str::to_string),
            dst_host: host.into(),
            dst_ip: "93.184.216.34".into(),
            dst_port: 443,
            protocol: "tcp".into(),
            direction: "outgoing".into(),
            action: None,
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms,
            matched_rule: None,
            auto_answer: None,
            answer_deadline_ms: None,
            deferred: false,
            decided_by_default: false,
        }
    }

    fn held(flow: &FlowKey, count: u32, now: Instant) -> HeldBackNotices {
        let notices = HeldBackNotices::default();
        let until = now + Duration::from_secs(3600);
        notices.record(
            flow,
            Some(HeldBack {
                count,
                until,
                epoch: 0,
            }),
            now,
        );
        notices
    }

    #[test]
    fn only_the_newest_pending_row_of_the_flow_carries_the_count() {
        let now = Instant::now();
        let path = Some("/usr/bin/curl");
        let mut decided = ask("7:4", path, "example.com", 4_000);
        decided.action = Some("deny".into());
        let rows = [
            ask("7:1", path, "example.com", 1_000),
            ask("7:2", path, "example.com", 2_000),
            ask("7:3", path, "example.org", 3_000),
            decided,
        ];
        let notices = held(&FlowKey::of(7, path, "curl", "example.com"), 3, now);
        let counts = held_back_by_row(&rows, Some(7), &notices, now);
        assert_eq!(counts, HashMap::from([("7:2".to_string(), 3)]));
    }

    /// Both shapes `PendingTarget::flow_key` builds: with a path and without.
    #[test]
    fn a_row_without_a_path_is_keyed_by_its_name() {
        let now = Instant::now();
        let rows = [ask("7:1", None, "example.com", 1_000)];
        let by_name = held(&FlowKey::of(7, None, "curl", "example.com"), 2, now);
        assert_eq!(
            held_back_by_row(&rows, Some(7), &by_name, now),
            HashMap::from([("7:1".to_string(), 2)])
        );
        let by_path = held(&FlowKey::of(7, Some(""), "curl", "example.com"), 2, now);
        assert!(held_back_by_row(&rows, Some(7), &by_path, now).is_empty());
    }

    #[test]
    fn another_session_or_an_ended_window_carries_nothing() {
        let now = Instant::now();
        let path = Some("/usr/bin/curl");
        let rows = [ask("7:1", path, "example.com", 1_000)];
        let notices = held(&FlowKey::of(7, path, "curl", "example.com"), 3, now);
        assert!(held_back_by_row(&rows, Some(8), &notices, now).is_empty());
        assert!(held_back_by_row(&rows, None, &notices, now).is_empty());
        let past = now + Duration::from_secs(3600);
        assert!(held_back_by_row(&rows, Some(7), &notices, past).is_empty());
    }
}

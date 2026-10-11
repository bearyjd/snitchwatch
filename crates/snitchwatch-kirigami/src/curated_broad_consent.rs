//! One deliberate DNS intent, tied to the authenticated session that offered it.
//! Tickets and confirmations are consumed once; no retry or reconnect replay.
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const CAPABILITY: &str = "curatedBroadOptInV1";
pub const DNS_ID: &str = "dns-resolved";
pub const DISCLOSURE: &str = "/usr/lib/systemd/systemd-resolved (UID 193) may send DNS over TCP/UDP port 53, using IPv4/IPv6, to any destination. Apps can send data inside DNS lookups. Keeping an enabled rule records this choice without rewriting it. Keeping a disabled rule leaves it disabled; re-enabling requires a separate confirmation.";

#[derive(Clone, PartialEq)]
struct Intent {
    session: u64,
    request_id: String,
    purpose: String,
    ticket: Option<String>,
    expires: Instant,
}

impl std::fmt::Debug for Intent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Intent")
            .field("session", &self.session)
            .field("request_id", &self.request_id)
            .field("purpose", &self.purpose)
            .field("ticket_present", &self.ticket.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BroadConsent {
    session: u64,
    capable: bool,
    sequence: u64,
    pending: Option<Intent>,
    submitted: Option<(u64, String)>,
    result: String,
}

impl BroadConsent {
    pub fn authenticate(&mut self, session: u64, capable: bool) {
        if self.session != session || !capable {
            self.pending = None;
            self.submitted = None;
            self.result.clear();
        }
        self.session = session;
        self.capable = capable;
    }

    pub fn disconnect(&mut self) {
        self.capable = false;
        self.pending = None;
        self.submitted = None;
        self.result.clear();
    }

    pub fn supported(&self) -> bool {
        self.capable
    }
    pub fn open(&self) -> bool {
        self.pending.is_some()
    }
    pub fn ready(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| p.ticket.is_some() && Instant::now() < p.expires)
    }
    pub fn result(&self) -> &str {
        &self.result
    }

    pub fn begin(&mut self, purpose: &str) -> Option<Value> {
        if !self.capable || !matches!(purpose, "enable" | "reenable" | "keep") {
            return None;
        }
        self.sequence = self.sequence.checked_add(1)?;
        let request_id = format!("dns-{}-{}", self.session, self.sequence);
        self.pending = Some(Intent {
            session: self.session,
            request_id: request_id.clone(),
            purpose: purpose.into(),
            ticket: None,
            expires: Instant::now() + Duration::from_secs(120),
        });
        self.result.clear();
        self.submitted = None;
        Some(
            json!({"action":"prepareCuratedBroadOptIn", "requestId":request_id, "id":DNS_ID, "purpose":purpose}),
        )
    }

    pub fn prepared(&mut self, session: u64, msg: &Value) -> bool {
        let Some(p) = self.pending.as_mut() else {
            return false;
        };
        if !self.capable
            || session != p.session
            || session != self.session
            || Instant::now() >= p.expires
            || p.ticket.is_some()
            || msg["requestId"] != p.request_id
            || msg["id"] != DNS_ID
            || msg["purpose"] != p.purpose
        {
            return false;
        }
        let Some(ticket) = msg["ticket"].as_str().filter(|t| {
            t.len() == 64
                && t.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) else {
            return false;
        };
        p.ticket = Some(ticket.into());
        true
    }

    /// Consume before queue admission. A full queue cannot retain consent for retry.
    pub fn confirm(&mut self) -> Option<(u64, Value)> {
        let p = self.pending.take()?;
        if !self.capable || p.session != self.session || Instant::now() >= p.expires {
            return None;
        }
        let ticket = p.ticket?;
        self.submitted = Some((p.session, p.request_id.clone()));
        let action = if p.purpose == "keep" {
            "keepCuratedBroadDefault"
        } else {
            "enableCuratedBroadDefault"
        };
        Some((
            p.session,
            json!({"action":action,"requestId":p.request_id,"id":DNS_ID,"purpose":p.purpose,"ticket":ticket,"consent":"resolver-any-address-v1"}),
        ))
    }

    pub fn cancel(&mut self) -> Option<(u64, Value)> {
        let p = self.pending.take()?;
        Some((
            p.session,
            json!({"action":"cancelCuratedBroadOptIn","requestId":p.request_id,"id":DNS_ID}),
        ))
    }

    pub fn revoke_local(&mut self) {
        self.pending = None;
        self.submitted = None;
        self.result.clear();
    }

    pub fn failed(&mut self, text: &str) {
        self.pending = None;
        self.submitted = None;
        self.result = text.into();
    }

    pub fn receive_result(&mut self, session: u64, msg: &Value) {
        let request_id = msg["requestId"].as_str();
        let matching = self
            .pending
            .as_ref()
            .is_some_and(|p| p.session == session && Some(p.request_id.as_str()) == request_id)
            || self
                .submitted
                .as_ref()
                .is_some_and(|(s, id)| *s == session && Some(id.as_str()) == request_id);
        if !self.capable || self.session != session || !matching {
            return;
        }
        let Some(result) = msg["results"]
            .as_array()
            .and_then(|r| r.iter().find(|r| r["id"] == DNS_ID))
        else {
            return;
        };
        self.pending = None;
        self.submitted = None;
        self.result = if result["outcome"] == "choiceAccepted" {
            if result["persisted"] == true {
                "DNS choice saved. Installation status follows the firewall's rule list."
            } else {
                "DNS choice accepted for this session; it was not saved. Installation status follows the firewall's rule list."
            }
        } else {
            "DNS choice refused. Open a fresh confirmation to try again; no enable was confirmed."
        }.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn prepared(p: &Value) -> Value {
        let mut result = p.clone();
        result["action"] = json!("curatedBroadOptInPrepared");
        result["ticket"] = json!("a".repeat(64));
        result
    }
    #[test]
    fn old_bridge_and_reconnect_never_replay_consent() {
        let mut state = BroadConsent::default();
        assert!(state.begin("enable").is_none());
        state.authenticate(1, true);
        let p = state.begin("enable").unwrap();
        state.authenticate(2, true);
        assert!(!state.prepared(1, &prepared(&p)));
        assert!(state.confirm().is_none());
        state.disconnect();
        assert!(!state.open());
        assert!(!state.supported());
    }
    #[test]
    fn purpose_ticket_cancel_and_expiry_are_bound_and_single_use() {
        let mut state = BroadConsent::default();
        state.authenticate(7, true);
        let p = state.begin("keep").unwrap();
        let mut wrong = prepared(&p);
        wrong["purpose"] = json!("enable");
        assert!(!state.prepared(7, &wrong));
        assert!(!state.prepared(8, &prepared(&p)));
        assert!(state.prepared(7, &prepared(&p)));
        assert!(!state.prepared(7, &prepared(&p)));
        assert_eq!(
            state.confirm().unwrap().1["action"],
            "keepCuratedBroadDefault"
        );
        assert!(state.confirm().is_none());
        let p = state.begin("reenable").unwrap();
        assert!(state.cancel().is_some());
        assert!(!state.prepared(7, &prepared(&p)));
        let p = state.begin("enable").unwrap();
        state.pending.as_mut().unwrap().expires = Instant::now();
        assert!(!state.prepared(7, &prepared(&p)));
        assert!(state.confirm().is_none());
    }
    #[test]
    fn replacement_and_queue_failure_require_a_fresh_deliberate_action() {
        let mut state = BroadConsent::default();
        state.authenticate(1, true);
        let old = state.begin("enable").unwrap();
        let new = state.begin("reenable").unwrap();
        assert!(!state.prepared(1, &prepared(&old)));
        assert!(state.prepared(1, &prepared(&new)));
        assert_eq!(state.confirm().unwrap().1["purpose"], "reenable");
        state.failed("Request queue full. Open the confirmation again.");
        assert!(!state.open());
        assert!(state.confirm().is_none());
    }

    #[test]
    fn results_require_matching_session_and_request_and_do_not_claim_installation() {
        let mut state = BroadConsent::default();
        state.authenticate(4, true);
        let request = state.begin("enable").unwrap();
        state.prepared(4, &prepared(&request));
        state.confirm().unwrap();
        let result = json!({"requestId":request["requestId"],"results":[{"id":DNS_ID,"outcome":"choiceAccepted","persisted":true}]});
        state.receive_result(3, &result);
        assert!(state.result().is_empty());
        let mut wrong = result.clone();
        wrong["requestId"] = json!("different");
        state.receive_result(4, &wrong);
        assert!(state.result().is_empty());
        state.receive_result(4, &result);
        assert!(state.result().contains("choice saved"));
        assert!(state.result().contains("Installation status follows"));
        state.revoke_local();
        state.receive_result(4, &result);
        assert!(
            state.result().is_empty(),
            "late accepted reply overrode local Off"
        );
    }
}

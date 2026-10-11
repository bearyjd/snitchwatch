//! Modern broad choices and private results. State is locked before the
//! connection lease, and a final daemon send retains the state guard.
use super::*;
use crate::curated::intent::{IntentContext, IntentError};
use crate::daemon_commands::{PendingReply, SendError};
use crate::ws_messages::{
    valid_request_id, CuratedIntentPurpose as Purpose, CuratedRequestOutcome as Outcome,
    CuratedRequestResult, ReplyTo,
};

pub(super) const CONSENT_REQUIRED: &str = "This DNS rule allows the system resolver to send DNS to any address. Confirm it on the Recommended page before turning it on.";

pub(super) struct PendingResult {
    reply: ReplyTo,
    request_id: Option<String>,
    results: Vec<CuratedRequestResult>,
    version: u64,
    authority_revision: Option<u64>,
    grant: Option<store::BroadGrant>,
}

fn result(id: &str, outcome: Outcome, reason: &str) -> CuratedRequestResult {
    CuratedRequestResult {
        id: id.to_owned(),
        outcome,
        reason: reason.to_owned(),
        choice_accepted: None,
        persisted: None,
    }
}
fn error_outcome(error: IntentError) -> Outcome {
    match error {
        IntentError::InvalidIntent => Outcome::InvalidIntent,
        IntentError::StaleIntent => Outcome::StaleIntent,
        IntentError::Unavailable => Outcome::Unavailable,
    }
}
fn private(
    reply: Option<&ReplyTo>,
    request_id: Option<String>,
    results: Vec<CuratedRequestResult>,
) {
    if let Some(reply) = reply {
        // A full private queue never falls back to a broadcast.
        reply.try_send(ServerMessage::CuratedDefaultsRequestResult {
            request_id,
            results,
        });
    }
}

impl CuratedDefaults {
    pub(super) fn route_intent(&self, message: ClientMessage) -> Option<ClientMessage> {
        match message {
            ClientMessage::SetCuratedDefaults {
                ids,
                on,
                request_id,
                reply,
            } => self.choose_legacy(&ids, on, request_id, reply),
            ClientMessage::RemoveCuratedDefault { id } => self.ask_removal(&id),
            ClientMessage::PrepareCuratedBroadOptIn {
                request_id,
                id,
                purpose,
                context,
                reply,
            } => self.prepare_intent(request_id, id, purpose, context, reply),
            ClientMessage::CancelCuratedBroadOptIn {
                request_id,
                id,
                context,
                reply,
            } => {
                let state = lock(&self.inner.state);
                let outcome = match context.as_ref().map(IntentContext::admit) {
                    Some(Ok(mut admission))
                        if valid_request_id(&request_id) && id == "dns-resolved" =>
                    {
                        admission.cancel(&request_id, &id);
                        Outcome::ChoiceAccepted
                    }
                    Some(Ok(mut admission)) => {
                        admission.invalidate();
                        Outcome::InvalidIntent
                    }
                    Some(Err(error)) => error_outcome(error),
                    None => Outcome::InvalidIntent,
                };
                drop(state);
                private(
                    reply.as_ref(),
                    Some(request_id),
                    vec![result(
                        &id,
                        outcome,
                        "Intent cancelled; no rule was enabled.",
                    )],
                );
            }
            ClientMessage::EnableCuratedBroadDefault {
                request_id,
                id,
                ticket,
                consent,
                purpose,
                context,
                reply,
            } => self.accept_intent(
                request_id, id, ticket, consent, purpose, false, context, reply,
            ),
            ClientMessage::KeepCuratedBroadDefault {
                request_id,
                id,
                ticket,
                consent,
                purpose,
                context,
                reply,
            } => self.accept_intent(
                request_id, id, ticket, consent, purpose, true, context, reply,
            ),
            other => return Some(other),
        }
        None
    }

    fn choose_legacy(
        &self,
        ids: &[String],
        on: bool,
        request_id: Option<String>,
        reply: Option<ReplyTo>,
    ) {
        let mut state = lock(&self.inner.state);
        if reply.is_some() && state.request_results.len() >= 256 {
            private(
                reply.as_ref(),
                request_id,
                vec![result("", Outcome::Unavailable, "Request queue is full.")],
            );
            return;
        }
        let mut results = Vec::new();
        let mut seen = BTreeSet::new();
        if ids.len() > 256 || ids.iter().any(|id| id.len() > 64) {
            private(
                reply.as_ref(),
                request_id,
                vec![result("", Outcome::InvalidIntent, "Too many entries.")],
            );
            return;
        }
        for id in ids {
            if !seen.insert(id) {
                continue;
            }
            let Some(entry) = entries().iter().find(|entry| &entry.id == id) else {
                results.push(result(id, Outcome::UnknownId, "Unknown recommended rule."));
                continue;
            };
            // Refusal precedes choices, resets, saves and adoption, even
            // when the only requested ID is DNS or a saved grant exists.
            if on && entry.broad() {
                state.consent_problems.insert(id.clone(), CONSENT_REQUIRED);
                results.push(result(id, Outcome::RefusedConsent, CONSENT_REQUIRED));
                continue;
            }
            if state.inert.is_some() {
                results.push(result(
                    id,
                    Outcome::Unavailable,
                    "Recommended rules are unavailable.",
                ));
                continue;
            }
            if !on {
                self.revoke_locked(&mut state, id);
            }
            let choices = if on {
                state.choices.enable(id)
            } else {
                state.choices.disable(id)
            };
            keep(&mut state, choices);
            state.requests += 1;
            state.failures.remove(id);
            state.consent_problems.remove(id);
            let mut accepted = result(
                id,
                Outcome::ChoiceAccepted,
                "Choice accepted; installation is reported separately.",
            );
            accepted.choice_accepted = Some(true);
            results.push(accepted);
        }
        let version = state.version;
        if let Some(reply) = reply {
            if state.request_results.len() < 256 {
                state.request_results.push(PendingResult {
                    reply,
                    request_id: request_id.filter(|id| valid_request_id(id)),
                    results,
                    version,
                    authority_revision: None,
                    grant: None,
                });
            } else {
                private(
                    Some(&reply),
                    request_id,
                    vec![result("", Outcome::Unavailable, "Request queue is full.")],
                );
            }
        }
        drop(state);
        self.inner.wake.notify_one();
        self.announce_changed();
    }

    fn prepare_intent(
        &self,
        request_id: String,
        id: String,
        purpose: Purpose,
        context: Option<IntentContext>,
        reply: Option<ReplyTo>,
    ) {
        let state = lock(&self.inner.state);
        let prepared = (|| {
            let context = context.as_ref().ok_or(IntentError::InvalidIntent)?;
            let mut admission = context.admit()?;
            admission.invalidate();
            if !valid_request_id(&request_id) || id != "dns-resolved" {
                return Err(IntentError::InvalidIntent);
            }
            if state.inert.is_some() || state.file.is_none() || !state.storage.persistent {
                return Err(IntentError::Unavailable);
            }
            let entry = entries()
                .iter()
                .find(|entry| entry.id == id && entry.broad())
                .ok_or(IntentError::InvalidIntent)?;
            admission.prepare(
                &request_id,
                &id,
                purpose,
                &store::broad_policy_fingerprint(entry),
                state.broad_revision,
            )
        })();
        drop(state);
        match prepared {
            Ok(ticket) => {
                if let Some(reply) = reply {
                    reply.try_send(ServerMessage::CuratedBroadOptInPrepared {
                        request_id,
                        id,
                        purpose,
                        ticket,
                    });
                }
            }
            Err(error) => private(
                reply.as_ref(),
                Some(request_id),
                vec![result(
                    &id,
                    error_outcome(error),
                    "The DNS intent could not be prepared.",
                )],
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn accept_intent(
        &self,
        request_id: String,
        id: String,
        ticket: String,
        consent: String,
        purpose: Purpose,
        keeping: bool,
        context: Option<IntentContext>,
        reply: Option<ReplyTo>,
    ) {
        let mut state = lock(&self.inner.state);
        let accepted = (|| {
            let context = context.as_ref().ok_or(IntentError::InvalidIntent)?;
            let mut admission = context.admit()?;
            if !valid_request_id(&request_id)
                || id != "dns-resolved"
                || keeping != (purpose == Purpose::Keep)
            {
                admission.invalidate();
                return Err(IntentError::InvalidIntent);
            }
            let entry = entries()
                .iter()
                .find(|entry| entry.id == id && entry.broad())
                .ok_or(IntentError::InvalidIntent)?;
            admission.consume(
                &request_id,
                &ticket,
                &id,
                purpose,
                &store::broad_policy_fingerprint(entry),
                state.broad_revision,
                &consent,
            )?;
            if state.inert.is_some()
                || state.file.is_none()
                || !state.storage.persistent
                || (reply.is_some() && state.request_results.len() >= 256)
            {
                return Err(IntentError::Unavailable);
            }
            let cache = self.inner.rules.lock().unwrap_or_else(|e| e.into_inner());
            let rules = cache.rules().ok_or(IntentError::Unavailable)?;
            let name = entry.rule_name();
            if cache.left_out().contains_key(&name)
                || cache.files_left().contains(&name)
                || (state.choices.deleted_by_user.contains(&id)
                    && !(purpose == Purpose::Enable && state.choices.disabled.contains(&id)))
            {
                return Err(IntentError::StaleIntent);
            }
            let current = rules.get(&name);
            if current.is_some_and(|rule| !is_unedited(Some(entry), None, rule)) {
                return Err(IntentError::InvalidIntent);
            }
            let activation = match purpose {
                Purpose::Keep => {
                    if current.is_none() {
                        return Err(IntentError::StaleIntent);
                    }
                    current.is_some_and(|rule| rule.enabled)
                }
                Purpose::Enable => {
                    if current.is_some_and(|rule| !rule.enabled) {
                        return Err(IntentError::InvalidIntent);
                    }
                    true
                }
                Purpose::Reenable => {
                    if current.is_none_or(|rule| rule.enabled) {
                        return Err(IntentError::StaleIntent);
                    }
                    true
                }
            };
            let mut choices = state.choices.enable(&id).grant_broad(&id, activation);
            if let Some(rule) = current {
                choices = choices.installed(&id, rule);
            }
            keep(&mut state, choices);
            state.requests += 1;
            state.failures.remove(&id);
            state.consent_problems.remove(&id);
            if purpose == Purpose::Reenable {
                let revision = state.broad_revision;
                state.reenables.insert(id.clone(), revision);
            }
            // Admission commits before dropping the lease. Durable accepted
            // choices may reconcile after disconnect; the ticket never does.
            Ok(())
        })();
        match accepted {
            Ok(()) => {
                let mut accepted = result(
                    &id,
                    Outcome::ChoiceAccepted,
                    "Choice accepted; installation is reported separately.",
                );
                accepted.choice_accepted = Some(true);
                if let Some(reply) = reply {
                    let version = state.version;
                    let revision = state.broad_revision;
                    let grant = state.choices.broad_grants.get(&id).cloned();
                    state.request_results.push(PendingResult {
                        reply,
                        request_id: Some(request_id),
                        results: vec![accepted],
                        version,
                        authority_revision: Some(revision),
                        grant,
                    });
                }
                drop(state);
                self.inner.wake.notify_one();
                self.announce_changed();
            }
            Err(error) => {
                drop(state);
                private(
                    reply.as_ref(),
                    Some(request_id),
                    vec![result(
                        &id,
                        error_outcome(error),
                        "The DNS intent was refused; the firewall was left unchanged.",
                    )],
                );
            }
        }
    }

    pub(super) fn revoke_locked(&self, state: &mut State, id: &str) {
        if entries()
            .iter()
            .any(|entry| entry.id == id && entry.broad())
        {
            state.broad_revision = state.broad_revision.wrapping_add(1);
            state.reenables.remove(id);
            state.consent_problems.remove(id);
            let choices = state.choices.revoke_broad(id);
            keep(state, choices);
        }
    }

    /// Rules-page off revokes authority atomically with queue admission.
    /// Generic broad on is always refused, including when a saved grant exists.
    pub fn send_managed_toggle(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<PendingReply, SendError> {
        let mut state = lock(&self.inner.state);
        if state.inert.is_some() {
            return Err(SendError::ReservedName);
        }
        if let Some(entry) = super::super::entry_for_rule_name(name) {
            if entry.broad() {
                if enabled {
                    return Err(SendError::ReservedName);
                }
                self.revoke_locked(&mut state, &entry.id);
                state.requests += 1;
                self.inner.wake.notify_one();
            }
        }
        self.inner.commands.send_curated_toggle(name, enabled)
    }

    pub(super) fn flush_request_results(&self) {
        let mut state = lock(&self.inner.state);
        let pending = std::mem::take(&mut state.request_results);
        for mut request in pending {
            for result in &mut request.results {
                if result.choice_accepted == Some(true) {
                    let authority_current = request.authority_revision.is_none_or(|revision| {
                        revision == state.broad_revision
                            && state.choices.broad_grants.get(&result.id) == request.grant.as_ref()
                    });
                    let persisted = state.storage.persistent
                        && state.inert.is_none()
                        && state.saved >= request.version
                        && authority_current;
                    result.persisted = Some(persisted);
                    if !persisted {
                        result.reason = "Choice accepted in memory, but saved authority is unavailable or revoked; no activation is authorized.".into();
                    }
                }
            }
            private(Some(&request.reply), request.request_id, request.results);
        }
    }

    pub(super) async fn reenable(&self, id: &str, generation: u64, revision: u64) {
        let sent = {
            let state = lock(&self.inner.state);
            if state.inert.is_some()
                || state.saved < state.version
                || !state.storage.persistent
                || state.broad_revision != revision
                || !state.choices.has_broad_authority(id)
                || self.generation() != generation
            {
                return;
            }
            let Some(entry) = entries().iter().find(|entry| entry.id == id) else {
                return;
            };
            let cache = self.inner.rules.lock().unwrap_or_else(|e| e.into_inner());
            if cache.left_out().contains_key(&entry.rule_name())
                || cache.files_left().contains(&entry.rule_name())
            {
                return;
            }
            let Some(current) = cache
                .rules()
                .and_then(|rules| rules.get(&entry.rule_name()))
            else {
                return;
            };
            if current.enabled {
                return;
            }
            let Some(command) = CuratedCommand::authorized_toggle(current) else {
                return;
            };
            drop(cache);
            self.inner
                .commands
                .send_curated_guarded(command, generation, true)
        };
        let outcome = Self::wait_sent(sent, Refusal::Add).await;
        let mut state = lock(&self.inner.state);
        match outcome {
            Ok(()) => {
                state.statuses.insert(id.to_owned(), EntryStatus::Installed);
            }
            Err(problem) => fail(
                &mut state,
                false,
                id,
                EntryStatus::InstalledButOff,
                problem,
                generation,
            ),
        }
    }
}

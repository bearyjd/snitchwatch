//! The sequencer: one `AddRule` at a time, each answered before the next.

use std::time::Duration;

use snitchwatch_bridge::ws_messages::{RuleCommandOutcome, ServerMessage};

use super::compare::same_content;
use super::plan::BulkPlan;
use super::progress::{AbortReason, BatchState, NotSentWhy, Progress, RowStatus};
use super::sink::{Incoming, RuleSink, SendError, Settings};
use super::snapshot::{ListView, Listing};

/// Why a batch ends early.
enum Stop {
    Abort(AbortReason),
    Cancel,
}

enum Awaited {
    Outcome(RuleCommandOutcome),
    /// The feed dropped messages while waiting.
    Lagged,
}

/// Run `plan` against `sink`. `observer` sees every new [`Progress`]; the
/// last one is also returned. Blocks until the batch ends, so call it off
/// the UI thread.
pub fn run(
    plan: &BulkPlan,
    sink: &mut dyn RuleSink,
    settings: &Settings,
    observer: &mut dyn FnMut(&Progress),
) -> Progress {
    let mut runner = Runner {
        plan,
        sink,
        settings,
        observer,
        view: ListView::new(),
        progress: Progress::new(plan),
        timeouts: 0,
    };
    runner.notify();
    let ended = runner.batch();
    runner.finish(ended)
}

struct Runner<'a> {
    plan: &'a BulkPlan,
    sink: &'a mut dyn RuleSink,
    settings: &'a Settings,
    observer: &'a mut dyn FnMut(&Progress),
    view: ListView,
    progress: Progress,
    /// Timeouts in a row, up to this rule.
    timeouts: u32,
}

impl Runner<'_> {
    fn notify(&mut self) {
        (self.observer)(&self.progress);
    }

    fn set(&mut self, index: usize, status: RowStatus) {
        self.progress = self.progress.with_status(index, status);
        self.notify();
    }

    fn finish(&mut self, ended: Result<(), Stop>) -> Progress {
        let (why, state) = match ended {
            Ok(()) => (NotSentWhy::Stopped, BatchState::Finished),
            Err(Stop::Cancel) => (NotSentWhy::Cancelled, BatchState::Cancelled),
            Err(Stop::Abort(reason)) => (NotSentWhy::Stopped, BatchState::Aborted(reason)),
        };
        self.progress = self.progress.with_rest_not_sent(0, why).with_state(state);
        self.notify();
        self.progress.clone()
    }

    fn check_cancel(&self) -> Result<(), Stop> {
        match self.sink.cancelled() {
            true => Err(Stop::Cancel),
            false => Ok(()),
        }
    }

    fn batch(&mut self) -> Result<(), Stop> {
        self.check_cancel()?;
        for index in 0..self.plan.rules().len() {
            self.check_cancel()?;
            self.one(index)?;
        }
        Ok(())
    }

    /// Read what the bridge has queued, without waiting. A lag means the
    /// list can no longer be trusted.
    fn drain(&mut self) -> Result<(), Stop> {
        loop {
            match self.sink.next_message(Duration::ZERO) {
                Ok(Incoming::Message(message)) => self.view.absorb(&message),
                Ok(Incoming::Lagged) => self.view.expect_fresh(),
                Ok(Incoming::Idle) => return Ok(()),
                Err(_) => return Err(Stop::Abort(AbortReason::SessionLost)),
            }
        }
    }

    /// Before ANY look-up or send: the list in hand is a read one, as of
    /// now. Otherwise ask again (bounded), and stop if it still is not.
    fn ensure_list(&mut self) -> Result<(), Stop> {
        self.drain()?;
        if self.view.listing() == Listing::Ready {
            return Ok(());
        }
        match self.fetch_list()? {
            Listing::Ready => Ok(()),
            Listing::Unavailable(why) => Err(Stop::Abort(AbortReason::ListUnavailable(why))),
            Listing::Waiting => Err(Stop::Abort(AbortReason::ListUnavailable(
                self.view.why_waiting(),
            ))),
        }
    }

    /// Look the rule's name up in a read list. `Ok(true)` means it is
    /// free to send; otherwise the row has been settled.
    fn decide(&mut self, index: usize) -> Result<bool, Stop> {
        self.ensure_list()?;
        let planned = &self.plan.rules()[index];
        let settled = match self.view.find(planned.name()) {
            Some(existing) if same_content(planned, existing) => RowStatus::AlreadyPresent,
            Some(_) => RowStatus::ChangedByOwner,
            None if self.view.left_out() => RowStatus::NotChecked,
            None => return Ok(true),
        };
        self.set(index, settled);
        Ok(false)
    }

    /// Queue the `AddRule`. A full queue is retried after a growing wait,
    /// and each retry first looks the name up again in a fresh list: the
    /// waits are long enough for the list to change.
    fn queue_add(&mut self, index: usize, id: &str) -> Result<bool, Stop> {
        let mut wait = self.settings.backoff_start;
        for attempt in 1..=self.settings.send_attempts {
            if !self.decide(index)? {
                return Ok(false);
            }
            self.check_cancel()?;
            self.set(index, RowStatus::Sending);
            let planned = &self.plan.rules()[index];
            match self.sink.send_add(id, &planned.payload) {
                Ok(()) => return Ok(true),
                Err(SendError::NotConnected) => return Err(Stop::Abort(AbortReason::NotConnected)),
                Err(SendError::Full) if attempt < self.settings.send_attempts => {
                    self.sink.sleep(wait);
                    wait = (wait * 2).min(self.settings.backoff_max);
                }
                Err(SendError::Full) => {}
            }
        }
        Err(Stop::Abort(AbortReason::QueueBusy))
    }

    fn one(&mut self, index: usize) -> Result<(), Stop> {
        let id = self.sink.new_request_id();
        if !self.queue_add(index, &id)? {
            return Ok(());
        }
        match self.await_result(index, &id)? {
            Awaited::Outcome(outcome) => self.settle(index, outcome),
            // A lag is not a timeout: settle by content now, and it ends a
            // run of timeouts (the bridge's answer was lost, not missing).
            Awaited::Lagged => {
                self.timeouts = 0;
                self.reconcile(index)
            }
        }
    }

    fn settle(&mut self, index: usize, outcome: RuleCommandOutcome) -> Result<(), Stop> {
        let timed_out = matches!(outcome, RuleCommandOutcome::Timeout);
        self.timeouts = if timed_out { self.timeouts + 1 } else { 0 };
        match outcome {
            RuleCommandOutcome::Ok => self.set(index, RowStatus::Added),
            RuleCommandOutcome::OkWithNote { note } => {
                self.set(index, RowStatus::AddedWithNote(note))
            }
            RuleCommandOutcome::Rejected { reason } => self.set(index, RowStatus::Rejected(reason)),
            RuleCommandOutcome::Refused { problems } => {
                let lines = problems
                    .iter()
                    .map(|p| format!("{}: {}", p.path, p.reason))
                    .collect();
                self.set(index, RowStatus::Refused(lines))
            }
            RuleCommandOutcome::NoDaemon => {
                self.set(index, RowStatus::NotSent(NotSentWhy::Stopped));
                return Err(Stop::Abort(AbortReason::NoDaemon));
            }
            RuleCommandOutcome::Timeout | RuleCommandOutcome::Unsure { .. } => {
                if timed_out && self.timeouts >= self.settings.max_consecutive_timeouts {
                    self.set(index, RowStatus::NotConfirmed(NO_ANSWER_NOTE.into()));
                    return Err(Stop::Abort(AbortReason::RepeatedTimeouts(self.timeouts)));
                }
                return self.reconcile(index);
            }
        }
        Ok(())
    }

    /// No answer, or an unsure one: ask for the list again and compare
    /// CONTENT. The rule is never sent a second time. If the list cannot
    /// be read the batch stops: nothing more is sent on a list in doubt.
    fn reconcile(&mut self, index: usize) -> Result<(), Stop> {
        let planned = &self.plan.rules()[index];
        let listing = match self.fetch_list() {
            Ok(listing) => listing,
            Err(stop) => {
                let note = match stop {
                    Stop::Cancel => CANCEL_NOTE,
                    Stop::Abort(_) => LOST_NOTE,
                };
                self.set(index, RowStatus::NotConfirmed(note.into()));
                return Err(stop);
            }
        };
        if let Listing::Unavailable(why) = listing {
            self.set(
                index,
                RowStatus::NotConfirmed(
                    "The rules list is not available, so it could not be checked.".into(),
                ),
            );
            return Err(Stop::Abort(AbortReason::ListUnavailable(why)));
        }
        let status = match self.view.find(planned.name()) {
            Some(found) if same_content(planned, found) => RowStatus::ConfirmedAfterNoAnswer,
            Some(_) => RowStatus::DifferentAfterNoAnswer,
            None if self.view.left_out() => {
                RowStatus::NotConfirmed("The rules list leaves some rules out.".into())
            }
            None => RowStatus::NotConfirmed("It is not in the rules list.".into()),
        };
        self.set(index, status);
        Ok(())
    }

    /// Wait for the result of `id`; none in time is a `Timeout`.
    fn await_result(&mut self, index: usize, id: &str) -> Result<Awaited, Stop> {
        let start = self.sink.now();
        loop {
            let left = self.left(start, self.settings.result_wait);
            if left.is_zero() {
                return Ok(Awaited::Outcome(RuleCommandOutcome::Timeout));
            }
            match self.sink.next_message(left.min(self.settings.poll)) {
                Ok(Incoming::Message(ServerMessage::RuleCommandResult {
                    request_id,
                    outcome,
                })) if request_id == id => return Ok(Awaited::Outcome(outcome)),
                Ok(Incoming::Message(other)) => self.view.absorb(&other),
                Ok(Incoming::Lagged) => {
                    self.view.expect_fresh();
                    return Ok(Awaited::Lagged);
                }
                Ok(Incoming::Idle) => {}
                Err(_) => {
                    self.set(index, RowStatus::NotConfirmed(LOST_NOTE.into()));
                    return Err(Stop::Abort(AbortReason::SessionLost));
                }
            }
            if self.sink.cancelled() {
                self.set(index, RowStatus::NotConfirmed(CANCEL_NOTE.into()));
                return Err(Stop::Cancel);
            }
        }
    }

    /// Ask for the list and wait, bounded, for it to be read.
    fn fetch_list(&mut self) -> Result<Listing, Stop> {
        self.view.expect_fresh();
        self.send(|sink| sink.request_snapshot())?;
        let start = self.sink.now();
        loop {
            match self.view.listing() {
                Listing::Waiting => {}
                done => return Ok(done),
            }
            let left = self.left(start, self.settings.list_wait);
            if left.is_zero() {
                return Ok(Listing::Unavailable(self.view.why_waiting()));
            }
            match self.sink.next_message(left.min(self.settings.poll)) {
                Ok(Incoming::Message(message)) => self.view.absorb(&message),
                Ok(Incoming::Lagged) => self.view.expect_fresh(),
                Ok(Incoming::Idle) => {}
                Err(_) => return Err(Stop::Abort(AbortReason::SessionLost)),
            }
            self.check_cancel()?;
        }
    }

    fn left(&self, start: Duration, limit: Duration) -> Duration {
        limit.saturating_sub(self.sink.now().saturating_sub(start))
    }

    /// Queue a message; a full queue is retried with a growing wait.
    fn send(
        &mut self,
        mut queue: impl FnMut(&mut dyn RuleSink) -> Result<(), SendError>,
    ) -> Result<(), Stop> {
        let mut wait = self.settings.backoff_start;
        for attempt in 1..=self.settings.send_attempts {
            match queue(&mut *self.sink) {
                Ok(()) => return Ok(()),
                Err(SendError::NotConnected) => return Err(Stop::Abort(AbortReason::NotConnected)),
                Err(SendError::Full) if attempt < self.settings.send_attempts => {
                    self.sink.sleep(wait);
                    wait = (wait * 2).min(self.settings.backoff_max);
                }
                Err(SendError::Full) => {}
            }
        }
        Err(Stop::Abort(AbortReason::QueueBusy))
    }
}

const NO_ANSWER_NOTE: &str = "It may or may not have been applied.";
const CANCEL_NOTE: &str = "Cancelled while waiting for an answer.";
const LOST_NOTE: &str = "The connection changed before an answer came.";

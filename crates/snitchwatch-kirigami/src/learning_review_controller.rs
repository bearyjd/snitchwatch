//! `LearningReviewController`: the review page's state (learning mode, phase
//! L4b), bound by `LearningReviewPage.qml`. The logic is in
//! `crate::learning_review_session` (no Qt, tested there); this file only
//! moves it onto the Qt thread and back.
//!
//! The rows, the ticks, the notices, the summary and the batch's progress are
//! JSON `QString` properties, parsed in QML the way the scanner page does
//! with its report. The ticks are their own property, so ticking a row does
//! not rebuild the rows (that would reset the list's scroll position).
//!
//! * The constructor does no I/O; `refresh()` reads the saved list and the
//!   rules list on a worker thread.
//! * "Apply" sends the plan frozen in the summary, on its own thread (the
//!   sequencer blocks), never on the Qt or an async runtime thread. One batch
//!   runs at a time in the whole process (`APPLYING`).
//! * Deleting the saved list (Clear, or after a completed apply) is refused
//!   while learning runs or a batch runs.

use core::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;

use crate::bulk_rules::{BridgeSink, Settings};
use crate::learning_review_session::{
    delete_record, kind_from_token, progress_json, read_rules, run_batch_guarded, ReviewSession,
};
use crate::rule_commands::NOT_CONNECTED;

/// How long the rules list may take to arrive before the page goes without.
const RULES_WAIT: Duration = Duration::from_secs(4);

/// Whether a batch is running anywhere in this process.
static APPLYING: AtomicBool = AtomicBool::new(false);

/// Whether rules are being applied now (Start learning is refused then).
pub fn applying_now() -> bool {
    APPLYING.load(Ordering::SeqCst)
}

/// How long deleting the saved list may take before it is called unconfirmed.
const DELETE_WAIT: Duration = Duration::from_secs(3);

/// The real deletion of the saved list: the writer's confirmed result.
fn clear_saved_list() -> Result<(), String> {
    crate::learned::clear_and_wait(DELETE_WAIT).map_err(|_| {
        "The recorded list could not be deleted (or the deletion was not confirmed).".to_string()
    })
}

/// The deletion a finished batch does for itself. It runs INSIDE the batch,
/// while `APPLYING` is still set (that is the batch), so applying is not
/// checked here; learning is, under the settings gate.
fn batch_delete(
    learning: &dyn Fn() -> bool,
    clear: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), String> {
    delete_record(
        &crate::settings_controller::SETTINGS_WRITE,
        learning,
        &|| false,
        clear,
    )
}

/// The deletion the owner asked for ("Delete the recorded list"): refused
/// while a batch runs.
fn delete_saved_list() -> Result<(), String> {
    delete_record(
        &crate::settings_controller::SETTINGS_WRITE,
        &crate::learned::is_active,
        &applying_now,
        &mut clear_saved_list,
    )
}

/// Releases [`APPLYING`] when dropped, however the batch ends.
struct Batch;

impl Batch {
    fn begin() -> Option<Self> {
        APPLYING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        APPLYING.store(false, Ordering::SeqCst);
    }
}

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        /// The program rows as a JSON array.
        #[qproperty(QString, rows_json, cxx_name = "rowsJson")]
        /// `{ "<row key>": "<choice kind>" }`: what is ticked.
        #[qproperty(QString, ticks_json, cxx_name = "ticksJson")]
        /// Facts about the record as a whole, as JSON.
        #[qproperty(QString, notices_json, cxx_name = "noticesJson")]
        /// The prepared summary as JSON, or `null`.
        #[qproperty(QString, summary_json, cxx_name = "summaryJson")]
        /// The batch's progress as JSON, or empty before one runs.
        #[qproperty(QString, progress_json, cxx_name = "progressJson")]
        /// The last refusal or outcome, a fixed sentence.
        #[qproperty(QString, status_text, cxx_name = "statusText")]
        /// A fixed sentence about the saved list's state, or empty.
        #[qproperty(QString, file_notice, cxx_name = "fileNotice")]
        /// How many rules "Apply" would send for what is ticked.
        #[qproperty(i32, tick_count, cxx_name = "tickCount")]
        /// A batch is running.
        #[qproperty(bool, applying)]
        /// The saved list is being read.
        #[qproperty(bool, loading)]
        /// `refresh()` finished at least once.
        #[qproperty(bool, loaded)]
        type LearningReviewController = super::LearningReviewControllerRust;

        /// Read the saved list and the rules list; nothing is ticked after.
        #[qinvokable]
        fn refresh(self: Pin<&mut LearningReviewController>);

        /// Tick `kind` (a token from the rows' choices) on a row, replacing
        /// its other tick; an empty `kind` unticks it.
        #[qinvokable]
        #[cxx_name = "setTick"]
        fn set_tick(self: Pin<&mut LearningReviewController>, key: &QString, kind: &QString);

        /// The explicit "Select suggested" act.
        #[qinvokable]
        #[cxx_name = "selectSuggested"]
        fn select_suggested(self: Pin<&mut LearningReviewController>);

        #[qinvokable]
        #[cxx_name = "untickAll"]
        fn untick_all(self: Pin<&mut LearningReviewController>);

        /// Build the plan and its summary from the ticks (into `summaryJson`).
        #[qinvokable]
        #[cxx_name = "prepareSummary"]
        fn prepare_summary(self: Pin<&mut LearningReviewController>);

        /// Close the summary without applying.
        #[qinvokable]
        #[cxx_name = "discardSummary"]
        fn discard_summary(self: Pin<&mut LearningReviewController>);

        /// Give or take back the summary's extra confirmation `index`.
        #[qinvokable]
        #[cxx_name = "setExtraConfirmed"]
        fn set_extra_confirmed(self: Pin<&mut LearningReviewController>, index: i32, on: bool);

        /// Send the summary's rules, one at a time, on a worker thread.
        #[qinvokable]
        fn apply(self: Pin<&mut LearningReviewController>);

        /// Stop before the next rule.
        #[qinvokable]
        #[cxx_name = "cancelApply"]
        fn cancel_apply(self: Pin<&mut LearningReviewController>);

        /// Delete the saved list. Refused while learning or applying.
        #[qinvokable]
        #[cxx_name = "clearData"]
        fn clear_data(self: Pin<&mut LearningReviewController>);

        /// A row's destinations with counts, busiest first, as a JSON array.
        #[qinvokable]
        fn destinations(self: &LearningReviewController, key: &QString) -> QString;
    }

    impl cxx_qt::Threading for LearningReviewController {}
}

pub struct LearningReviewControllerRust {
    rows_json: QString,
    ticks_json: QString,
    notices_json: QString,
    summary_json: QString,
    progress_json: QString,
    status_text: QString,
    file_notice: QString,
    tick_count: i32,
    applying: bool,
    loading: bool,
    loaded: bool,
    session: ReviewSession,
    cancel: Arc<AtomicBool>,
    /// Bumped by every refresh and every batch, so a late result of an older
    /// one is ignored.
    epoch: u64,
}

/// A controller that goes away (its page's window closed, the app quits)
/// cancels the batch it started: the sequencer stops before its next rule, so
/// nothing more is sent and nothing is deleted for it. Rules already added
/// stay added; at process exit the worker thread simply ends with the process
/// and the record is kept.
impl Drop for LearningReviewControllerRust {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Default for LearningReviewControllerRust {
    fn default() -> Self {
        Self {
            rows_json: QString::from("[]"),
            ticks_json: QString::from("{}"),
            notices_json: QString::from("{}"),
            summary_json: QString::from("null"),
            progress_json: QString::default(),
            status_text: QString::default(),
            file_notice: QString::default(),
            tick_count: 0,
            applying: false,
            loading: false,
            loaded: false,
            session: ReviewSession::empty(),
            cancel: Arc::new(AtomicBool::new(false)),
            epoch: 0,
        }
    }
}

fn q(text: &str) -> QString {
    QString::from(text)
}

/// The saved list and (best effort) the rules list; worker thread only.
fn load_session() -> (ReviewSession, Option<&'static str>) {
    let loaded = crate::learned::stored(&crate::paths::learned_path());
    let rules = crate::bridge_runtime::handles().and_then(|handles| {
        let mut sink = BridgeSink::new(handles, Arc::new(AtomicBool::new(false)));
        read_rules(&mut sink, RULES_WAIT)
    });
    let read = rules.is_some();
    let session = ReviewSession::new(&loaded.aggregate, &rules.unwrap_or_default(), read);
    (session, loaded.notice.map(|notice| notice.text()))
}

impl qobject::LearningReviewController {
    fn session(&self) -> &ReviewSession {
        &self.rust().session
    }

    fn say(mut self: Pin<&mut Self>, text: &str) {
        self.as_mut().set_status_text(q(text));
    }

    /// Put `session` in place and show everything of it.
    fn install(mut self: Pin<&mut Self>, session: ReviewSession) {
        self.as_mut().set_rows_json(q(&session.rows_json()));
        self.as_mut().set_notices_json(q(&session.notices_json()));
        self.as_mut().rust_mut().session = session;
        self.show_choices();
    }

    /// Show what depends on the ticks (not the rows).
    fn show_choices(mut self: Pin<&mut Self>) {
        let (ticks, summary, count) = {
            let session = self.session();
            (
                session.ticks_json(),
                session.summary_json(),
                i32::try_from(session.tick_count()).unwrap_or(i32::MAX),
            )
        };
        self.as_mut().set_ticks_json(q(&ticks));
        self.as_mut().set_summary_json(q(&summary));
        self.as_mut().set_tick_count(count);
    }

    /// Replace the session after a change of ticks or confirmations.
    fn change(
        mut self: Pin<&mut Self>,
        next: Result<ReviewSession, crate::learning_review_session::Refusal>,
    ) {
        match next {
            Ok(session) => {
                self.as_mut().rust_mut().session = session;
                self.as_mut().show_choices();
            }
            Err(refusal) => self.say(&refusal.text()),
        }
    }

    fn busy_refusal(mut self: Pin<&mut Self>) -> bool {
        let busy = APPLYING.load(Ordering::SeqCst);
        if busy {
            self.as_mut()
                .say("Rules are being applied; wait until that ends.");
        }
        busy
    }

    fn refresh(mut self: Pin<&mut Self>) {
        if crate::demo::enabled() || self.as_mut().busy_refusal() {
            return;
        }
        // A refresh would replace the rows under an open summary.
        if self.session().summary().is_some() {
            return self.say("Close the summary before refreshing.");
        }
        let epoch = self.rust().epoch.wrapping_add(1);
        self.as_mut().rust_mut().epoch = epoch;
        self.as_mut().set_loading(true);
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let loaded = std::panic::catch_unwind(load_session);
            let _ = qt_thread.queue(move |mut qobject| {
                let Ok((session, notice)) = loaded else {
                    qobject.as_mut().set_loading(false);
                    return qobject.say("The recorded list could not be read.");
                };
                if qobject.rust().epoch != epoch {
                    qobject.as_mut().set_loading(false);
                    return;
                }
                if qobject.session().summary().is_some() {
                    // A summary opened while this was loading: leave it be.
                    qobject.as_mut().set_loading(false);
                    return;
                }
                qobject
                    .as_mut()
                    .set_file_notice(q(notice.unwrap_or_default()));
                qobject.as_mut().say("");
                qobject.as_mut().install(session);
                qobject.as_mut().set_loading(false);
                qobject.as_mut().set_loaded(true);
            });
        });
    }

    fn set_tick(mut self: Pin<&mut Self>, key: &QString, kind: &QString) {
        if self.as_mut().busy_refusal() {
            return;
        }
        let token = kind.to_string();
        let kind = match token.as_str() {
            "" => None,
            other => match kind_from_token(other) {
                Some(kind) => Some(kind),
                None => return self.say("That choice is not offered for this program."),
            },
        };
        let next = self.session().tick(&key.to_string(), kind);
        self.change(next);
    }

    fn select_suggested(mut self: Pin<&mut Self>) {
        if !self.as_mut().busy_refusal() {
            let next = Ok(self.session().select_suggested());
            self.change(next);
        }
    }

    fn untick_all(mut self: Pin<&mut Self>) {
        if !self.as_mut().busy_refusal() {
            let next = Ok(self.session().untick_all());
            self.change(next);
        }
    }

    fn prepare_summary(mut self: Pin<&mut Self>) {
        if self.as_mut().busy_refusal() {
            return;
        }
        let next = self.session().freeze();
        self.as_mut().change(next);
    }

    fn discard_summary(self: Pin<&mut Self>) {
        let next = Ok(self.session().unfreeze());
        self.change(next);
    }

    fn set_extra_confirmed(self: Pin<&mut Self>, index: i32, on: bool) {
        if let Ok(index) = usize::try_from(index) {
            let next = Ok(self.session().confirm(index, on));
            self.change(next);
        }
    }

    fn apply(mut self: Pin<&mut Self>) {
        if crate::demo::enabled() {
            return;
        }
        let Some(batch) = Batch::begin() else {
            return self.say("Rules are being applied; wait until that ends.");
        };
        let plan = match self.session().authorize() {
            Ok(plan) => plan.clone(),
            Err(refusal) => return self.say(&refusal.text()),
        };
        let Some(handles) = crate::bridge_runtime::handles() else {
            return self.say(NOT_CONNECTED);
        };
        // The summary closes; the ticks stay.
        let session = self.session().unfreeze();
        self.as_mut().rust_mut().session = session;
        self.as_mut().show_choices();
        let cancel = Arc::new(AtomicBool::new(false));
        let epoch = self.rust().epoch.wrapping_add(1);
        self.as_mut().rust_mut().cancel = cancel.clone();
        self.as_mut().rust_mut().epoch = epoch;
        self.as_mut().set_progress_json(QString::default());
        self.as_mut().set_loading(false);
        self.as_mut().set_applying(true);
        self.as_mut().say("Applying the rules, one at a time.");
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let mut sink = BridgeSink::new(handles, cancel);
            let progress_thread = qt_thread.clone();
            let result = run_batch_guarded(
                &plan,
                &mut sink,
                &Settings::default(),
                &crate::learned::is_active,
                &mut || batch_delete(&crate::learned::is_active, &mut clear_saved_list),
                &mut |progress| {
                    let json = progress_json(progress);
                    let _ = progress_thread.queue(move |mut qobject| {
                        if qobject.rust().epoch == epoch {
                            qobject.as_mut().set_progress_json(q(&json));
                        }
                    });
                },
            );
            // However the batch ended, the page is told so, and only then is
            // the process-wide "applying" released (inside this closure, or
            // when it is dropped unrun because the controller is gone).
            let _ = qt_thread.queue(move |mut qobject| {
                qobject.as_mut().set_applying(false);
                qobject.as_mut().set_loading(false);
                match result {
                    Ok(outcome) => {
                        qobject
                            .as_mut()
                            .set_progress_json(q(&progress_json(&outcome.progress)));
                        if outcome.deleted {
                            qobject.as_mut().say("Done. The recorded list was deleted.");
                            drop(batch);
                            qobject.as_mut().install(ReviewSession::empty());
                        } else {
                            let note = match &outcome.delete_error {
                                Some(why) => format!("Done. The recorded list was kept. {why}"),
                                None => "Done. The recorded list was kept.".to_string(),
                            };
                            qobject.as_mut().say(&note);
                            drop(batch);
                            qobject.as_mut().refresh();
                        }
                    }
                    Err(why) => {
                        qobject.as_mut().say(&why);
                        drop(batch);
                        qobject.as_mut().refresh();
                    }
                }
            });
        });
    }

    fn cancel_apply(self: Pin<&mut Self>) {
        self.rust().cancel.store(true, Ordering::SeqCst);
    }

    fn clear_data(mut self: Pin<&mut Self>) {
        if crate::demo::enabled() || self.as_mut().busy_refusal() {
            return;
        }
        let qt_thread = self.qt_thread();
        self.as_mut().set_loading(true);
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(delete_saved_list).unwrap_or_else(|_| {
                Err("The deletion stopped unexpectedly; the list may still be there.".to_string())
            });
            let _ = qt_thread.queue(move |mut qobject| {
                qobject.as_mut().set_loading(false);
                match result {
                    Ok(()) => {
                        qobject.as_mut().say("The recorded list was deleted.");
                        qobject.as_mut().set_file_notice(QString::default());
                        qobject.as_mut().rust_mut().epoch += 1;
                        qobject.install(ReviewSession::empty());
                    }
                    Err(why) => qobject.say(&why),
                }
            });
        });
    }

    fn destinations(&self, key: &QString) -> QString {
        let list: Vec<serde_json::Value> = self
            .session()
            .destinations(&key.to_string(), 200)
            .into_iter()
            .map(|(host, count)| serde_json::json!({"host": host, "count": count}))
            .collect();
        q(&serde_json::Value::Array(list).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bulk_rules::tests::fake::{plan_of, Fake};

    /// A finished batch runs its deletion while `APPLYING` is set by the batch
    /// itself. The real `batch_delete` and the real `Batch` guard, a file
    /// standing in for the saved list.
    fn finish_batch(learning: bool) -> (bool, bool) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("learned.json");
        std::fs::write(&file, b"{}").unwrap();
        let guard = Batch::begin().expect("no other test applies");
        let mut fake = Fake::new();
        let outcome = run_batch_guarded(
            &plan_of(&["a"]),
            &mut fake,
            &Settings::default(),
            &move || learning,
            &mut || {
                batch_delete(&move || learning, &mut || {
                    std::fs::remove_file(&file).map_err(|e| e.to_string())
                })
            },
            &mut |_| {},
        )
        .unwrap();
        assert!(
            applying_now(),
            "the batch holds APPLYING until it is released"
        );
        drop(guard);
        (outcome.deleted, dir.path().join("learned.json").exists())
    }

    /// One test, because `APPLYING` is process-wide.
    #[test]
    fn a_clean_batch_deletes_the_list_while_it_holds_applying_and_learning_keeps_it() {
        assert_eq!(finish_batch(false), (true, false));
        assert_eq!(finish_batch(true), (false, true));
    }
}

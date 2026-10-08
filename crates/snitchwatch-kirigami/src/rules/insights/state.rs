//! The on-demand analysis' lifecycle, apart from Qt (P2.6 Part 2).
//!
//! The analysis (`shadow::analyze`) runs on a worker thread over a copy of the
//! rule list. A rule list that changes while it runs, or after it finished,
//! makes its result untrue: the findings go and the state is `stale`, and the
//! user asks again. A worker's result is taken only if it belongs to the run
//! that is still wanted (`running`); one from a run the list outlived is
//! dropped, even when the user has since started another. A run the list
//! outlived is also told to stop ([`Run::cancel`]), so asking again after
//! every change doesn't stack threads computing answers nobody wants.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;

use super::shadow::{analyze_until, Analysis, Finding, FindingKind, MAX_ANALYZED_RULES};
use crate::rules::row_store::Rule;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    /// Never asked.
    Idle,
    Running,
    Done,
    /// More enabled rules than the analysis takes.
    TooMany,
    /// The rule list changed since the last (or the running) analysis.
    Stale,
}

impl Run {
    /// What the worker thread does: analyse `rules`, or give up with `None`
    /// once the rule list has changed. The generation goes back with the
    /// result, for [`AnalysisState::finish`].
    pub fn execute(&self, rules: &[Rule]) -> Option<(u64, Analysis)> {
        analyze_until(rules, &self.cancel).map(|analysis| (self.generation, analysis))
    }
}

#[derive(Debug)]
pub struct AnalysisState {
    phase: Phase,
    /// Bumped by every rule list change.
    generation: u64,
    /// The generation of the run still wanted, if one is, and its cancel flag.
    running: Option<(u64, Arc<AtomicBool>)>,
    enabled: usize,
    findings: BTreeMap<String, Finding>,
}

impl Default for AnalysisState {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            generation: 0,
            running: None,
            enabled: 0,
            findings: BTreeMap::new(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Summary {
    state: Phase,
    enabled: usize,
    limit: usize,
    never_decides: usize,
    may_be_shadowed: usize,
}

/// A run to start: the rule list generation it is for, and the flag it
/// should check to stop early.
#[derive(Debug, Clone)]
pub struct Run {
    pub generation: u64,
    pub cancel: Arc<AtomicBool>,
}

impl AnalysisState {
    /// The rule list changed (or was replaced). Drops the findings.
    pub fn rules_changed(&mut self) {
        self.generation += 1;
        if let Some((_, cancel)) = self.running.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.findings.clear();
        if self.phase != Phase::Idle {
            self.phase = Phase::Stale;
        }
    }

    /// The user asked. The run to start, or `None` if one is already running.
    pub fn start(&mut self) -> Option<Run> {
        if self.phase == Phase::Running {
            return None;
        }
        let run = Run {
            generation: self.generation,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        self.phase = Phase::Running;
        self.running = Some((run.generation, run.cancel.clone()));
        self.findings.clear();
        Some(run)
    }

    /// A worker finished the analysis it began at `generation`. A result for
    /// a run that is no longer wanted changes nothing.
    pub fn finish(&mut self, generation: u64, analysis: Analysis) {
        if self.running.as_ref().map(|(wanted, _)| *wanted) != Some(generation) {
            return;
        }
        self.running = None;
        match analysis {
            Analysis::Done { findings } => {
                self.findings = findings;
                self.phase = Phase::Done;
            }
            Analysis::TooMany { enabled, .. } => {
                self.enabled = enabled;
                self.phase = Phase::TooMany;
            }
        }
    }

    pub fn finding(&self, rule_name: &str) -> Option<&Finding> {
        self.findings.get(rule_name)
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn summary_json(&self) -> String {
        let count = |kind| self.findings.values().filter(|f| f.kind == kind).count();
        let summary = Summary {
            state: self.phase,
            enabled: self.enabled,
            limit: MAX_ANALYZED_RULES,
            never_decides: count(FindingKind::NeverDecides),
            may_be_shadowed: count(FindingKind::MayBeShadowed),
        };
        serde_json::to_string(&summary).unwrap_or_default()
    }
}

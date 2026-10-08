//! The on-demand analysis' lifecycle, apart from Qt (P2.6 Part 2).
//!
//! The analysis (`shadow::analyze`) runs on a worker thread over a copy of the
//! rule list. A rule list that changes while it runs, or after it finished,
//! makes its result untrue, so a result is shown only for the **generation**
//! of the list it was computed from. Anything older becomes `stale`, with the
//! findings gone; the user asks again.

use std::collections::BTreeMap;

use serde::Serialize;

use super::shadow::{Analysis, Finding, FindingKind, MAX_ANALYZED_RULES};

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

#[derive(Debug)]
pub struct AnalysisState {
    phase: Phase,
    generation: u64,
    enabled: usize,
    findings: BTreeMap<String, Finding>,
}

impl Default for AnalysisState {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            generation: 0,
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
    redundant: usize,
    never_applies: usize,
    may_be_shadowed: usize,
}

impl AnalysisState {
    /// The rule list changed (or was replaced). Drops the findings.
    pub fn rules_changed(&mut self) {
        self.generation += 1;
        self.findings.clear();
        if self.phase != Phase::Idle {
            self.phase = Phase::Stale;
        }
    }

    /// The user asked. `Some(generation)` to run for, or `None` if one is
    /// already running.
    pub fn start(&mut self) -> Option<u64> {
        if self.phase == Phase::Running {
            return None;
        }
        self.phase = Phase::Running;
        self.findings.clear();
        Some(self.generation)
    }

    /// A worker finished the analysis it began at `generation`.
    pub fn finish(&mut self, generation: u64, analysis: Analysis) {
        if generation != self.generation || self.phase != Phase::Running {
            self.findings.clear();
            self.phase = Phase::Stale;
            return;
        }
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
            redundant: count(FindingKind::Redundant),
            never_applies: count(FindingKind::NeverApplies),
            may_be_shadowed: count(FindingKind::MayBeShadowed),
        };
        serde_json::to_string(&summary).unwrap_or_default()
    }
}

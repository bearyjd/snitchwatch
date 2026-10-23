//! What a UI renders while a batch runs: one status per row, counts, and
//! the state of the batch. Plain values; every change builds a new one.
//! The texts state facts (what was sent, what came back), nothing more.

use super::plan::BulkPlan;

/// Longest outside text (a daemon reason) shown in a row.
const MAX_OUTSIDE_CHARS: usize = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotSentWhy {
    Cancelled,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowStatus {
    Pending,
    Sending,
    Added,
    AddedWithNote(String),
    /// The same rule was already in the list; nothing sent.
    AlreadyPresent,
    /// A rule with this name has other content (edited by the owner);
    /// nothing sent.
    ChangedByOwner,
    Refused(Vec<String>),
    Rejected(String),
    /// No answer came, and the rules list then held this rule.
    ConfirmedAfterNoAnswer,
    /// No answer came, and the list then held a rule of this name with
    /// other content. Left alone.
    DifferentAfterNoAnswer,
    /// No answer came and the rule is not in the list (or the list could
    /// not be read). Not sent again.
    NotConfirmed(String),
    /// The list leaves rules out, so the name could not be looked up.
    NotChecked,
    NotSent(NotSentWhy),
}

impl RowStatus {
    pub fn text(&self) -> String {
        match self {
            Self::Pending => "Waiting".into(),
            Self::Sending => "Sending".into(),
            Self::Added => "Added".into(),
            Self::AddedWithNote(note) => format!("Added. Note: {}", outside(note)),
            Self::AlreadyPresent => "Already present, not sent again".into(),
            Self::ChangedByOwner => "Changed by you, left alone".into(),
            Self::Refused(problems) => format!("Refused: {}", problems.join("; ")),
            Self::Rejected(reason) => format!("Rejected: {}", outside(reason)),
            Self::ConfirmedAfterNoAnswer => {
                "No answer was seen; the rules list now holds this rule".into()
            }
            Self::DifferentAfterNoAnswer => {
                "No answer was seen; a rule with this name has other content, left alone".into()
            }
            Self::NotConfirmed(why) => format!("No answer was seen; not sent again. {why}"),
            Self::NotChecked => {
                "Not sent: the rules list leaves some rules out, so this name could not be looked up"
                    .into()
            }
            Self::NotSent(NotSentWhy::Cancelled) => "Not sent: cancelled".into(),
            Self::NotSent(NotSentWhy::Stopped) => "Not sent: the batch stopped".into(),
        }
    }
}

fn outside(text: &str) -> String {
    text.chars().take(MAX_OUTSIDE_CHARS).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortReason {
    /// The rules list was not available (the text says why).
    ListUnavailable(String),
    /// The firewall service is not connected.
    NoDaemon,
    RepeatedTimeouts(u32),
    /// Snitchwatch's link to its service is down.
    NotConnected,
    /// The send queue stayed full.
    QueueBusy,
    /// The service connection changed during the batch.
    SessionLost,
}

impl AbortReason {
    pub fn text(&self) -> String {
        match self {
            Self::ListUnavailable(why) => format!("The rules list is not available. {why}"),
            Self::NoDaemon => "The firewall service is not connected.".into(),
            Self::RepeatedTimeouts(n) => format!("{n} rules in a row got no answer."),
            Self::NotConnected => "Snitchwatch is not connected to its service.".into(),
            Self::QueueBusy => "Snitchwatch's send queue stayed full.".into(),
            Self::SessionLost => "The connection to the service changed during the batch.".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchState {
    Running,
    Finished,
    Cancelled,
    Aborted(AbortReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub label: String,
    pub status: RowStatus,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub added: usize,
    pub already_present: usize,
    pub left_alone: usize,
    pub refused: usize,
    pub rejected: usize,
    /// No answer and not confirmed, or not looked up.
    pub unconfirmed: usize,
    pub not_sent: usize,
    /// Waiting or sending.
    pub waiting: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub rows: Vec<Row>,
    pub state: BatchState,
    /// Names that came more than once in the plan and were planned once.
    pub dropped: Vec<String>,
}

impl Progress {
    pub fn new(plan: &BulkPlan) -> Self {
        Self {
            rows: plan
                .rules()
                .iter()
                .map(|rule| Row {
                    name: rule.name().to_owned(),
                    label: rule.label.clone(),
                    status: RowStatus::Pending,
                })
                .collect(),
            state: BatchState::Running,
            dropped: plan.dropped().to_vec(),
        }
    }

    pub fn counts(&self) -> Counts {
        self.rows.iter().fold(Counts::default(), |c, row| {
            let mut c = c;
            match row.status {
                RowStatus::Added
                | RowStatus::AddedWithNote(_)
                | RowStatus::ConfirmedAfterNoAnswer => c.added += 1,
                RowStatus::AlreadyPresent => c.already_present += 1,
                RowStatus::ChangedByOwner | RowStatus::DifferentAfterNoAnswer => c.left_alone += 1,
                RowStatus::Refused(_) => c.refused += 1,
                RowStatus::Rejected(_) => c.rejected += 1,
                RowStatus::NotConfirmed(_) | RowStatus::NotChecked => c.unconfirmed += 1,
                RowStatus::NotSent(_) => c.not_sent += 1,
                RowStatus::Pending | RowStatus::Sending => c.waiting += 1,
            }
            c
        })
    }

    pub fn with_status(&self, index: usize, status: RowStatus) -> Self {
        let rows = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| match i == index {
                true => Row {
                    status: status.clone(),
                    ..row.clone()
                },
                false => row.clone(),
            })
            .collect();
        Self {
            rows,
            ..self.clone()
        }
    }

    pub fn with_state(&self, state: BatchState) -> Self {
        Self {
            state,
            ..self.clone()
        }
    }

    /// Every row from `from` on that is still waiting (or was about to be
    /// sent) becomes "not sent".
    pub fn with_rest_not_sent(&self, from: usize, why: NotSentWhy) -> Self {
        let rows = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                match i >= from && matches!(row.status, RowStatus::Pending | RowStatus::Sending) {
                    true => Row {
                        status: RowStatus::NotSent(why.clone()),
                        ..row.clone()
                    },
                    false => row.clone(),
                }
            })
            .collect();
        Self {
            rows,
            ..self.clone()
        }
    }

    /// The one line a UI shows under the list.
    pub fn summary(&self) -> String {
        let c = self.counts();
        let mut parts = vec![format!("{} added", c.added)];
        for (n, what) in [
            (c.already_present, "already present"),
            (c.left_alone, "left alone"),
            (c.refused, "refused"),
            (c.rejected, "rejected"),
            (c.unconfirmed, "not confirmed"),
            (c.not_sent, "not sent"),
            (c.waiting, "waiting"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        }
        let head = match &self.state {
            BatchState::Aborted(why) => format!("Stopped. {} ", why.text()),
            BatchState::Cancelled => "Cancelled. ".into(),
            _ => String::new(),
        };
        if !self.dropped.is_empty() {
            parts.push(format!(
                "{} repeated names planned once",
                self.dropped.len()
            ));
        }
        format!("{head}{}", parts.join(", "))
    }
}

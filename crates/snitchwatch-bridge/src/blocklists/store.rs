//! SQLite storage for blocklist subscriptions and their resolved entries.

use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub id: String,
    pub url: String,
    pub display_name: String,
    pub format_hint: Option<String>,
    pub refresh_interval_secs: i64,
    pub last_fetched_at: Option<DateTime<Utc>>,
    pub last_fetch_status: FetchStatus,
    pub entry_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchStatus {
    Pending,
    Ok,
    Failed { reason: String },
}

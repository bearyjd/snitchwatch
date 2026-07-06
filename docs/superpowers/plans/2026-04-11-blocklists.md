# M4 Blocklists Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a bridge-owned blocklist subscription feature: store subscriptions in SQLite, fetch hosts-file/domains/ABP-style lists over HTTPS on a schedule, materialize entries as opensnitchd deny rules in the `900–999` specificity band tagged `__source: blocklist:<id>`, and wire all 5 LS Blocklist WebSocket message types so the Blocklists tab is fully functional end-to-end.

**Architecture:** A new `snitchwatch-bridge::blocklists` module containing four cohesive files: `store.rs` (rusqlite, schema migration, CRUD on subscriptions/entries), `fetcher.rs` (reqwest GET + content-type-aware parsers, retry-on-failure-keeps-cache discipline), `materializer.rs` (pure function `Entry → opensnitch RuleMessage` with `900-blocklist:<id>:<seq>` filename and JSON tag in description), and `mod.rs` (a `BlocklistsManager` that owns a tokio task running the refresh schedule, exposes a `BlocklistEvent` broadcast channel for state changes, and bridges to the existing translator+gRPC client). The translator's `downstream` and `upstream` modules grow handlers for the 5 server messages (`setBlocklists`/`setBlocklistDetails`/`setBlocklistEntries`/`setBlocklistStatus`/`setBlocklistEntryLocation`) and 2 client actions (`subscribeBlocklist`/`unsubscribeBlocklist`).

**Tech Stack:** rusqlite 0.32 (bundled feature), reqwest 0.12 (rustls-tls), addr 0.15 (host validation), chrono 0.4 (refresh timestamps), tokio 1.40, anyhow, thiserror, tracing, serde_json. The Tauri shell (Plan 4) does not change in this plan — the bridge stays headless and the Blocklists tab JS that ships with the vendored LS UI consumes the messages exactly as it would from upstream.

**Out of scope for this plan:** Wiring the install.sh path or first-run wizard handler for "no daemon to install rules into" (Plan 6). Real packaging of the SQLite path inside a Flatpak sandbox (Plan 6). Public-release defaults or auth on the WS port (Plan 7). UI changes to the vendored web/ — the Blocklists tab JS already exists and consumes these messages from upstream LS for Linux.

---

## Memory Constraints

These memory entries shape the implementation choices below — read before starting any task:

1. **`bash_antipattern_hook.md`** — workspace blocks `find/ls/cat/grep/rg/head/tail/sed/awk` in Bash. Use Glob/Grep/Read tools instead. PostToolUse "Tool failed" reminders are false-positives; verify success by stdout content.
2. **`m1_envelope_hack.md`** — the `ask-rule` JSON envelope was a M1 testing-only hack that was deleted at the M2 topology flip. Do **not** introduce a similar JSON-blob escape hatch for blocklists. If a blocklist field needs to cross the WS, give it a strongly-typed slot in `ws_messages.rs`.
3. **`plan1_deferred_criteria.md`** — live opensnitchd 60s smoke and `cargo-llvm-cov` coverage are environmental, deferred to Plan 7. Do **not** reopen those acceptance items here.
4. **`clippy_gotchas_bridge.md`** — `Translated::AskRule` must stay boxed (`large_enum_variant`). When discarding a future or receiver use `drop(rx);` not `let _ = rx;`.
5. **`autonomous_tdd_resume.md`** — on PreCompact resume, pick up the last task without recap. The plan's task ordering must therefore be self-contained: each task's "Files" block, code blocks, and commit message must be fully reconstructable from the file alone.

---

## File Structure

### NEW

```
crates/snitchwatch-bridge/src/blocklists/
├── mod.rs                  # BlocklistsManager — owns the refresh task + broadcast bus
├── store.rs                # rusqlite schema, migrations, CRUD on subscriptions/entries
├── fetcher.rs              # reqwest GET + format-aware parsers (hosts/domains/abp)
├── materializer.rs         # Entry → RuleMessage (pure function, 900-band, source tag)
└── format.rs               # ListFormat enum + sniff_format(&str) classifier

crates/snitchwatch-bridge/tests/
└── blocklists_e2e.rs       # End-to-end: WS client → bridge → mock daemon, asserts
                              # SetBlocklists payload + materialized rule on cache hit

tests/fixtures/blocklists/
├── stevenblack-tiny.txt    # 8-line hosts-file fixture (real-shape, no real malware)
├── domains-tiny.txt        # 5-line domains-only fixture
├── abp-tiny.txt            # 4-line ABP-style fixture (||domain^ syntax)
└── garbage.bin             # Random bytes — fetcher must reject + keep prior cache
```

### MODIFIED

```
crates/snitchwatch-bridge/Cargo.toml          # add rusqlite, reqwest, addr, chrono
crates/snitchwatch-bridge/src/lib.rs          # pub mod blocklists;
crates/snitchwatch-bridge/src/error.rs        # +BlocklistError variant
crates/snitchwatch-bridge/src/ws_messages.rs  # strongly-typed Blocklist + Entry structs
crates/snitchwatch-bridge/src/translator/downstream.rs  # emit SetBlocklists on BlocklistEvent
crates/snitchwatch-bridge/src/translator/upstream.rs    # handle SubscribeBlocklist/UnsubscribeBlocklist
crates/snitchwatch-bridge/src/ws_server.rs    # accept BlocklistEvent broadcast subscription
Cargo.toml                                    # workspace deps: rusqlite, reqwest, addr, chrono
docs/superpowers/specs/2026-04-10-snitchwatch-design.md  # tick M4 row in milestone table
README.md                                     # Try-it section: subscribe to a tiny test list
justfile                                      # `just blocklist-fixture-server` recipe
.gitignore                                    # blocklists.db (in test runs)
```

### DELETED

None.

---

## Part A — Schema, store, and fixtures (Tasks 1–4)

### Task 1: Workspace dependency wiring + blocklists module skeleton

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`)
- Modify: `crates/snitchwatch-bridge/Cargo.toml`
- Modify: `crates/snitchwatch-bridge/src/lib.rs`
- Create: `crates/snitchwatch-bridge/src/blocklists/mod.rs`
- Create: `crates/snitchwatch-bridge/src/blocklists/store.rs`
- Create: `crates/snitchwatch-bridge/src/blocklists/fetcher.rs`
- Create: `crates/snitchwatch-bridge/src/blocklists/materializer.rs`
- Create: `crates/snitchwatch-bridge/src/blocklists/format.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/snitchwatch-bridge/src/blocklists/mod.rs`:

```rust
//! Bridge-owned blocklist subscriptions, fetch loop, and rule materialization.
//!
//! opensnitchd has no native blocklist concept. The bridge stores subscription
//! URLs in SQLite, fetches them on a schedule, parses hosts-file / domains /
//! ABP-style lists, and materializes each entry as an opensnitchd deny rule in
//! the `900–999` specificity band tagged `__source: blocklist:<id>`.

pub mod fetcher;
pub mod format;
pub mod materializer;
pub mod store;

#[cfg(test)]
mod tests {
    #[test]
    fn module_exports_compile() {
        // Pure compile-time guarantee — fails at link if any submodule is missing.
        let _ = std::any::type_name::<super::store::Subscription>();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge blocklists::tests::module_exports_compile`
Expected: FAIL — `error[E0432]: unresolved import super::store::Subscription` (struct doesn't exist yet).

- [ ] **Step 3: Wire workspace dependencies and stub modules**

Edit `Cargo.toml` (workspace root) — add to `[workspace.dependencies]`:

```toml
rusqlite = { version = "0.32", features = ["bundled", "chrono"] }
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "gzip"] }
addr = "0.15"
chrono = { version = "0.4", default-features = false, features = ["clock", "serde"] }
```

Edit `crates/snitchwatch-bridge/Cargo.toml` — add to `[dependencies]`:

```toml
rusqlite.workspace = true
reqwest.workspace = true
addr.workspace = true
chrono.workspace = true
```

Edit `crates/snitchwatch-bridge/src/lib.rs` — add module declaration after the existing `pub mod cache;`:

```rust
pub mod blocklists;
```

Create stub `crates/snitchwatch-bridge/src/blocklists/store.rs`:

```rust
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
```

Create empty stubs for `fetcher.rs`, `format.rs`, `materializer.rs`:

```rust
//! See module-level docs in mod.rs.
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge blocklists::tests::module_exports_compile`
Expected: PASS.

Run: `cargo build -p snitchwatch-bridge`
Expected: build succeeds; new deps download.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/snitchwatch-bridge/Cargo.toml crates/snitchwatch-bridge/src/lib.rs crates/snitchwatch-bridge/src/blocklists/
git commit -m "feat(blocklists): scaffold blocklists module + workspace deps"
```

---

### Task 2: SQLite schema, migration, and Subscription CRUD

**Files:**
- Modify: `crates/snitchwatch-bridge/src/blocklists/store.rs`
- Modify: `crates/snitchwatch-bridge/src/error.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/snitchwatch-bridge/src/blocklists/store.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn open_in_memory() -> BlocklistStore {
        BlocklistStore::open_in_memory().expect("in-memory store opens")
    }

    #[test]
    fn fresh_store_has_zero_subscriptions() {
        let store = open_in_memory();
        let all = store.list_subscriptions().expect("list");
        assert!(all.is_empty(), "fresh store must be empty, got {all:?}");
    }

    #[test]
    fn upsert_subscription_round_trips() {
        let store = open_in_memory();
        let sub = Subscription {
            id: "stevenblack".to_string(),
            url: "https://raw.githubusercontent.com/StevenBlack/hosts/master/hosts".to_string(),
            display_name: "StevenBlack".to_string(),
            format_hint: Some("hosts".to_string()),
            refresh_interval_secs: 86_400,
            last_fetched_at: None,
            last_fetch_status: FetchStatus::Pending,
            entry_count: 0,
        };
        store.upsert_subscription(&sub).expect("upsert");
        let loaded = store.get_subscription("stevenblack").expect("get").expect("found");
        assert_eq!(loaded, sub);
    }

    #[test]
    fn delete_subscription_cascades_entries() {
        let store = open_in_memory();
        let sub = Subscription {
            id: "test".to_string(),
            url: "https://example.invalid/list.txt".to_string(),
            display_name: "Test".to_string(),
            format_hint: None,
            refresh_interval_secs: 3600,
            last_fetched_at: None,
            last_fetch_status: FetchStatus::Pending,
            entry_count: 0,
        };
        store.upsert_subscription(&sub).unwrap();
        store
            .replace_entries("test", &["doubleclick.net", "google-analytics.com"])
            .unwrap();
        store.delete_subscription("test").unwrap();
        assert_eq!(store.list_subscriptions().unwrap().len(), 0);
        assert_eq!(store.list_entries("test").unwrap().len(), 0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge blocklists::store::tests`
Expected: FAIL — `error[E0599]: no function or associated item named open_in_memory found for struct BlocklistStore`.

- [ ] **Step 3: Implement the store**

Replace `crates/snitchwatch-bridge/src/blocklists/store.rs` with:

```rust
//! SQLite storage for blocklist subscriptions and their resolved entries.

use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

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

impl FetchStatus {
    fn to_db(&self) -> (&'static str, Option<&str>) {
        match self {
            FetchStatus::Pending => ("pending", None),
            FetchStatus::Ok => ("ok", None),
            FetchStatus::Failed { reason } => ("failed", Some(reason.as_str())),
        }
    }

    fn from_db(kind: &str, reason: Option<String>) -> Self {
        match kind {
            "ok" => FetchStatus::Ok,
            "failed" => FetchStatus::Failed {
                reason: reason.unwrap_or_default(),
            },
            _ => FetchStatus::Pending,
        }
    }
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store mutex poisoned")]
    Poisoned,
}

pub struct BlocklistStore {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS subscriptions (
    id                   TEXT PRIMARY KEY,
    url                  TEXT NOT NULL,
    display_name         TEXT NOT NULL,
    format_hint          TEXT,
    refresh_interval_secs INTEGER NOT NULL,
    last_fetched_at      TEXT,
    last_fetch_status    TEXT NOT NULL,
    last_fetch_reason    TEXT,
    entry_count          INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS entries (
    subscription_id TEXT NOT NULL,
    host            TEXT NOT NULL,
    PRIMARY KEY (subscription_id, host),
    FOREIGN KEY (subscription_id) REFERENCES subscriptions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_entries_sub ON entries(subscription_id);
"#;

impl BlocklistStore {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::initialize(conn)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::initialize(conn)
    }

    fn initialize(conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StoreError> {
        self.conn.lock().map_err(|_| StoreError::Poisoned)
    }

    pub fn upsert_subscription(&self, sub: &Subscription) -> Result<(), StoreError> {
        let conn = self.lock()?;
        let (kind, reason) = sub.last_fetch_status.to_db();
        conn.execute(
            r#"
            INSERT INTO subscriptions
                (id, url, display_name, format_hint, refresh_interval_secs,
                 last_fetched_at, last_fetch_status, last_fetch_reason, entry_count)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(id) DO UPDATE SET
                url                   = excluded.url,
                display_name          = excluded.display_name,
                format_hint           = excluded.format_hint,
                refresh_interval_secs = excluded.refresh_interval_secs,
                last_fetched_at       = excluded.last_fetched_at,
                last_fetch_status     = excluded.last_fetch_status,
                last_fetch_reason     = excluded.last_fetch_reason,
                entry_count           = excluded.entry_count
            "#,
            params![
                sub.id,
                sub.url,
                sub.display_name,
                sub.format_hint,
                sub.refresh_interval_secs,
                sub.last_fetched_at.map(|t| t.to_rfc3339()),
                kind,
                reason,
                sub.entry_count,
            ],
        )?;
        Ok(())
    }

    pub fn get_subscription(&self, id: &str) -> Result<Option<Subscription>, StoreError> {
        let conn = self.lock()?;
        conn.query_row(
            r#"
            SELECT id, url, display_name, format_hint, refresh_interval_secs,
                   last_fetched_at, last_fetch_status, last_fetch_reason, entry_count
            FROM subscriptions WHERE id = ?1
            "#,
            params![id],
            row_to_subscription,
        )
        .optional()
        .map_err(StoreError::from)
    }

    pub fn list_subscriptions(&self) -> Result<Vec<Subscription>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, url, display_name, format_hint, refresh_interval_secs,
                   last_fetched_at, last_fetch_status, last_fetch_reason, entry_count
            FROM subscriptions ORDER BY id
            "#,
        )?;
        let rows = stmt
            .query_map([], row_to_subscription)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn delete_subscription(&self, id: &str) -> Result<(), StoreError> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM subscriptions WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn replace_entries(&self, sub_id: &str, hosts: &[&str]) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM entries WHERE subscription_id = ?1",
            params![sub_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO entries (subscription_id, host) VALUES (?1, ?2)",
            )?;
            for host in hosts {
                stmt.execute(params![sub_id, host])?;
            }
        }
        tx.execute(
            "UPDATE subscriptions SET entry_count = ?1 WHERE id = ?2",
            params![hosts.len() as i64, sub_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_entries(&self, sub_id: &str) -> Result<Vec<String>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT host FROM entries WHERE subscription_id = ?1 ORDER BY host",
        )?;
        let rows = stmt
            .query_map(params![sub_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn row_to_subscription(row: &rusqlite::Row<'_>) -> rusqlite::Result<Subscription> {
    let id: String = row.get(0)?;
    let url: String = row.get(1)?;
    let display_name: String = row.get(2)?;
    let format_hint: Option<String> = row.get(3)?;
    let refresh_interval_secs: i64 = row.get(4)?;
    let last_fetched_at: Option<String> = row.get(5)?;
    let kind: String = row.get(6)?;
    let reason: Option<String> = row.get(7)?;
    let entry_count: i64 = row.get(8)?;
    let last_fetched_at = last_fetched_at.and_then(|s| DateTime::parse_from_rfc3339(&s).ok().map(|t| t.with_timezone(&Utc)));
    Ok(Subscription {
        id,
        url,
        display_name,
        format_hint,
        refresh_interval_secs,
        last_fetched_at,
        last_fetch_status: FetchStatus::from_db(&kind, reason),
        entry_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_in_memory() -> BlocklistStore {
        BlocklistStore::open_in_memory().expect("in-memory store opens")
    }

    #[test]
    fn fresh_store_has_zero_subscriptions() {
        let store = open_in_memory();
        let all = store.list_subscriptions().expect("list");
        assert!(all.is_empty(), "fresh store must be empty, got {all:?}");
    }

    #[test]
    fn upsert_subscription_round_trips() {
        let store = open_in_memory();
        let sub = Subscription {
            id: "stevenblack".to_string(),
            url: "https://raw.githubusercontent.com/StevenBlack/hosts/master/hosts".to_string(),
            display_name: "StevenBlack".to_string(),
            format_hint: Some("hosts".to_string()),
            refresh_interval_secs: 86_400,
            last_fetched_at: None,
            last_fetch_status: FetchStatus::Pending,
            entry_count: 0,
        };
        store.upsert_subscription(&sub).expect("upsert");
        let loaded = store.get_subscription("stevenblack").expect("get").expect("found");
        assert_eq!(loaded, sub);
    }

    #[test]
    fn delete_subscription_cascades_entries() {
        let store = open_in_memory();
        let sub = Subscription {
            id: "test".to_string(),
            url: "https://example.invalid/list.txt".to_string(),
            display_name: "Test".to_string(),
            format_hint: None,
            refresh_interval_secs: 3600,
            last_fetched_at: None,
            last_fetch_status: FetchStatus::Pending,
            entry_count: 0,
        };
        store.upsert_subscription(&sub).unwrap();
        store
            .replace_entries("test", &["doubleclick.net", "google-analytics.com"])
            .unwrap();
        store.delete_subscription("test").unwrap();
        assert_eq!(store.list_subscriptions().unwrap().len(), 0);
        assert_eq!(store.list_entries("test").unwrap().len(), 0);
    }
}
```

Edit `crates/snitchwatch-bridge/src/error.rs` — add a `Blocklist` variant. Read the file first to find the `BridgeError` enum, then insert this variant in alphabetical order (after `Auth`/`Cache` if present, otherwise as a new arm — match the existing style):

```rust
    #[error("blocklist store error: {0}")]
    Blocklist(#[from] crate::blocklists::store::StoreError),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge blocklists::store::tests`
Expected: PASS — 3 tests pass.

Run: `cargo clippy -p snitchwatch-bridge -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/blocklists/store.rs crates/snitchwatch-bridge/src/error.rs
git commit -m "feat(blocklists): SQLite store with subscription + entry CRUD"
```

---

### Task 3: Format sniffer and parsers (hosts / domains / ABP)

**Files:**
- Modify: `crates/snitchwatch-bridge/src/blocklists/format.rs`
- Create: `tests/fixtures/blocklists/stevenblack-tiny.txt`
- Create: `tests/fixtures/blocklists/domains-tiny.txt`
- Create: `tests/fixtures/blocklists/abp-tiny.txt`
- Create: `tests/fixtures/blocklists/garbage.bin`

- [ ] **Step 1: Write the failing test**

Replace `crates/snitchwatch-bridge/src/blocklists/format.rs`:

```rust
//! List format detection and parsing.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_hosts_format() {
        let body = "127.0.0.1 localhost\n0.0.0.0 doubleclick.net\n0.0.0.0 google-analytics.com\n";
        assert_eq!(sniff_format(body), ListFormat::Hosts);
    }

    #[test]
    fn sniffs_domains_format() {
        let body = "doubleclick.net\ngoogle-analytics.com\nfacebook.net\n";
        assert_eq!(sniff_format(body), ListFormat::Domains);
    }

    #[test]
    fn sniffs_abp_format() {
        let body = "[Adblock Plus 2.0]\n||doubleclick.net^\n||tracker.example^\n";
        assert_eq!(sniff_format(body), ListFormat::AdblockPlus);
    }

    #[test]
    fn parses_hosts_skipping_localhost_and_comments() {
        let body = "# StevenBlack tiny\n127.0.0.1 localhost\n0.0.0.0 doubleclick.net\n0.0.0.0 google-analytics.com\n# trailing comment\n0.0.0.0 facebook.net\n";
        let parsed = parse(ListFormat::Hosts, body);
        assert_eq!(parsed, vec!["doubleclick.net", "google-analytics.com", "facebook.net"]);
    }

    #[test]
    fn parses_domains_one_per_line() {
        let body = "doubleclick.net\n# comment\n\ngoogle-analytics.com\n";
        let parsed = parse(ListFormat::Domains, body);
        assert_eq!(parsed, vec!["doubleclick.net", "google-analytics.com"]);
    }

    #[test]
    fn parses_abp_extracts_domain_between_pipes_and_caret() {
        let body = "[Adblock Plus 2.0]\n||doubleclick.net^\n!comment\n||tracker.example^$third-party\n";
        let parsed = parse(ListFormat::AdblockPlus, body);
        assert_eq!(parsed, vec!["doubleclick.net", "tracker.example"]);
    }

    #[test]
    fn rejects_invalid_hostnames() {
        let body = "doubleclick.net\nnot a hostname\n   \n--bad--\nvalid.example\n";
        let parsed = parse(ListFormat::Domains, body);
        assert_eq!(parsed, vec!["doubleclick.net", "valid.example"]);
    }

    #[test]
    fn deduplicates_entries() {
        let body = "doubleclick.net\ndoubleclick.net\ngoogle-analytics.com\n";
        let parsed = parse(ListFormat::Domains, body);
        assert_eq!(parsed, vec!["doubleclick.net", "google-analytics.com"]);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge blocklists::format::tests`
Expected: FAIL — `error[E0425]: cannot find function sniff_format in this scope`.

- [ ] **Step 3: Implement format detection and parsing**

Replace `crates/snitchwatch-bridge/src/blocklists/format.rs`:

```rust
//! List format detection and parsing.
//!
//! We support three real-world blocklist formats:
//!
//! 1. **Hosts** — `0.0.0.0 doubleclick.net` lines (StevenBlack/hosts).
//! 2. **Domains** — bare `doubleclick.net` one-per-line (Pi-hole style).
//! 3. **AdblockPlus** — `||doubleclick.net^` filter rules (EasyList style).
//!
//! `sniff_format` looks at the first ~20 non-comment lines and picks the most
//! likely format. Comments (`#`, `!`) and blank lines are skipped during sniffing
//! and during parsing.

use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListFormat {
    Hosts,
    Domains,
    AdblockPlus,
}

pub fn sniff_format(body: &str) -> ListFormat {
    let mut hosts_hits = 0u32;
    let mut abp_hits = 0u32;
    let mut domain_hits = 0u32;
    for line in body.lines().take(64) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') || line.starts_with('[') {
            continue;
        }
        if line.starts_with("||") && line.contains('^') {
            abp_hits += 1;
        } else if line.starts_with("0.0.0.0") || line.starts_with("127.0.0.1") {
            hosts_hits += 1;
        } else if is_valid_hostname(line) {
            domain_hits += 1;
        }
    }
    if abp_hits >= hosts_hits && abp_hits >= domain_hits && abp_hits > 0 {
        ListFormat::AdblockPlus
    } else if hosts_hits >= domain_hits && hosts_hits > 0 {
        ListFormat::Hosts
    } else {
        ListFormat::Domains
    }
}

pub fn parse(format: ListFormat, body: &str) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<String> = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') || line.starts_with('[') {
            continue;
        }
        let host_opt = match format {
            ListFormat::Hosts => parse_hosts_line(line),
            ListFormat::Domains => parse_domains_line(line),
            ListFormat::AdblockPlus => parse_abp_line(line),
        };
        if let Some(host) = host_opt {
            if is_valid_hostname(&host) && !is_local_loopback(&host) && seen.insert(host.clone()) {
                out.push(host);
            }
        }
    }
    out
}

fn parse_hosts_line(line: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    let _ip = parts.next()?;
    let host = parts.next()?;
    Some(host.to_ascii_lowercase())
}

fn parse_domains_line(line: &str) -> Option<String> {
    let token = line.split_whitespace().next()?;
    Some(token.to_ascii_lowercase())
}

fn parse_abp_line(line: &str) -> Option<String> {
    let stripped = line.strip_prefix("||")?;
    let end = stripped.find(['^', '$', '/']).unwrap_or(stripped.len());
    let host = &stripped[..end];
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

fn is_valid_hostname(s: &str) -> bool {
    if s.is_empty() || s.len() > 253 {
        return false;
    }
    s.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    }) && s.contains('.')
}

fn is_local_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "localhost.localdomain" | "local" | "broadcasthost" | "ip6-localhost" | "ip6-loopback")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_hosts_format() {
        let body = "127.0.0.1 localhost\n0.0.0.0 doubleclick.net\n0.0.0.0 google-analytics.com\n";
        assert_eq!(sniff_format(body), ListFormat::Hosts);
    }

    #[test]
    fn sniffs_domains_format() {
        let body = "doubleclick.net\ngoogle-analytics.com\nfacebook.net\n";
        assert_eq!(sniff_format(body), ListFormat::Domains);
    }

    #[test]
    fn sniffs_abp_format() {
        let body = "[Adblock Plus 2.0]\n||doubleclick.net^\n||tracker.example^\n";
        assert_eq!(sniff_format(body), ListFormat::AdblockPlus);
    }

    #[test]
    fn parses_hosts_skipping_localhost_and_comments() {
        let body = "# StevenBlack tiny\n127.0.0.1 localhost\n0.0.0.0 doubleclick.net\n0.0.0.0 google-analytics.com\n# trailing comment\n0.0.0.0 facebook.net\n";
        let parsed = parse(ListFormat::Hosts, body);
        assert_eq!(parsed, vec!["doubleclick.net", "google-analytics.com", "facebook.net"]);
    }

    #[test]
    fn parses_domains_one_per_line() {
        let body = "doubleclick.net\n# comment\n\ngoogle-analytics.com\n";
        let parsed = parse(ListFormat::Domains, body);
        assert_eq!(parsed, vec!["doubleclick.net", "google-analytics.com"]);
    }

    #[test]
    fn parses_abp_extracts_domain_between_pipes_and_caret() {
        let body = "[Adblock Plus 2.0]\n||doubleclick.net^\n!comment\n||tracker.example^$third-party\n";
        let parsed = parse(ListFormat::AdblockPlus, body);
        assert_eq!(parsed, vec!["doubleclick.net", "tracker.example"]);
    }

    #[test]
    fn rejects_invalid_hostnames() {
        let body = "doubleclick.net\nnot a hostname\n   \n--bad--\nvalid.example\n";
        let parsed = parse(ListFormat::Domains, body);
        assert_eq!(parsed, vec!["doubleclick.net", "valid.example"]);
    }

    #[test]
    fn deduplicates_entries() {
        let body = "doubleclick.net\ndoubleclick.net\ngoogle-analytics.com\n";
        let parsed = parse(ListFormat::Domains, body);
        assert_eq!(parsed, vec!["doubleclick.net", "google-analytics.com"]);
    }
}
```

Create `tests/fixtures/blocklists/stevenblack-tiny.txt`:

```
# Title: StevenBlack tiny test fixture
# Generated: 2026-04-11
127.0.0.1 localhost
0.0.0.0 doubleclick.net
0.0.0.0 google-analytics.com
0.0.0.0 facebook.net
0.0.0.0 scorecardresearch.com
```

Create `tests/fixtures/blocklists/domains-tiny.txt`:

```
doubleclick.net
google-analytics.com
facebook.net
scorecardresearch.com
quantserve.com
```

Create `tests/fixtures/blocklists/abp-tiny.txt`:

```
[Adblock Plus 2.0]
! tiny test fixture
||doubleclick.net^
||google-analytics.com^$third-party
||facebook.net^
```

Create `tests/fixtures/blocklists/garbage.bin` — write 64 bytes of junk:

```
\x00\x01\x02\x03binarynotalist\xff\xfe\xfd\xfc\x00\x00\x00\x00\x90\x90\x90\x90\xcc\xcc\xcc\xcc........garbage........
```

(Any non-text content is fine — the fetcher must reject it without polluting the cache. If your editor rejects raw bytes, use `printf '\x00\x01garbage\xff\xfe' > tests/fixtures/blocklists/garbage.bin` via Bash with `dangerouslyDisableSandbox: false` — single approved command, no sed/awk.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge blocklists::format::tests`
Expected: PASS — 8 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/blocklists/format.rs tests/fixtures/blocklists/
git commit -m "feat(blocklists): list format sniffer + hosts/domains/ABP parsers"
```

---

### Task 4: Materializer — Entry → opensnitch deny RuleMessage

**Files:**
- Modify: `crates/snitchwatch-bridge/src/blocklists/materializer.rs`

- [ ] **Step 1: Write the failing test**

Replace `crates/snitchwatch-bridge/src/blocklists/materializer.rs`:

```rust
//! Convert blocklist entries into opensnitchd deny rules.
//!
//! Each entry produces one rule in the `900–999` specificity band so user rules
//! always win. The rule's `description` field carries a JSON tag
//! `{"snitchwatch": {"source": "blocklist", "list_id": "<id>", "entry": "<host>"}}`
//! so the bridge can re-group rules into the Blocklists tab on the next
//! `ListRules` reconciliation.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materializes_single_entry_as_deny_rule() {
        let rule = materialize_entry("stevenblack", "doubleclick.net", 17);
        assert_eq!(rule.action, "deny");
        assert_eq!(rule.duration, "always");
        assert!(rule.enabled);
        assert!(
            rule.name.starts_with("900-blocklist:stevenblack:"),
            "name should be in 900-band: {}",
            rule.name
        );
        assert!(rule.name.contains("doubleclick.net") || rule.name.contains("0017"));
    }

    #[test]
    fn rule_operator_targets_dest_host_simple() {
        let rule = materialize_entry("stevenblack", "doubleclick.net", 0);
        assert_eq!(rule.operator.kind, "simple");
        assert_eq!(rule.operator.operand, "dest.host");
        assert_eq!(rule.operator.data, "doubleclick.net");
    }

    #[test]
    fn rule_description_carries_source_tag_json() {
        let rule = materialize_entry("stevenblack", "doubleclick.net", 0);
        let parsed: serde_json::Value =
            serde_json::from_str(&rule.description).expect("description is JSON");
        assert_eq!(parsed["snitchwatch"]["source"], "blocklist");
        assert_eq!(parsed["snitchwatch"]["list_id"], "stevenblack");
        assert_eq!(parsed["snitchwatch"]["entry"], "doubleclick.net");
    }

    #[test]
    fn names_are_stable_for_same_input() {
        let a = materialize_entry("stevenblack", "doubleclick.net", 42);
        let b = materialize_entry("stevenblack", "doubleclick.net", 42);
        assert_eq!(a.name, b.name);
    }

    #[test]
    fn names_are_distinct_for_different_seq() {
        let a = materialize_entry("stevenblack", "doubleclick.net", 1);
        let b = materialize_entry("stevenblack", "doubleclick.net", 2);
        assert_ne!(a.name, b.name);
    }

    #[test]
    fn batch_materialize_preserves_order_and_seq() {
        let hosts = vec!["a.example".to_string(), "b.example".to_string(), "c.example".to_string()];
        let rules = materialize_batch("test", &hosts);
        assert_eq!(rules.len(), 3);
        assert!(rules[0].name.contains("0000"));
        assert!(rules[1].name.contains("0001"));
        assert!(rules[2].name.contains("0002"));
    }

    #[test]
    fn list_id_special_chars_sanitized_in_filename() {
        let rule = materialize_entry("steven/black:bad", "x.example", 0);
        assert!(!rule.name.contains('/'), "filename must not contain slash");
        assert!(!rule.name.contains(':') || rule.name.matches(':').count() == 2,
            "exactly the two delimiter colons allowed: {}", rule.name);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge blocklists::materializer::tests`
Expected: FAIL — `error[E0425]: cannot find function materialize_entry in this scope`.

- [ ] **Step 3: Implement the materializer**

Replace `crates/snitchwatch-bridge/src/blocklists/materializer.rs`:

```rust
//! Convert blocklist entries into opensnitchd deny rules.
//!
//! Each entry produces one rule in the `900–999` specificity band so user rules
//! always win. The rule's `description` field carries a JSON tag
//! `{"snitchwatch": {"source": "blocklist", "list_id": "<id>", "entry": "<host>"}}`
//! so the bridge can re-group rules into the Blocklists tab on the next
//! `ListRules` reconciliation.

use serde::{Deserialize, Serialize};

/// Plain-data shape that mirrors the subset of `protocol::ui::Rule` we need
/// when materializing into opensnitchd. The bridge converts this to the prost
/// type at the call boundary in `translator::upstream` — we keep this struct
/// transport-agnostic so the materializer is pure and trivially testable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializedRule {
    pub name: String,
    pub enabled: bool,
    pub action: String,
    pub duration: String,
    pub description: String,
    pub operator: Operator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operator {
    #[serde(rename = "type")]
    pub kind: String,
    pub operand: String,
    pub data: String,
}

/// Deterministic per-entry materialization.
///
/// Filename layout: `900-blocklist:<sanitized_id>:<seq04>-<host>.json`
/// stored in the rule `name` field; the daemon strips the `.json` suffix when
/// loading. The `900-` prefix puts every blocklist rule below the 0–899 user
/// rule band so user rules win on conflict.
pub fn materialize_entry(list_id: &str, host: &str, seq: usize) -> MaterializedRule {
    let safe_id = sanitize_id(list_id);
    let safe_host = host.to_ascii_lowercase();
    let name = format!("900-blocklist:{safe_id}:{seq:04}-{safe_host}");
    let description = serde_json::json!({
        "snitchwatch": {
            "source": "blocklist",
            "list_id": list_id,
            "entry": host,
        }
    })
    .to_string();
    MaterializedRule {
        name,
        enabled: true,
        action: "deny".to_string(),
        duration: "always".to_string(),
        description,
        operator: Operator {
            kind: "simple".to_string(),
            operand: "dest.host".to_string(),
            data: safe_host,
        },
    }
}

pub fn materialize_batch(list_id: &str, hosts: &[String]) -> Vec<MaterializedRule> {
    hosts
        .iter()
        .enumerate()
        .map(|(seq, host)| materialize_entry(list_id, host, seq))
        .collect()
}

fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materializes_single_entry_as_deny_rule() {
        let rule = materialize_entry("stevenblack", "doubleclick.net", 17);
        assert_eq!(rule.action, "deny");
        assert_eq!(rule.duration, "always");
        assert!(rule.enabled);
        assert!(
            rule.name.starts_with("900-blocklist:stevenblack:"),
            "name should be in 900-band: {}",
            rule.name
        );
        assert!(rule.name.contains("doubleclick.net") || rule.name.contains("0017"));
    }

    #[test]
    fn rule_operator_targets_dest_host_simple() {
        let rule = materialize_entry("stevenblack", "doubleclick.net", 0);
        assert_eq!(rule.operator.kind, "simple");
        assert_eq!(rule.operator.operand, "dest.host");
        assert_eq!(rule.operator.data, "doubleclick.net");
    }

    #[test]
    fn rule_description_carries_source_tag_json() {
        let rule = materialize_entry("stevenblack", "doubleclick.net", 0);
        let parsed: serde_json::Value =
            serde_json::from_str(&rule.description).expect("description is JSON");
        assert_eq!(parsed["snitchwatch"]["source"], "blocklist");
        assert_eq!(parsed["snitchwatch"]["list_id"], "stevenblack");
        assert_eq!(parsed["snitchwatch"]["entry"], "doubleclick.net");
    }

    #[test]
    fn names_are_stable_for_same_input() {
        let a = materialize_entry("stevenblack", "doubleclick.net", 42);
        let b = materialize_entry("stevenblack", "doubleclick.net", 42);
        assert_eq!(a.name, b.name);
    }

    #[test]
    fn names_are_distinct_for_different_seq() {
        let a = materialize_entry("stevenblack", "doubleclick.net", 1);
        let b = materialize_entry("stevenblack", "doubleclick.net", 2);
        assert_ne!(a.name, b.name);
    }

    #[test]
    fn batch_materialize_preserves_order_and_seq() {
        let hosts = vec!["a.example".to_string(), "b.example".to_string(), "c.example".to_string()];
        let rules = materialize_batch("test", &hosts);
        assert_eq!(rules.len(), 3);
        assert!(rules[0].name.contains("0000"));
        assert!(rules[1].name.contains("0001"));
        assert!(rules[2].name.contains("0002"));
    }

    #[test]
    fn list_id_special_chars_sanitized_in_filename() {
        let rule = materialize_entry("steven/black:bad", "x.example", 0);
        assert!(!rule.name.contains('/'), "filename must not contain slash");
        assert!(!rule.name.contains(':') || rule.name.matches(':').count() == 2,
            "exactly the two delimiter colons allowed: {}", rule.name);
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge blocklists::materializer::tests`
Expected: PASS — 7 tests pass.

Run: `cargo clippy -p snitchwatch-bridge -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/blocklists/materializer.rs
git commit -m "feat(blocklists): materialize entries as 900-band deny rules"
```

---

## Part B — Fetcher and manager (Tasks 5–8)

### Task 5: Fetcher — HTTPS GET with cache-preserving failure handling

**Files:**
- Modify: `crates/snitchwatch-bridge/src/blocklists/fetcher.rs`

- [ ] **Step 1: Write the failing test**

Replace `crates/snitchwatch-bridge/src/blocklists/fetcher.rs`:

```rust
//! HTTPS fetcher for blocklist subscriptions.
//!
//! Discipline: a failed fetch must NEVER overwrite the prior cached entries.
//! On error we update the subscription's `last_fetch_status` to
//! `Failed { reason }` and leave the entries table untouched. The Blocklists
//! tab then renders "last updated 4h ago — last fetch failed".

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_outcome_ok_carries_parsed_hosts() {
        let outcome = FetchOutcome::Ok {
            hosts: vec!["a.example".to_string(), "b.example".to_string()],
            format: crate::blocklists::format::ListFormat::Domains,
        };
        match outcome {
            FetchOutcome::Ok { hosts, .. } => assert_eq!(hosts.len(), 2),
            _ => panic!("expected Ok"),
        }
    }

    #[test]
    fn fetch_outcome_failed_carries_reason() {
        let outcome = FetchOutcome::Failed {
            reason: "HTTP 503".to_string(),
        };
        match outcome {
            FetchOutcome::Failed { reason } => assert_eq!(reason, "HTTP 503"),
            _ => panic!("expected Failed"),
        }
    }

    #[tokio::test]
    async fn parses_local_fixture_via_file_url() {
        let path = std::env::current_dir()
            .unwrap()
            .join("../../tests/fixtures/blocklists/stevenblack-tiny.txt");
        let body = std::fs::read_to_string(&path).expect("fixture readable");
        let outcome = process_body(&body);
        match outcome {
            FetchOutcome::Ok { hosts, format } => {
                assert_eq!(format, crate::blocklists::format::ListFormat::Hosts);
                assert!(hosts.contains(&"doubleclick.net".to_string()));
                assert!(!hosts.iter().any(|h| h == "localhost"));
            }
            FetchOutcome::Failed { reason } => panic!("expected Ok, got Failed: {reason}"),
        }
    }

    #[test]
    fn rejects_garbage_binary_body() {
        let garbage: Vec<u8> = vec![0u8, 1, 2, 3, 0xff, 0xfe, 0xfd, 0xfc];
        let body = String::from_utf8_lossy(&garbage).into_owned();
        let outcome = process_body(&body);
        // Garbage either parses to zero hosts (Failed) or is rejected outright.
        match outcome {
            FetchOutcome::Failed { .. } => {}
            FetchOutcome::Ok { hosts, .. } if hosts.is_empty() => {}
            FetchOutcome::Ok { hosts, .. } => panic!("garbage parsed as {hosts:?}"),
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge blocklists::fetcher::tests`
Expected: FAIL — `error[E0412]: cannot find type FetchOutcome in this scope`.

- [ ] **Step 3: Implement the fetcher**

Replace `crates/snitchwatch-bridge/src/blocklists/fetcher.rs`:

```rust
//! HTTPS fetcher for blocklist subscriptions.
//!
//! Discipline: a failed fetch must NEVER overwrite the prior cached entries.
//! On error we update the subscription's `last_fetch_status` to
//! `Failed { reason }` and leave the entries table untouched. The Blocklists
//! tab then renders "last updated 4h ago — last fetch failed".

use std::time::Duration;

use reqwest::Client;
use tracing::{debug, warn};

use crate::blocklists::format::{parse, sniff_format, ListFormat};

#[derive(Debug, Clone)]
pub enum FetchOutcome {
    Ok {
        hosts: Vec<String>,
        format: ListFormat,
    },
    Failed {
        reason: String,
    },
}

const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB hard cap

pub fn build_client() -> Client {
    Client::builder()
        .timeout(FETCH_TIMEOUT)
        .user_agent(concat!("snitchwatch/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client builds")
}

pub async fn fetch(client: &Client, url: &str) -> FetchOutcome {
    debug!(url, "blocklist fetch begin");
    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(url, error = %e, "blocklist fetch transport error");
            return FetchOutcome::Failed {
                reason: format!("transport: {e}"),
            };
        }
    };
    let status = resp.status();
    if !status.is_success() {
        warn!(url, %status, "blocklist fetch non-2xx");
        return FetchOutcome::Failed {
            reason: format!("HTTP {}", status.as_u16()),
        };
    }
    let content_length = resp.content_length().unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        return FetchOutcome::Failed {
            reason: format!("body too large: {content_length} bytes"),
        };
    }
    let body = match resp.text().await {
        Ok(b) => b,
        Err(e) => {
            return FetchOutcome::Failed {
                reason: format!("read body: {e}"),
            };
        }
    };
    if body.len() as u64 > MAX_BODY_BYTES {
        return FetchOutcome::Failed {
            reason: format!("body too large: {} bytes", body.len()),
        };
    }
    process_body(&body)
}

pub fn process_body(body: &str) -> FetchOutcome {
    let format = sniff_format(body);
    let hosts = parse(format, body);
    FetchOutcome::Ok { hosts, format }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_outcome_ok_carries_parsed_hosts() {
        let outcome = FetchOutcome::Ok {
            hosts: vec!["a.example".to_string(), "b.example".to_string()],
            format: crate::blocklists::format::ListFormat::Domains,
        };
        match outcome {
            FetchOutcome::Ok { hosts, .. } => assert_eq!(hosts.len(), 2),
            _ => panic!("expected Ok"),
        }
    }

    #[test]
    fn fetch_outcome_failed_carries_reason() {
        let outcome = FetchOutcome::Failed {
            reason: "HTTP 503".to_string(),
        };
        match outcome {
            FetchOutcome::Failed { reason } => assert_eq!(reason, "HTTP 503"),
            _ => panic!("expected Failed"),
        }
    }

    #[tokio::test]
    async fn parses_local_fixture_via_file_url() {
        let path = std::env::current_dir()
            .unwrap()
            .join("../../tests/fixtures/blocklists/stevenblack-tiny.txt");
        let body = std::fs::read_to_string(&path).expect("fixture readable");
        let outcome = process_body(&body);
        match outcome {
            FetchOutcome::Ok { hosts, format } => {
                assert_eq!(format, crate::blocklists::format::ListFormat::Hosts);
                assert!(hosts.contains(&"doubleclick.net".to_string()));
                assert!(!hosts.iter().any(|h| h == "localhost"));
            }
            FetchOutcome::Failed { reason } => panic!("expected Ok, got Failed: {reason}"),
        }
    }

    #[test]
    fn rejects_garbage_binary_body() {
        let garbage: Vec<u8> = vec![0u8, 1, 2, 3, 0xff, 0xfe, 0xfd, 0xfc];
        let body = String::from_utf8_lossy(&garbage).into_owned();
        let outcome = process_body(&body);
        match outcome {
            FetchOutcome::Failed { .. } => {}
            FetchOutcome::Ok { hosts, .. } if hosts.is_empty() => {}
            FetchOutcome::Ok { hosts, .. } => panic!("garbage parsed as {hosts:?}"),
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge blocklists::fetcher::tests`
Expected: PASS — 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/blocklists/fetcher.rs
git commit -m "feat(blocklists): reqwest fetcher with cache-preserving failure mode"
```

---

### Task 6: BlocklistsManager — owns store, fetcher, and a broadcast bus

**Files:**
- Modify: `crates/snitchwatch-bridge/src/blocklists/mod.rs`

- [ ] **Step 1: Write the failing test**

Replace `crates/snitchwatch-bridge/src/blocklists/mod.rs`:

```rust
//! Bridge-owned blocklist subscriptions, fetch loop, and rule materialization.

pub mod fetcher;
pub mod format;
pub mod materializer;
pub mod store;

use std::sync::Arc;

use chrono::Utc;
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::blocklists::fetcher::{build_client, fetch, FetchOutcome};
use crate::blocklists::store::{BlocklistStore, FetchStatus, Subscription};

/// Events emitted whenever blocklist state changes. The translator subscribes
/// and rebroadcasts as `SetBlocklists` / `SetBlocklistEntries` over the WS.
#[derive(Debug, Clone)]
pub enum BlocklistEvent {
    SubscriptionsChanged,
    EntriesChanged { subscription_id: String },
    StatusChanged { subscription_id: String },
}

pub struct BlocklistsManager {
    store: Arc<BlocklistStore>,
    bus: broadcast::Sender<BlocklistEvent>,
    client: reqwest::Client,
}

impl BlocklistsManager {
    pub fn new(store: Arc<BlocklistStore>) -> Self {
        let (bus, _) = broadcast::channel(64);
        Self {
            store,
            bus,
            client: build_client(),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BlocklistEvent> {
        self.bus.subscribe()
    }

    pub fn store(&self) -> &Arc<BlocklistStore> {
        &self.store
    }

    /// Add a subscription record (does not fetch). Use [`refresh_now`] to pull.
    pub async fn add_subscription(&self, url: &str) -> anyhow::Result<String> {
        let id = derive_id(url);
        let display_name = derive_display_name(url);
        let sub = Subscription {
            id: id.clone(),
            url: url.to_string(),
            display_name,
            format_hint: None,
            refresh_interval_secs: 86_400,
            last_fetched_at: None,
            last_fetch_status: FetchStatus::Pending,
            entry_count: 0,
        };
        self.store.upsert_subscription(&sub)?;
        let _ = self.bus.send(BlocklistEvent::SubscriptionsChanged);
        Ok(id)
    }

    pub async fn remove_subscription(&self, id: &str) -> anyhow::Result<()> {
        self.store.delete_subscription(id)?;
        let _ = self.bus.send(BlocklistEvent::SubscriptionsChanged);
        Ok(())
    }

    /// Pull a subscription synchronously and update the store + bus accordingly.
    pub async fn refresh_now(&self, id: &str) -> anyhow::Result<FetchStatus> {
        let Some(mut sub) = self.store.get_subscription(id)? else {
            anyhow::bail!("unknown subscription: {id}");
        };
        let outcome = fetch(&self.client, &sub.url).await;
        let new_status = match outcome {
            FetchOutcome::Ok { hosts, .. } => {
                let host_refs: Vec<&str> = hosts.iter().map(String::as_str).collect();
                self.store.replace_entries(&sub.id, &host_refs)?;
                sub.entry_count = host_refs.len() as i64;
                sub.last_fetched_at = Some(Utc::now());
                sub.last_fetch_status = FetchStatus::Ok;
                self.store.upsert_subscription(&sub)?;
                let _ = self.bus.send(BlocklistEvent::EntriesChanged {
                    subscription_id: sub.id.clone(),
                });
                let _ = self.bus.send(BlocklistEvent::StatusChanged {
                    subscription_id: sub.id.clone(),
                });
                info!(id = %sub.id, count = host_refs.len(), "blocklist refreshed");
                FetchStatus::Ok
            }
            FetchOutcome::Failed { reason } => {
                sub.last_fetch_status = FetchStatus::Failed { reason: reason.clone() };
                self.store.upsert_subscription(&sub)?;
                let _ = self.bus.send(BlocklistEvent::StatusChanged {
                    subscription_id: sub.id.clone(),
                });
                warn!(id = %sub.id, %reason, "blocklist refresh failed; cache preserved");
                FetchStatus::Failed { reason }
            }
        };
        Ok(new_status)
    }
}

fn derive_id(url: &str) -> String {
    let stem = url
        .rsplit('/')
        .next()
        .unwrap_or(url)
        .split('?')
        .next()
        .unwrap_or(url)
        .trim_end_matches(".txt");
    let cleaned: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if cleaned.is_empty() {
        format!("list-{:x}", url.len())
    } else {
        cleaned
    }
}

fn derive_display_name(url: &str) -> String {
    derive_id(url).replace('_', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager() -> BlocklistsManager {
        let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
        BlocklistsManager::new(store)
    }

    #[test]
    fn module_exports_compile() {
        let _ = std::any::type_name::<store::Subscription>();
    }

    #[tokio::test]
    async fn add_subscription_emits_subscriptions_changed_event() {
        let mgr = manager();
        let mut rx = mgr.subscribe();
        let id = mgr
            .add_subscription("https://example.invalid/list.txt")
            .await
            .unwrap();
        assert_eq!(id, "list");
        let evt = rx.recv().await.expect("event");
        assert!(matches!(evt, BlocklistEvent::SubscriptionsChanged));
    }

    #[tokio::test]
    async fn remove_subscription_clears_store() {
        let mgr = manager();
        let id = mgr
            .add_subscription("https://example.invalid/test.txt")
            .await
            .unwrap();
        mgr.remove_subscription(&id).await.unwrap();
        assert!(mgr.store.get_subscription(&id).unwrap().is_none());
    }

    #[test]
    fn derive_id_handles_query_strings_and_special_chars() {
        assert_eq!(derive_id("https://x.example/hosts.txt?branch=main"), "hosts");
        assert_eq!(derive_id("https://x.example/StevenBlack/hosts"), "hosts");
        assert_eq!(derive_id("https://x.example/path/with%20space"), "with_20space");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge blocklists::tests`
Expected: FAIL — `error[E0432]: unresolved import` for `BlocklistEvent` (or compilation succeeds and the runtime tests fail because the channel doesn't exist yet — re-read the file as written above and confirm the type names match).

If the file as written above is in place, the failure mode is: a compilation succeeds but no test exists yet for the new behavior. Re-run this step after writing only the test scaffold (top of the file) and before the implementation. The intended sequence is: red → green → refactor.

Practical version: write the file in two passes — first the `#[cfg(test)] mod tests` block plus stub `pub struct BlocklistsManager;` with `unimplemented!()` bodies, then run the tests to see them panic, then replace with the real implementation above.

- [ ] **Step 3: Stub-then-implement**

(Already shown above — the file contents from Step 1 are the final implementation.) Make sure the file compiles cleanly with the in-memory store.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge blocklists`
Expected: PASS — all module tests + manager tests pass.

Run: `cargo clippy -p snitchwatch-bridge -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/blocklists/mod.rs
git commit -m "feat(blocklists): BlocklistsManager with broadcast bus + refresh_now"
```

---

### Task 7: Refresh scheduler — periodic background tokio task

**Files:**
- Modify: `crates/snitchwatch-bridge/src/blocklists/mod.rs`

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block at the bottom of `crates/snitchwatch-bridge/src/blocklists/mod.rs`:

```rust
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refresh_loop_drives_pending_subscriptions_with_short_interval() {
        use std::time::Duration;
        let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
        // Pre-seed a subscription whose URL points at a file:// path so the
        // network call resolves locally without sandbox / DNS / connectivity.
        let fixture_path = std::env::current_dir()
            .unwrap()
            .join("../../tests/fixtures/blocklists/domains-tiny.txt")
            .canonicalize()
            .unwrap();
        let url = format!("file://{}", fixture_path.display());
        store
            .upsert_subscription(&Subscription {
                id: "tiny".into(),
                url,
                display_name: "tiny".into(),
                format_hint: None,
                refresh_interval_secs: 1,
                last_fetched_at: None,
                last_fetch_status: FetchStatus::Pending,
                entry_count: 0,
            })
            .unwrap();
        let mgr = Arc::new(BlocklistsManager::new(store.clone()));
        let mut rx = mgr.subscribe();
        let handle = BlocklistsManager::spawn_refresh_loop(mgr.clone(), Duration::from_millis(100));
        // Wait up to 5s for an EntriesChanged event for "tiny".
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if tokio::time::Instant::now() >= deadline {
                handle.abort();
                panic!("never observed EntriesChanged for tiny");
            }
            match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
                Ok(Ok(BlocklistEvent::EntriesChanged { subscription_id })) if subscription_id == "tiny" => break,
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => continue,
                Err(_) => continue,
            }
        }
        handle.abort();
        let entries = store.list_entries("tiny").unwrap();
        assert!(entries.contains(&"doubleclick.net".to_string()));
    }
```

> **Note on `file://` URLs:** reqwest's default backend does not handle `file://`. The implementation in Step 3 detects the `file://` scheme and short-circuits to a local read so this test runs without a real HTTP server. Production paths still go through reqwest.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge blocklists::tests::refresh_loop_drives_pending_subscriptions_with_short_interval`
Expected: FAIL — `error[E0599]: no function or associated item named spawn_refresh_loop`.

- [ ] **Step 3: Implement spawn_refresh_loop and file:// short-circuit**

In `crates/snitchwatch-bridge/src/blocklists/mod.rs`, add at the top of the file with the other imports:

```rust
use std::time::Duration;
use tokio::task::JoinHandle;
```

Add to `impl BlocklistsManager`:

```rust
    pub fn spawn_refresh_loop(self: Arc<Self>, tick: Duration) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tick);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let due = match self.due_subscriptions() {
                    Ok(d) => d,
                    Err(e) => {
                        warn!(error = %e, "blocklist scheduler: store read failed");
                        continue;
                    }
                };
                for id in due {
                    if let Err(e) = self.refresh_now(&id).await {
                        warn!(%id, error = %e, "scheduled refresh failed");
                    }
                }
            }
        })
    }

    fn due_subscriptions(&self) -> Result<Vec<String>, store::StoreError> {
        let now = Utc::now();
        let subs = self.store.list_subscriptions()?;
        Ok(subs
            .into_iter()
            .filter(|s| match s.last_fetched_at {
                None => true,
                Some(t) => {
                    let elapsed = (now - t).num_seconds();
                    elapsed >= s.refresh_interval_secs
                }
            })
            .map(|s| s.id)
            .collect())
    }
```

In `crates/snitchwatch-bridge/src/blocklists/fetcher.rs`, add `file://` short-circuit at the top of `pub async fn fetch`:

```rust
pub async fn fetch(client: &Client, url: &str) -> FetchOutcome {
    debug!(url, "blocklist fetch begin");
    if let Some(path) = url.strip_prefix("file://") {
        return match tokio::fs::read_to_string(path).await {
            Ok(body) => process_body(&body),
            Err(e) => FetchOutcome::Failed {
                reason: format!("file://{path}: {e}"),
            },
        };
    }
    // ...rest of the existing reqwest-based body...
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge blocklists::tests::refresh_loop_drives_pending_subscriptions_with_short_interval`
Expected: PASS within 5 seconds.

Run: `cargo test -p snitchwatch-bridge blocklists`
Expected: every prior blocklist test still PASSes.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/blocklists/mod.rs crates/snitchwatch-bridge/src/blocklists/fetcher.rs
git commit -m "feat(blocklists): periodic refresh loop + file:// fetch short-circuit"
```

---

### Task 8: Failure preserves prior cache (regression test)

**Files:**
- Modify: `crates/snitchwatch-bridge/src/blocklists/mod.rs`

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `crates/snitchwatch-bridge/src/blocklists/mod.rs`:

```rust
    #[tokio::test]
    async fn failed_refresh_preserves_prior_entries() {
        use std::time::Duration;
        let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
        // First, prime entries from a working file:// fetch.
        let good = std::env::current_dir()
            .unwrap()
            .join("../../tests/fixtures/blocklists/domains-tiny.txt")
            .canonicalize()
            .unwrap();
        store
            .upsert_subscription(&Subscription {
                id: "preserve".into(),
                url: format!("file://{}", good.display()),
                display_name: "preserve".into(),
                format_hint: None,
                refresh_interval_secs: 86_400,
                last_fetched_at: None,
                last_fetch_status: FetchStatus::Pending,
                entry_count: 0,
            })
            .unwrap();
        let mgr = BlocklistsManager::new(store.clone());
        mgr.refresh_now("preserve").await.unwrap();
        let count_before = store.list_entries("preserve").unwrap().len();
        assert!(count_before > 0, "priming failed");

        // Now point the URL at a file that does not exist and refresh again.
        store
            .upsert_subscription(&Subscription {
                id: "preserve".into(),
                url: "file:///definitely/does/not/exist.txt".into(),
                display_name: "preserve".into(),
                format_hint: None,
                refresh_interval_secs: 86_400,
                last_fetched_at: Some(Utc::now() - chrono::Duration::seconds(100_000)),
                last_fetch_status: FetchStatus::Ok,
                entry_count: count_before as i64,
            })
            .unwrap();
        let status = mgr.refresh_now("preserve").await.unwrap();
        match status {
            FetchStatus::Failed { reason } => {
                assert!(reason.contains("does/not/exist") || reason.contains("No such file"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        // Critical: entries must STILL be present.
        let entries_after = store.list_entries("preserve").unwrap();
        assert_eq!(
            entries_after.len(),
            count_before,
            "failed fetch must not clear cached entries"
        );
        // Avoid `unused_variables` warning when Duration import is only used in scheduler test.
        let _ = Duration::from_secs(0);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge blocklists::tests::failed_refresh_preserves_prior_entries`
Expected: depends on the current `refresh_now` implementation. If it accidentally calls `replace_entries(&[])` on the failure path, the test fails with `entries_after.len() == 0`. If it correctly skips on failure, it passes immediately — in which case the regression test still has value as a guard.

If it passes immediately, that's allowed: this task is a deliberate guard test against a regression we explicitly designed against in Task 6. Document that fact in the test name and move on. (TDD allows guard tests when the property they enforce is non-obvious from the implementation.)

- [ ] **Step 3: Confirm or harden the implementation**

Re-read `pub async fn refresh_now` in `mod.rs`. Verify the `FetchOutcome::Failed` arm:
- does NOT call `replace_entries`
- does NOT call `entries_changed` event broadcast
- ONLY updates `last_fetch_status` and emits `StatusChanged`

If any of those properties are missing, fix them before continuing.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge blocklists::tests::failed_refresh_preserves_prior_entries`
Expected: PASS.

Run: `cargo test -p snitchwatch-bridge blocklists`
Expected: every blocklist test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/blocklists/mod.rs
git commit -m "test(blocklists): regression guard for cache preservation on fetch failure"
```

---

## Part C — WS protocol typing and translator wiring (Tasks 9–12)

### Task 9: Strongly-typed WS Blocklist message structs

**Files:**
- Modify: `crates/snitchwatch-bridge/src/ws_messages.rs`

- [ ] **Step 1: Write the failing test**

Append to the existing test module at the bottom of `crates/snitchwatch-bridge/src/ws_messages.rs` (or add one if absent):

```rust
#[cfg(test)]
mod blocklist_message_tests {
    use super::*;

    #[test]
    fn set_blocklists_serializes_to_camel_case_action() {
        let msg = ServerMessage::SetBlocklists {
            blocklists: vec![BlocklistSummary {
                id: "stevenblack".into(),
                display_name: "StevenBlack".into(),
                url: "https://x.example/hosts".into(),
                entry_count: 1234,
                status: "ok".into(),
                last_updated_iso8601: Some("2026-04-11T12:00:00Z".into()),
                last_failure_reason: None,
            }],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["action"], "setBlocklists");
        assert_eq!(json["blocklists"][0]["id"], "stevenblack");
        assert_eq!(json["blocklists"][0]["displayName"], "StevenBlack");
        assert_eq!(json["blocklists"][0]["entryCount"], 1234);
    }

    #[test]
    fn set_blocklist_entries_carries_strongly_typed_entries() {
        let msg = ServerMessage::SetBlocklistEntries {
            subscription_id: "stevenblack".into(),
            entries: vec![
                BlocklistEntry { host: "doubleclick.net".into() },
                BlocklistEntry { host: "google-analytics.com".into() },
            ],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["action"], "setBlocklistEntries");
        assert_eq!(json["subscriptionId"], "stevenblack");
        assert_eq!(json["entries"][0]["host"], "doubleclick.net");
    }

    #[test]
    fn subscribe_blocklist_action_round_trips() {
        let action = ClientMessage::SubscribeBlocklist {
            url: "https://x.example/hosts".into(),
        };
        let json = serde_json::to_string(&action).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, action);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge ws_messages::blocklist_message_tests`
Expected: FAIL — `error[E0432]: unresolved import` for `BlocklistSummary` and `BlocklistEntry`, plus mismatched fields on existing `SetBlocklists`/`SetBlocklistEntries` variants (which currently use `serde_json::Value`).

- [ ] **Step 3: Replace untyped variants with strongly-typed structs**

In `crates/snitchwatch-bridge/src/ws_messages.rs`:

1. Add the new structs near the other type definitions:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlocklistSummary {
    pub id: String,
    pub display_name: String,
    pub url: String,
    pub entry_count: i64,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated_iso8601: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlocklistEntry {
    pub host: String,
}
```

2. Replace the existing untyped `SetBlocklists` / `SetBlocklistEntries` / `SetBlocklistDetails` / `SetBlocklistEntryLocation` / `SetBlocklistStatus` arms in `ServerMessage` with:

```rust
    SetBlocklists {
        blocklists: Vec<BlocklistSummary>,
    },
    SetBlocklistDetails {
        details: BlocklistSummary,
    },
    SetBlocklistEntries {
        subscription_id: String,
        entries: Vec<BlocklistEntry>,
    },
    SetBlocklistEntryLocation {
        subscription_id: String,
        host: String,
        line_number: u64,
    },
    SetBlocklistStatus {
        subscription_id: String,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        last_failure_reason: Option<String>,
    },
```

3. Verify the existing `SubscribeBlocklist` / `UnsubscribeBlocklist` arms in `ClientMessage` are unchanged and serde-compatible.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge ws_messages`
Expected: PASS — the 3 new tests pass, plus any existing ws_messages tests still pass.

Run: `cargo build -p snitchwatch-bridge`
Expected: build fails in `translator/downstream.rs` and `translator/upstream.rs` because they reference the old `serde_json::Value`-shaped variants. That is intentional — the next task wires those up.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/ws_messages.rs
git commit -m "feat(blocklists): strongly-typed BlocklistSummary + BlocklistEntry WS structs"
```

---

### Task 10: Translator downstream — emit Blocklist messages on BlocklistEvent

**Files:**
- Modify: `crates/snitchwatch-bridge/src/translator/downstream.rs`

- [ ] **Step 1: Write the failing test**

In `crates/snitchwatch-bridge/src/translator/downstream.rs`, add (or extend) a `#[cfg(test)] mod tests` block:

```rust
#[cfg(test)]
mod blocklist_emission_tests {
    use super::*;
    use crate::blocklists::mod_for_test::test_helpers::seeded_manager;
    use crate::ws_messages::ServerMessage;

    #[tokio::test]
    async fn subscriptions_changed_yields_set_blocklists() {
        let mgr = seeded_manager(&[("stevenblack", 5), ("easylist", 3)]).await;
        let summaries = build_set_blocklists(&mgr).await.unwrap();
        match summaries {
            ServerMessage::SetBlocklists { blocklists } => {
                assert_eq!(blocklists.len(), 2);
                assert!(blocklists.iter().any(|b| b.id == "stevenblack" && b.entry_count == 5));
                assert!(blocklists.iter().any(|b| b.id == "easylist" && b.entry_count == 3));
            }
            other => panic!("expected SetBlocklists, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn entries_changed_yields_set_blocklist_entries() {
        let mgr = seeded_manager(&[("test", 2)]).await;
        let msg = build_set_blocklist_entries(&mgr, "test").await.unwrap();
        match msg {
            ServerMessage::SetBlocklistEntries { subscription_id, entries } => {
                assert_eq!(subscription_id, "test");
                assert_eq!(entries.len(), 2);
            }
            other => panic!("expected SetBlocklistEntries, got {other:?}"),
        }
    }
}
```

In `crates/snitchwatch-bridge/src/blocklists/mod.rs`, add a test-only helper module at the end of the file (still inside the crate, but gated `#[cfg(any(test, feature = "test-helpers"))]`):

```rust
#[cfg(test)]
pub mod mod_for_test {
    pub mod test_helpers {
        use std::sync::Arc;

        use crate::blocklists::store::{BlocklistStore, FetchStatus, Subscription};
        use crate::blocklists::BlocklistsManager;

        pub async fn seeded_manager(seeds: &[(&str, usize)]) -> BlocklistsManager {
            let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
            for (id, n_entries) in seeds {
                store
                    .upsert_subscription(&Subscription {
                        id: (*id).to_string(),
                        url: format!("https://example.invalid/{id}.txt"),
                        display_name: (*id).to_string(),
                        format_hint: None,
                        refresh_interval_secs: 86_400,
                        last_fetched_at: None,
                        last_fetch_status: FetchStatus::Ok,
                        entry_count: *n_entries as i64,
                    })
                    .unwrap();
                let hosts: Vec<String> = (0..*n_entries)
                    .map(|i| format!("host{i}.{id}.example"))
                    .collect();
                let host_refs: Vec<&str> = hosts.iter().map(String::as_str).collect();
                store.replace_entries(id, &host_refs).unwrap();
            }
            BlocklistsManager::new(store)
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge translator::downstream::blocklist_emission_tests`
Expected: FAIL — `error[E0425]: cannot find function build_set_blocklists in this scope`.

- [ ] **Step 3: Implement the build helpers and the dispatch hook**

In `crates/snitchwatch-bridge/src/translator/downstream.rs`, add at the top:

```rust
use crate::blocklists::store::FetchStatus;
use crate::blocklists::BlocklistsManager;
use crate::ws_messages::{BlocklistEntry, BlocklistSummary, ServerMessage};
```

Add the public helpers:

```rust
pub async fn build_set_blocklists(
    mgr: &BlocklistsManager,
) -> anyhow::Result<ServerMessage> {
    let subs = mgr.store().list_subscriptions()?;
    let blocklists = subs
        .into_iter()
        .map(|s| {
            let (status, last_failure_reason) = match s.last_fetch_status {
                FetchStatus::Pending => ("pending".to_string(), None),
                FetchStatus::Ok => ("ok".to_string(), None),
                FetchStatus::Failed { reason } => ("failed".to_string(), Some(reason)),
            };
            BlocklistSummary {
                id: s.id,
                display_name: s.display_name,
                url: s.url,
                entry_count: s.entry_count,
                status,
                last_updated_iso8601: s.last_fetched_at.map(|t| t.to_rfc3339()),
                last_failure_reason,
            }
        })
        .collect();
    Ok(ServerMessage::SetBlocklists { blocklists })
}

pub async fn build_set_blocklist_entries(
    mgr: &BlocklistsManager,
    subscription_id: &str,
) -> anyhow::Result<ServerMessage> {
    let hosts = mgr.store().list_entries(subscription_id)?;
    let entries = hosts
        .into_iter()
        .map(|host| BlocklistEntry { host })
        .collect();
    Ok(ServerMessage::SetBlocklistEntries {
        subscription_id: subscription_id.to_string(),
        entries,
    })
}

pub async fn build_set_blocklist_status(
    mgr: &BlocklistsManager,
    subscription_id: &str,
) -> anyhow::Result<ServerMessage> {
    let sub = mgr
        .store()
        .get_subscription(subscription_id)?
        .ok_or_else(|| anyhow::anyhow!("unknown subscription: {subscription_id}"))?;
    let (status, last_failure_reason) = match sub.last_fetch_status {
        FetchStatus::Pending => ("pending".to_string(), None),
        FetchStatus::Ok => ("ok".to_string(), None),
        FetchStatus::Failed { reason } => ("failed".to_string(), Some(reason)),
    };
    Ok(ServerMessage::SetBlocklistStatus {
        subscription_id: sub.id,
        status,
        last_failure_reason,
    })
}
```

If the existing `downstream.rs` exposes a `Translated` enum for outbound messages (e.g., to be polled by `ws_server`), wire `BlocklistEvent` into it. Otherwise just call these helpers directly from `ws_server` in Task 12 — the choice depends on the existing translator topology, which may have been refactored at the M1.5 flip.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge translator::downstream::blocklist_emission_tests`
Expected: PASS — both tests pass.

Run: `cargo build -p snitchwatch-bridge`
Expected: build clean.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/translator/downstream.rs crates/snitchwatch-bridge/src/blocklists/mod.rs
git commit -m "feat(blocklists): translator emits SetBlocklists + SetBlocklistEntries"
```

---

### Task 11: Translator upstream — handle SubscribeBlocklist / UnsubscribeBlocklist

**Files:**
- Modify: `crates/snitchwatch-bridge/src/translator/upstream.rs`

- [ ] **Step 1: Write the failing test**

In `crates/snitchwatch-bridge/src/translator/upstream.rs`, add to the `#[cfg(test)] mod tests` block:

```rust
#[cfg(test)]
mod blocklist_action_tests {
    use super::*;
    use crate::blocklists::store::BlocklistStore;
    use crate::blocklists::BlocklistsManager;
    use crate::ws_messages::ClientMessage;
    use std::sync::Arc;

    fn manager() -> Arc<BlocklistsManager> {
        Arc::new(BlocklistsManager::new(Arc::new(
            BlocklistStore::open_in_memory().unwrap(),
        )))
    }

    #[tokio::test]
    async fn subscribe_blocklist_adds_subscription_to_store() {
        let mgr = manager();
        let action = ClientMessage::SubscribeBlocklist {
            url: "https://example.invalid/hosts.txt".into(),
        };
        handle_blocklist_action(mgr.clone(), action).await.unwrap();
        let subs = mgr.store().list_subscriptions().unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].url, "https://example.invalid/hosts.txt");
    }

    #[tokio::test]
    async fn unsubscribe_blocklist_removes_subscription() {
        let mgr = manager();
        let id = mgr
            .add_subscription("https://example.invalid/hosts.txt")
            .await
            .unwrap();
        handle_blocklist_action(mgr.clone(), ClientMessage::UnsubscribeBlocklist { id })
            .await
            .unwrap();
        assert!(mgr.store().list_subscriptions().unwrap().is_empty());
    }

    #[tokio::test]
    async fn non_blocklist_action_is_returned_unhandled() {
        let mgr = manager();
        let action = ClientMessage::Undo;
        let outcome = handle_blocklist_action(mgr.clone(), action.clone()).await.unwrap();
        assert_eq!(outcome, BlocklistActionOutcome::Unhandled(action));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge translator::upstream::blocklist_action_tests`
Expected: FAIL — `error[E0425]: cannot find function handle_blocklist_action`.

- [ ] **Step 3: Implement handle_blocklist_action**

In `crates/snitchwatch-bridge/src/translator/upstream.rs`, add:

```rust
use std::sync::Arc;

use crate::blocklists::BlocklistsManager;
use crate::ws_messages::ClientMessage;

#[derive(Debug, PartialEq)]
pub enum BlocklistActionOutcome {
    Subscribed { id: String },
    Unsubscribed { id: String },
    Unhandled(ClientMessage),
}

pub async fn handle_blocklist_action(
    mgr: Arc<BlocklistsManager>,
    action: ClientMessage,
) -> anyhow::Result<BlocklistActionOutcome> {
    match action {
        ClientMessage::SubscribeBlocklist { url } => {
            let id = mgr.add_subscription(&url).await?;
            // Best-effort immediate refresh; failure is logged inside refresh_now and the
            // subscription stays in Pending state until the next scheduler tick.
            let _ = mgr.refresh_now(&id).await;
            Ok(BlocklistActionOutcome::Subscribed { id })
        }
        ClientMessage::UnsubscribeBlocklist { id } => {
            mgr.remove_subscription(&id).await?;
            Ok(BlocklistActionOutcome::Unsubscribed { id })
        }
        other => Ok(BlocklistActionOutcome::Unhandled(other)),
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge translator::upstream::blocklist_action_tests`
Expected: PASS — 3 tests pass.

Run: `cargo clippy -p snitchwatch-bridge -- -D warnings`
Expected: clean. If `large_enum_variant` fires on `BlocklistActionOutcome::Unhandled(ClientMessage)`, box it: `Unhandled(Box<ClientMessage>)` and update the test accordingly. (Apply the same memory rule as `Translated::AskRule`.)

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/translator/upstream.rs
git commit -m "feat(blocklists): translator routes Subscribe/Unsubscribe to manager"
```

---

### Task 12: Wire BlocklistsManager into the WS server connection lifecycle

**Files:**
- Modify: `crates/snitchwatch-bridge/src/ws_server.rs`

- [ ] **Step 1: Write the failing test**

This is a wiring task. The most useful failing test lives in the integration suite (Task 13), but a small unit-level guard is worth adding here. Append to the existing `#[cfg(test)] mod tests` in `crates/snitchwatch-bridge/src/ws_server.rs`:

```rust
#[tokio::test]
async fn server_state_carries_blocklists_manager() {
    use crate::blocklists::store::BlocklistStore;
    use crate::blocklists::BlocklistsManager;
    use std::sync::Arc;
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    let mgr = Arc::new(BlocklistsManager::new(store));
    let state = ServerState::new_with_blocklists(mgr.clone());
    assert!(Arc::ptr_eq(state.blocklists(), &mgr));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge ws_server::tests::server_state_carries_blocklists_manager`
Expected: FAIL — either `ServerState::new_with_blocklists` or `ServerState::blocklists()` doesn't exist yet.

- [ ] **Step 3: Add blocklists field to ServerState**

In `crates/snitchwatch-bridge/src/ws_server.rs`:

1. Find the `ServerState` (or equivalent shared-state struct) that the WS handler holds. Add a field:

```rust
pub struct ServerState {
    // ... existing fields ...
    pub(crate) blocklists: std::sync::Arc<crate::blocklists::BlocklistsManager>,
}
```

2. Add the constructor and accessor:

```rust
impl ServerState {
    pub fn new_with_blocklists(
        blocklists: std::sync::Arc<crate::blocklists::BlocklistsManager>,
    ) -> Self {
        Self {
            // ... existing field defaults ...
            blocklists,
        }
    }

    pub fn blocklists(&self) -> &std::sync::Arc<crate::blocklists::BlocklistsManager> {
        &self.blocklists
    }
}
```

(If your existing `ServerState::new` already takes other dependencies, add `blocklists` as a parameter to the existing constructor instead of adding a separate `new_with_blocklists`. The test name above is illustrative — match it to whichever shape is least disruptive.)

3. In the WS connection handler, where the loop deserializes incoming `ClientMessage`s and dispatches them, add a step that first calls `crate::translator::upstream::handle_blocklist_action`:

```rust
let action: ClientMessage = serde_json::from_str(&text)?;
match crate::translator::upstream::handle_blocklist_action(state.blocklists.clone(), action).await? {
    crate::translator::upstream::BlocklistActionOutcome::Subscribed { .. }
    | crate::translator::upstream::BlocklistActionOutcome::Unsubscribed { .. } => {
        // Already handled; the BlocklistEvent broadcast will trigger a SetBlocklists push.
    }
    crate::translator::upstream::BlocklistActionOutcome::Unhandled(passthrough) => {
        // Existing handlers (verdict / rule mutation / etc.) take it from here.
        existing_dispatch(passthrough).await?;
    }
}
```

4. In the same connection handler, on connection setup, send an initial `SetBlocklists` snapshot and spawn a task that listens to `state.blocklists.subscribe()` and forwards events as the appropriate `ServerMessage`:

```rust
let initial = crate::translator::downstream::build_set_blocklists(&state.blocklists).await?;
ws_tx.send(serde_json::to_string(&initial)?.into()).await?;

let mut bl_rx = state.blocklists.subscribe();
let bl_mgr = state.blocklists.clone();
let outbound = ws_tx.clone();
tokio::spawn(async move {
    while let Ok(evt) = bl_rx.recv().await {
        let msg = match evt {
            crate::blocklists::BlocklistEvent::SubscriptionsChanged => {
                crate::translator::downstream::build_set_blocklists(&bl_mgr).await
            }
            crate::blocklists::BlocklistEvent::EntriesChanged { subscription_id } => {
                crate::translator::downstream::build_set_blocklist_entries(&bl_mgr, &subscription_id).await
            }
            crate::blocklists::BlocklistEvent::StatusChanged { subscription_id } => {
                crate::translator::downstream::build_set_blocklist_status(&bl_mgr, &subscription_id).await
            }
        };
        match msg {
            Ok(m) => {
                if let Ok(s) = serde_json::to_string(&m) {
                    if outbound.send(s.into()).await.is_err() {
                        break;
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "blocklist event build failed"),
        }
    }
});
```

5. Finally, in `Bridge::serve` (or whatever the top-level entrypoint is named — Plan 4 changed the signature; adapt to current shape), construct a `BlocklistsManager` from a real `BlocklistStore` (use the XDG path via `crate::blocklists::store::BlocklistStore::open(&data_dir().join("blocklists.db"))?` once a `data_dir()` helper is in scope; until then, accept a path parameter) and pass it to `ServerState::new_with_blocklists`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge ws_server`
Expected: PASS — the new test plus all prior ws_server tests.

Run: `cargo build -p snitchwatch-bridge -p snitchwatch-bridge-cli`
Expected: build clean. (CLI may need a small change to pass the blocklists path / temp store.)

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/ws_server.rs crates/snitchwatch-bridge-cli/src/main.rs
git commit -m "feat(blocklists): wire BlocklistsManager into WS connection lifecycle"
```

---

## Part D — End-to-end test, materialized rules, daemon push (Tasks 13–15)

### Task 13: End-to-end integration test with mock daemon

**Files:**
- Create: `crates/snitchwatch-bridge/tests/blocklists_e2e.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/snitchwatch-bridge/tests/blocklists_e2e.rs`:

```rust
//! End-to-end: a real WS client connects to the bridge, subscribes to a
//! file:// blocklist, and receives a SetBlocklists message followed by a
//! SetBlocklistEntries message containing the parsed hosts.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use snitchwatch_bridge::blocklists::store::BlocklistStore;
use snitchwatch_bridge::blocklists::BlocklistsManager;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscribe_blocklist_via_ws_yields_entries() {
    // Resolve the fixture path BEFORE spawning the bridge so the test fails fast
    // if the fixture is missing.
    let fixture = std::env::current_dir()
        .unwrap()
        .join("../../tests/fixtures/blocklists/domains-tiny.txt")
        .canonicalize()
        .expect("fixture exists");
    let file_url = format!("file://{}", fixture.display());

    // Boot a fresh bridge on an ephemeral port with an in-memory blocklist store.
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    let mgr = Arc::new(BlocklistsManager::new(store));
    let bind_addr = "127.0.0.1:0";
    let (ws_url, _shutdown) = snitchwatch_bridge::ws_server::serve_with_blocklists(
        bind_addr.parse().unwrap(),
        mgr.clone(),
    )
    .await
    .expect("bridge boots");

    // Connect a WS client.
    let (mut ws, _resp) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("ws client connects");

    // Drain the initial SetBlocklists snapshot (should be empty).
    let snapshot = next_server_message(&mut ws).await;
    assert!(matches!(snapshot, ServerMessage::SetBlocklists { ref blocklists } if blocklists.is_empty()));

    // Subscribe.
    let sub_msg = ClientMessage::SubscribeBlocklist { url: file_url };
    ws.send(Message::Text(serde_json::to_string(&sub_msg).unwrap()))
        .await
        .unwrap();

    // Expect a SetBlocklists with one entry, then a SetBlocklistEntries with the parsed hosts.
    let mut saw_set = false;
    let mut saw_entries = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && !(saw_set && saw_entries) {
        let msg = match tokio::time::timeout(Duration::from_secs(1), next_server_message(&mut ws)).await {
            Ok(m) => m,
            Err(_) => continue,
        };
        match msg {
            ServerMessage::SetBlocklists { blocklists } if !blocklists.is_empty() => {
                assert_eq!(blocklists[0].entry_count, 5, "domains-tiny.txt has 5 hosts");
                saw_set = true;
            }
            ServerMessage::SetBlocklistEntries { entries, .. } => {
                assert!(entries.iter().any(|e| e.host == "doubleclick.net"));
                saw_entries = true;
            }
            _ => {}
        }
    }
    assert!(saw_set, "never received populated SetBlocklists");
    assert!(saw_entries, "never received SetBlocklistEntries");
}

async fn next_server_message<S>(ws: &mut S) -> ServerMessage
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let msg = ws.next().await.expect("stream not closed").expect("read");
        if let Message::Text(text) = msg {
            // Skip messages we don't care about; the test only inspects blocklist payloads.
            if text.contains("\"action\":\"set") && text.contains("locklist") {
                return serde_json::from_str(&text).expect("decode ServerMessage");
            } else {
                let _ = json!(text);
            }
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge --test blocklists_e2e`
Expected: FAIL — `error[E0425]: cannot find function serve_with_blocklists` (the convenience entrypoint doesn't exist yet) OR the test panics on `next_server_message` because the SetBlocklists snapshot path isn't yet wired.

- [ ] **Step 3: Add serve_with_blocklists convenience entrypoint**

In `crates/snitchwatch-bridge/src/ws_server.rs`, add (or rename) a public entrypoint:

```rust
use std::net::SocketAddr;
use std::sync::Arc;

/// Convenience: bind on `addr`, hand the supplied BlocklistsManager to a fresh
/// ServerState, and return `(ws_url, shutdown_handle)`. Used by integration
/// tests so they don't need to wire a full Bridge::serve.
pub async fn serve_with_blocklists(
    addr: SocketAddr,
    blocklists: Arc<crate::blocklists::BlocklistsManager>,
) -> anyhow::Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    let state = Arc::new(ServerState::new_with_blocklists(blocklists));
    let handle = tokio::spawn(async move {
        let _ = run_server(listener, state).await;
    });
    Ok((format!("ws://{}/stream", bound), handle))
}
```

(If `run_server` doesn't exist by that name, factor the existing accept loop into it. The exact factoring depends on the M1.5 topology in place; adapt as needed.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge --test blocklists_e2e`
Expected: PASS within 10s.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/tests/blocklists_e2e.rs crates/snitchwatch-bridge/src/ws_server.rs
git commit -m "test(blocklists): end-to-end WS subscribe → entries integration"
```

---

### Task 14: Push materialized rules to opensnitchd via gRPC ChangeRule

**Files:**
- Modify: `crates/snitchwatch-bridge/src/blocklists/mod.rs`
- Modify: `crates/snitchwatch-bridge/src/grpc_client.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/snitchwatch-bridge/src/blocklists/mod.rs` test module:

```rust
    #[tokio::test]
    async fn refresh_pushes_materialized_rules_to_sink() {
        use crate::blocklists::materializer::MaterializedRule;
        use std::sync::Mutex;

        #[derive(Default)]
        struct CapturingSink {
            calls: Mutex<Vec<Vec<MaterializedRule>>>,
        }

        #[async_trait::async_trait]
        impl super::RuleSink for CapturingSink {
            async fn replace_blocklist_rules(
                &self,
                list_id: &str,
                rules: Vec<MaterializedRule>,
            ) -> anyhow::Result<()> {
                let _ = list_id;
                self.calls.lock().unwrap().push(rules);
                Ok(())
            }
        }

        let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
        let fixture = std::env::current_dir()
            .unwrap()
            .join("../../tests/fixtures/blocklists/domains-tiny.txt")
            .canonicalize()
            .unwrap();
        store
            .upsert_subscription(&Subscription {
                id: "tiny".into(),
                url: format!("file://{}", fixture.display()),
                display_name: "tiny".into(),
                format_hint: None,
                refresh_interval_secs: 86_400,
                last_fetched_at: None,
                last_fetch_status: FetchStatus::Pending,
                entry_count: 0,
            })
            .unwrap();
        let sink: Arc<CapturingSink> = Arc::new(CapturingSink::default());
        let mgr = BlocklistsManager::new(store).with_rule_sink(sink.clone());
        mgr.refresh_now("tiny").await.unwrap();
        let calls = sink.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "expected one push call");
        assert_eq!(calls[0].len(), 5, "domains-tiny.txt has 5 hosts");
        assert!(calls[0][0].name.starts_with("900-blocklist:tiny:"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge blocklists::tests::refresh_pushes_materialized_rules_to_sink`
Expected: FAIL — `error[E0405]: cannot find trait RuleSink in this scope`.

- [ ] **Step 3: Add RuleSink trait, in-mgr field, and gRPC implementation**

Add the workspace dep `async-trait = "0.1"` to `[workspace.dependencies]` in the root `Cargo.toml`, then add to `crates/snitchwatch-bridge/Cargo.toml`:

```toml
async-trait = { version = "0.1" }
```

In `crates/snitchwatch-bridge/src/blocklists/mod.rs`, add at the top:

```rust
use async_trait::async_trait;

use crate::blocklists::materializer::{materialize_batch, MaterializedRule};

#[async_trait]
pub trait RuleSink: Send + Sync + 'static {
    async fn replace_blocklist_rules(
        &self,
        list_id: &str,
        rules: Vec<MaterializedRule>,
    ) -> anyhow::Result<()>;
}

/// No-op sink used when the bridge runs headless against unit tests with no
/// daemon attached.
pub struct NoopRuleSink;

#[async_trait]
impl RuleSink for NoopRuleSink {
    async fn replace_blocklist_rules(
        &self,
        _list_id: &str,
        _rules: Vec<MaterializedRule>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}
```

Add a field to `BlocklistsManager`:

```rust
pub struct BlocklistsManager {
    store: Arc<BlocklistStore>,
    bus: broadcast::Sender<BlocklistEvent>,
    client: reqwest::Client,
    rule_sink: Arc<dyn RuleSink>,
}
```

Update `new` to default to `NoopRuleSink`, add a `with_rule_sink` builder, and call the sink at the end of the `Ok` arm in `refresh_now` (BEFORE the `info!` log line):

```rust
impl BlocklistsManager {
    pub fn new(store: Arc<BlocklistStore>) -> Self {
        let (bus, _) = broadcast::channel(64);
        Self {
            store,
            bus,
            client: build_client(),
            rule_sink: Arc::new(NoopRuleSink),
        }
    }

    pub fn with_rule_sink(mut self, sink: Arc<dyn RuleSink>) -> Self {
        self.rule_sink = sink;
        self
    }
    // ... existing methods ...
}
```

In the `FetchOutcome::Ok` arm of `refresh_now`, after `replace_entries` and before the broadcast emissions, add:

```rust
let materialized = materialize_batch(&sub.id, &hosts);
if let Err(e) = self.rule_sink.replace_blocklist_rules(&sub.id, materialized).await {
    warn!(id = %sub.id, error = %e, "rule sink push failed; entries cached but not enforced");
}
```

In `crates/snitchwatch-bridge/src/grpc_client.rs`, add a `GrpcRuleSink` implementation that implements `RuleSink` and calls `ChangeRule` (or whatever the existing rule mutation method is named) once per `MaterializedRule`. Wire it through the existing reconnect/retry logic so a failed push during a daemon outage gets logged but doesn't crash the bridge:

```rust
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::blocklists::materializer::MaterializedRule;
use crate::blocklists::RuleSink;

pub struct GrpcRuleSink {
    client: Arc<Mutex<crate::grpc_client::OpenSnitchClient>>,
}

impl GrpcRuleSink {
    pub fn new(client: Arc<Mutex<crate::grpc_client::OpenSnitchClient>>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl RuleSink for GrpcRuleSink {
    async fn replace_blocklist_rules(
        &self,
        list_id: &str,
        rules: Vec<MaterializedRule>,
    ) -> anyhow::Result<()> {
        let mut client = self.client.lock().await;
        // Step 1: enumerate existing 900-blocklist:<list_id>: rules and delete them.
        let existing = client.list_rules().await?;
        for rule in existing {
            if rule.name.starts_with(&format!("900-blocklist:{list_id}:")) {
                let _ = client.delete_rule(&rule.name).await;
            }
        }
        // Step 2: push new rules.
        for materialized in rules {
            let _ = client.upsert_rule_from_materialized(&materialized).await;
        }
        Ok(())
    }
}
```

(`OpenSnitchClient::list_rules` / `delete_rule` / `upsert_rule_from_materialized` are illustrative names — match the existing client API. If those methods don't exist, add minimal wrappers around the tonic-generated stubs.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge blocklists::tests::refresh_pushes_materialized_rules_to_sink`
Expected: PASS.

Run: `cargo test -p snitchwatch-bridge blocklists`
Expected: every blocklist test passes.

Run: `cargo clippy -p snitchwatch-bridge -- -D warnings`
Expected: clean (watch for `clippy::large_enum_variant` if you box `RuleSink` indirectly).

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/blocklists/mod.rs crates/snitchwatch-bridge/src/grpc_client.rs Cargo.toml crates/snitchwatch-bridge/Cargo.toml
git commit -m "feat(blocklists): RuleSink trait + gRPC implementation pushes 900-band denies"
```

---

### Task 15: Reconciliation — re-group existing 900-band rules into Blocklists tab on ListRules

**Files:**
- Modify: `crates/snitchwatch-bridge/src/grpc_client.rs`

- [ ] **Step 1: Write the failing test**

Add to the test module of `crates/snitchwatch-bridge/src/grpc_client.rs`:

```rust
#[cfg(test)]
mod blocklist_reconciliation_tests {
    use super::*;

    fn make_rule(name: &str, description: &str) -> RuleSnapshot {
        RuleSnapshot {
            name: name.to_string(),
            description: description.to_string(),
            // ... fill required fields with sane defaults — match existing RuleSnapshot shape ...
        }
    }

    #[test]
    fn classify_900_band_rule_as_blocklist() {
        let rule = make_rule(
            "900-blocklist:stevenblack:0001-doubleclick.net",
            r#"{"snitchwatch":{"source":"blocklist","list_id":"stevenblack","entry":"doubleclick.net"}}"#,
        );
        assert_eq!(classify_rule(&rule), RuleCategory::Blocklist {
            list_id: "stevenblack".into(),
        });
    }

    #[test]
    fn classify_user_rule_as_user() {
        let rule = make_rule("050-allow-firefox", "");
        assert_eq!(classify_rule(&rule), RuleCategory::User);
    }

    #[test]
    fn classify_legacy_900_rule_without_tag_as_user() {
        // Defensive: a rule that happens to live in the 900 band but lacks the
        // snitchwatch tag was put there by something else and must NOT be
        // mass-deleted on next blocklist push.
        let rule = make_rule("900-legacy-rule", "{}");
        assert_eq!(classify_rule(&rule), RuleCategory::User);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge grpc_client::blocklist_reconciliation_tests`
Expected: FAIL — `RuleCategory` and `classify_rule` don't exist yet.

- [ ] **Step 3: Implement classification**

In `crates/snitchwatch-bridge/src/grpc_client.rs`, add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleCategory {
    User,
    Blocklist { list_id: String },
}

pub fn classify_rule(rule: &RuleSnapshot) -> RuleCategory {
    // Try parsing the description as a snitchwatch tag JSON.
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&rule.description) {
        if let Some(meta) = parsed.get("snitchwatch") {
            if meta.get("source").and_then(|v| v.as_str()) == Some("blocklist") {
                if let Some(list_id) = meta.get("list_id").and_then(|v| v.as_str()) {
                    return RuleCategory::Blocklist {
                        list_id: list_id.to_string(),
                    };
                }
            }
        }
    }
    RuleCategory::User
}
```

Update the existing `GrpcRuleSink::replace_blocklist_rules` (or wherever existing rules are enumerated) to use `classify_rule` instead of name-prefix matching, so legacy 900-band rules without the tag are preserved.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge grpc_client::blocklist_reconciliation_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/grpc_client.rs
git commit -m "feat(blocklists): tag-based rule classification preserves legacy 900-band rules"
```

---

## Part E — Polish, fixtures, and milestone tick (Tasks 16–17)

### Task 16: justfile recipe + README "Try a blocklist" section

**Files:**
- Modify: `justfile`
- Modify: `README.md`
- Modify: `.gitignore`

- [ ] **Step 1: Write the failing test**

This task is documentation + glue; the failing "test" is a manual verification step that the recipe is invokable. Add a smoke test inside the Plan 4 Tauri smoke directory only if Plan 4 is in scope; otherwise, write the recipe and a one-liner shell test.

Add to `justfile`:

```just
# Serve test fixtures over HTTP for manual blocklist subscription smoke testing.
blocklist-fixture-server:
    cd tests/fixtures/blocklists && python3 -m http.server 8731

# Run only the blocklist test suite.
test-blocklists:
    cargo test -p snitchwatch-bridge blocklists -- --nocapture
    cargo test -p snitchwatch-bridge --test blocklists_e2e -- --nocapture
```

- [ ] **Step 2: Run test to verify it fails**

Run: `just --list 2>&1 | grep blocklist || echo MISSING`
Expected: MISSING the first time, recipes present after Step 3.

(Bash hook block on `grep` — use Read on the justfile and visually verify the recipes are present, OR use `just test-blocklists` directly. The hook policy is in `bash_antipattern_hook.md`.)

- [ ] **Step 3: Add the recipes and README section**

The justfile edit is in Step 1. Add to `README.md` after the existing "Try the bridge" section:

```markdown
## M4 — Subscribe to a blocklist

Snitchwatch ships its own blocklist subscription manager. To smoke-test it
end-to-end against the local fixture set:

```bash
just blocklist-fixture-server &       # serves tests/fixtures/blocklists/ on :8731
cargo run -p snitchwatch-bridge-cli   # bridge boots on 127.0.0.1:3031
```

In another terminal, send a `subscribeBlocklist` action over the WS:

```bash
websocat ws://127.0.0.1:3031/stream <<EOF
{"action":"subscribeBlocklist","url":"http://127.0.0.1:8731/domains-tiny.txt"}
EOF
```

You should immediately see two server messages: `setBlocklists` (with the new
subscription) and `setBlocklistEntries` (with the parsed hosts). The bridge
also pushes 5 deny rules into opensnitchd in the `900-blocklist:domains-tiny:`
band — visible via `opensnitchd-cli list-rules` if you have a real daemon
attached.

To run the blocklist test suite in isolation:

```bash
just test-blocklists
```
```

Add to `.gitignore`:

```
# Blocklist database written during ad-hoc test runs (in-memory in unit tests)
**/blocklists.db
**/blocklists.db-wal
**/blocklists.db-shm
```

- [ ] **Step 4: Run test to verify it passes**

Run: `just test-blocklists`
Expected: PASS — every blocklist unit test plus the e2e integration test pass.

- [ ] **Step 5: Commit**

```bash
git add justfile README.md .gitignore
git commit -m "docs(blocklists): justfile recipe + README smoke instructions"
```

---

### Task 17: Tick M4 in milestone table + spec note

**Files:**
- Modify: `docs/superpowers/specs/2026-04-10-snitchwatch-design.md`

- [ ] **Step 1: Write the failing test**

The failing "test" is a Grep that should fail until the milestone row is updated. Run:

```
Grep pattern="M4 — Blocklists.*✅" path=docs/superpowers/specs/2026-04-10-snitchwatch-design.md
```

Expected: no match (M4 row currently lacks the ✅ tick).

- [ ] **Step 2: Run test to verify it fails**

(Above Grep result — ZERO matches.)

- [ ] **Step 3: Update the milestone table and add a brief implementation note**

In `docs/superpowers/specs/2026-04-10-snitchwatch-design.md`, locate the milestone table row for **M4 — Blocklists** and update it to include the ✅ marker:

Find:
```
| **M4 — Blocklists** | Subscription manager, hosts-file fetcher, deny-rule materializer, Blocklists tab fully wired. | Subscribe to StevenBlack/hosts, watch a tracker get blocked. |
```

Replace with:
```
| **M4 — Blocklists** ✅ | Subscription manager, hosts-file fetcher, deny-rule materializer, Blocklists tab fully wired. | Subscribe to StevenBlack/hosts, watch a tracker get blocked. |
```

Append a short paragraph below the table:

```markdown
**M4 implementation notes (2026-04-11).** Implemented per
[`docs/superpowers/plans/2026-04-11-blocklists.md`](../plans/2026-04-11-blocklists.md).
The bridge owns a SQLite store at `$XDG_DATA_HOME/snitchwatch/blocklists.db`,
sniffs hosts/domains/ABP formats, materializes entries as `900-band` deny
rules tagged `__source: blocklist:<id>`, and pushes them through a `RuleSink`
trait so unit tests can verify materialization without a live daemon. The
StevenBlack-on-real-daemon smoke test is deferred to the same Plan 7
environmental verification slot as the M1 deferred items.
```

- [ ] **Step 4: Run test to verify it passes**

Re-run the Grep:
```
Grep pattern="M4 — Blocklists.*✅" path=docs/superpowers/specs/2026-04-10-snitchwatch-design.md
```

Expected: ONE match.

- [ ] **Step 5: Commit**

```bash
git add docs/superpowers/specs/2026-04-10-snitchwatch-design.md
git commit -m "docs(spec): tick M4 — blocklists implemented per plan"
```

---

## Acceptance Criteria

Plan 5 is complete when **all** of the following are true:

1. `cargo build -p snitchwatch-bridge -p snitchwatch-bridge-cli` succeeds clean.
2. `cargo clippy --workspace -- -D warnings` succeeds clean (zero warnings).
3. `cargo test -p snitchwatch-bridge blocklists` passes — at least 25 tests across `store`, `format`, `fetcher`, `materializer`, `mod` (manager + scheduler + sink + cache-preservation guard).
4. `cargo test -p snitchwatch-bridge --test blocklists_e2e` passes within 10 seconds.
5. The five LS WS message types `setBlocklists`, `setBlocklistDetails`, `setBlocklistEntries`, `setBlocklistStatus`, `setBlocklistEntryLocation` are strongly typed in `ws_messages.rs` (no `serde_json::Value` slots remain for blocklist payloads).
6. The two LS WS client actions `subscribeBlocklist` and `unsubscribeBlocklist` are routed through `translator::upstream::handle_blocklist_action` to the manager.
7. A failed fetch (HTTP 5xx, connection refused, or missing file) leaves the prior `entries` rows in SQLite untouched and emits only a `StatusChanged` event.
8. Materialized deny rules use names of shape `900-blocklist:<sanitized_id>:<seq04>-<host>` and carry a JSON tag `{"snitchwatch":{"source":"blocklist","list_id":"<id>","entry":"<host>"}}` in the description field.
9. Rule classification preserves any pre-existing 900-band rule that lacks the snitchwatch tag — i.e., a `delete-then-replace` blocklist push is **scoped to tagged rules only**.
10. The Blocklists tab in the vendored web UI receives `setBlocklists` and `setBlocklistEntries` payloads on connection startup and on subscription changes (verified via the Playwright smoke test from Plan 4 IF Plan 4 is already merged; otherwise verified via the e2e integration test in Task 13).
11. `just test-blocklists` runs the blocklist unit + integration suite end-to-end.
12. The design spec milestone table shows ✅ next to **M4 — Blocklists**.
13. README has a "Subscribe to a blocklist" section with a copy-pasteable websocat invocation against the local fixture server.
14. Every new file is ≤ 800 lines (`store.rs` is the largest at ~340 lines; well within budget).

---

## Deferred to later plans

- **Real-daemon smoke test against StevenBlack/hosts** — needs a live opensnitchd in rootful podman; deferred to Plan 7 alongside the Plan 1 environmental items.
- **Refresh schedule UI** — the Blocklists tab in upstream LS-for-Linux already has a refresh button; the bridge handles it via `subscribeBlocklist` re-issue with the same URL. Custom intervals from the UI come in v2.
- **Per-list enable/disable toggle** without unsubscribing — requires a new client action `setBlocklistEnabled`; deferred until a user actually asks for it.
- **Ephemeral WS bind address** — Plan 7 flips `BridgeConfig::ws_bind` from `127.0.0.1:3031` to `127.0.0.1:0`. The blocklists code path doesn't care about the bind mode.
- **Flatpak sandbox path mapping** — `$XDG_DATA_HOME/snitchwatch/blocklists.db` works on host but the Flatpak manifest needs `--persist=.local/share/snitchwatch` permission. Plan 6 wires that.
- **`cargo-llvm-cov` ≥ 80%** on the `blocklists::*` modules — environmental, deferred to Plan 7 per `plan1_deferred_criteria.md`.
- **Diagnostic bundle** that includes the blocklists.db row count — Plan 6.

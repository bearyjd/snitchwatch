//! Building a page costs memory bounded by the row caps, not by the ledger:
//! 4,096 nested gaps over 4,096 short disjoint sessions would make about 50
//! million gap indexes (hundreds of MiB) if the tiling were built before the
//! row caps were checked. Its own test binary: the counting allocator is the
//! measurement.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use snitchwatch_bridge::history::lifecycle::Generation;
use snitchwatch_bridge::history::reader::{read_page, Context, Limits, Outcome, Request};
use snitchwatch_bridge::history::reader_registry::CancelToken;
use snitchwatch_bridge::history::{enable, locate, open_store, Consent, Mode};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(by: usize) {
    let now = LIVE.fetch_add(by, Ordering::SeqCst) + by;
    PEAK.fetch_max(now, Ordering::SeqCst);
}

// SAFETY: every call is forwarded unchanged to the system allocator; the
// counters only observe it.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        grow(layout.size());
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
        System.dealloc(ptr, layout);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
        grow(new_size);
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn a_page_over_a_huge_nested_ledger_stays_under_a_stated_memory_bound() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().canonicalize().unwrap().join("state");
    std::fs::create_dir(&state).unwrap();
    let location = locate(Mode::User, Some(&state)).unwrap();
    enable(
        &location,
        Consent {
            text_id: "t".into(),
            accepted_ms: 1,
        },
    )
    .unwrap();
    let store = open_store(&location).unwrap();
    store
        .connection()
        .execute_batch(
            "INSERT INTO events (ts_ms, action, answered_by, process_path) \
             VALUES (50000, 'allow', 'rule', '/usr/bin/x'); \
             WITH RECURSIVE c(x) AS (SELECT 0 UNION ALL SELECT x + 1 FROM c WHERE x < 4095) \
             INSERT INTO gaps (start_ms, end_ms, start_exact, cause, count) \
             SELECT x, 100000 - x, 1, 'upstream-loss', 1 FROM c; \
             WITH RECURSIVE c(x) AS (SELECT 0 UNION ALL SELECT x + 1 FROM c WHERE x < 4095) \
             INSERT INTO sessions (start_ms, end_ms, end_kind) \
             SELECT 4101 + x * 10, 4106 + x * 10, 'clean' FROM c;",
        )
        .unwrap();
    let (generation, cancel) = (Generation::default(), CancelToken::default());
    let ctx = Context {
        location: &location,
        generation: &generation,
        cancel: &cancel,
        now_ms: 100_000,
        live_session_start_ms: None,
        standing: None,
        limits: Limits::default(),
    };
    let request = Request {
        cursor: None,
        limit: 200,
    };
    let baseline = LIVE.load(Ordering::SeqCst);
    PEAK.store(baseline, Ordering::SeqCst);
    let outcome = read_page(&request, &ctx);
    let peak = PEAK.load(Ordering::SeqCst) - baseline;
    let Outcome::Page(page) = outcome else {
        panic!("{outcome:?}")
    };
    assert!(
        page.coverage.len() <= 1_000,
        "{} segments",
        page.coverage.len()
    );
    // 8,192 ledger rows and a page of a few hundred segments: a few MiB at
    // most. The whole tiling would be over 300 MiB.
    assert!(peak < 16 * 1024 * 1024, "peak {peak} bytes");
}

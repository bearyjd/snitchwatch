# Snitchwatch M2 — Vendored UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Vendor the Little Snitch for Linux web SPA into `web/`, rebrand it to "Snitchwatch" via an idempotent script, serve it from the bridge's axum router alongside the existing `/stream` WebSocket, switch the WebSocket bind to a fixed `127.0.0.1:3031` for predictable browser debugging, and prove a real Firefox tab can render the UI against live bridge data.

**Architecture:** A new sibling axum route group serves static assets out of an embedded directory (via `rust-embed` so the binary stays single-file). The same `axum::Router` already handling `/stream` gains `GET /` (index.html), `GET /assets/*path`, and a default fallback. The WebSocket bind address default flips from `127.0.0.1:0` to `127.0.0.1:3031` (memo-able for `websocat` and a real browser tab); the ephemeral mode stays available via env var for tests. Vendoring is a literal `git add` of the upstream snapshot under `web/`, recorded in `web/VENDORED.md`. Rebranding is a single bash script (`web/rebrand.sh`) that performs deterministic in-place text substitutions and is committed as a separate atomic commit so the diff is inspectable.

**Tech Stack:** rust-embed 8 (compile-time asset embedding, zero-runtime-IO), mime_guess 2 (content-type detection by extension), axum 0.7 (existing), tokio 1.40 (existing). Browser smoke test: a Playwright script in `tests/web_smoke/` driven by `npx playwright test` against a locally running `snitchwatch-bridge-cli` + `tests/mock_opensnitchd` driver.

**What this plan does NOT cover:**
- Tauri shell, system tray, autostart, native notifications (Plan 4 — M3).
- Blocklists tab wiring (Plan 5 — M4).
- Flatpak packaging (Plan 6 — M5).
- The flip from fixed 3031 back to ephemeral on `127.0.0.1:0` (Plan 7 — M6).
- Translating any of the 22 LS WS message types beyond what M1.5 already covers (`InsertConnectionRows`, `SetVerdict`). The remaining 20 message types are touched only enough that the UI doesn't crash on undefined fields; full coverage is its own follow-up.

---

## Memory Constraints (read before starting)

These guard rails come from `~/.claude/projects/-var-home-user-Documents-vibe-code-opensnitch-gui/memory/`:

1. **`bash_antipattern_hook.md`** — workspace blocks `find`/`ls`/`cat`/`grep`/`rg`/`head`/`tail`/`sed`/`awk` in Bash. Use Read/Grep/Glob tools. PostToolUse hooks may fire false-positive "failure" reminders on success — verify by stdout, not by reminder tag.
2. **`m1_envelope_hack.md`** — the JSON envelope inside `Notification.data` is gone after Plan 2. Do not reintroduce it. If a UI message type needs new payload, add it to `ws_messages.rs` as a typed variant.
3. **`clippy_gotchas_bridge.md`** — wherever a `oneshot::Receiver<Verdict>` is dropped, use `drop(...)`, never `let _ = ...`. Box `ConnectionRow` inside enum variants.
4. **`autonomous_tdd_resume.md`** — on resume after compaction, advance the next task with a tool call; don't recap.
5. **`plan1_deferred_criteria.md`** — Plan 1 deferred items (live opensnitchd smoke, llvm-cov ≥80%) are environmental and belong to Plan 7. Don't reopen them here.

---

## File Structure

### NEW files
- `web/VENDORED.md` — provenance: upstream URL, commit hash, fetch date, license, list of files captured in the snapshot.
- `web/rebrand.sh` — idempotent text-substitution script. POSIX bash, no GNU-only flags.
- `web/index.html` — vendored verbatim, then patched by `rebrand.sh`.
- `web/manifest.json` — vendored, then patched.
- `web/styles.css`, `web/connections.css`, `web/blocklists.css`, `web/rules.css`, `web/traffic.css`, `web/uPlot.min.css` — vendored.
- `web/js/app.js`, `web/js/connections.js`, `web/js/blocklists.js`, `web/js/rules.js`, `web/js/traffic.js`, `web/js/selection.js`, `web/js/datetime.js`, `web/js/localization.js`, `web/js/uPlot.iife.min.js` — vendored.
- `web/icons/snitchwatch-192.png`, `web/icons/snitchwatch-512.png`, `web/icons/snitchwatch.svg` — placeholder Snitchwatch icons (re-rendered eye-silhouette, see design spec §UX details / Branding rebrand).
- `crates/snitchwatch-bridge/src/web_assets.rs` — `rust-embed` derive over `web/`, plus an axum handler that resolves a request path to an embedded asset and returns the bytes with the right `Content-Type`. Falls back to `index.html` for 404s so client-side routing still works.
- `tests/web_smoke/playwright.config.ts` — Playwright config (Firefox channel, single worker, 60s test timeout).
- `tests/web_smoke/tests/loads_index.spec.ts` — smoke test: launch bridge, point Playwright at `http://127.0.0.1:3031/`, assert `<title>` and the Connections tab heading.
- `tests/web_smoke/tests/round_trips_ask_rule.spec.ts` — drives the mock to fire AskRule and asserts the row appears in the live UI.
- `tests/web_smoke/package.json` — single dev dependency `@playwright/test`. No production deps.
- `tests/web_smoke/.gitignore` — `node_modules/`, `playwright-report/`, `test-results/`.

### MODIFIED files
- `crates/snitchwatch-bridge/Cargo.toml` — add `rust-embed = "8"` and `mime_guess = "2"` deps.
- `crates/snitchwatch-bridge/src/lib.rs` — add `pub mod web_assets;` line.
- `crates/snitchwatch-bridge/src/ws_server.rs` — `Router::new()` chain gains `.route("/", get(serve_index))`, `.route("/assets/*path", get(serve_asset))`, `.fallback(serve_fallback)` from `web_assets`.
- `crates/snitchwatch-bridge-cli/src/lib.rs` — `BridgeConfig::default()` ws_bind flips from `127.0.0.1:0` to `127.0.0.1:3031`. The env var `SNITCHWATCH_WS_BIND` still overrides; the test setup explicitly passes `:0` so concurrent tests don't collide.
- `crates/snitchwatch-bridge-cli/src/main.rs` — startup banner mentions `http://{ws_addr}/` so a user can paste it into Firefox.
- `justfile` — add `just web-smoke` recipe that runs the Playwright suite, and `just web-rebrand` that re-runs the rebrand script.
- `README.md` — add a "Try it in your browser" section explaining `cargo run -p snitchwatch-bridge-cli` then opening `http://127.0.0.1:3031/`.
- `docs/superpowers/specs/2026-04-10-snitchwatch-design.md` — flip the milestone table to mark M2 done.
- `.gitignore` — add `tests/web_smoke/node_modules`, `tests/web_smoke/playwright-report`, `tests/web_smoke/test-results`.

### DELETED files
- None. M2 is purely additive on top of the M1.5 bridge.

---

## Part A — Vendor the upstream UI

### Task 1: Capture the upstream snapshot

**Files:**
- Create: `web/VENDORED.md`
- Create: `web/index.html`, `web/manifest.json`, `web/styles.css`, `web/connections.css`, `web/blocklists.css`, `web/rules.css`, `web/traffic.css`, `web/uPlot.min.css`
- Create: `web/js/app.js`, `web/js/connections.js`, `web/js/blocklists.js`, `web/js/rules.js`, `web/js/traffic.js`, `web/js/selection.js`, `web/js/datetime.js`, `web/js/localization.js`, `web/js/uPlot.iife.min.js`

This is a vendoring task, not a coding task. We are recording the exact upstream commit so future re-syncs are unambiguous.

- [ ] **Step 1: Fetch the upstream snapshot into a scratch directory**

Run from the repo root:

```bash
mkdir -p /tmp/ls-linux-fetch
git -C /tmp/ls-linux-fetch clone --depth 1 https://github.com/obdev/littlesnitch-linux ls-linux
UPSTREAM_COMMIT=$(git -C /tmp/ls-linux-fetch/ls-linux rev-parse HEAD)
UPSTREAM_DATE=$(git -C /tmp/ls-linux-fetch/ls-linux log -1 --format=%cd --date=iso-strict)
echo "$UPSTREAM_COMMIT $UPSTREAM_DATE"
```

Expected: a 40-char SHA followed by an ISO-8601 date. Record both — they go into `VENDORED.md`.

- [ ] **Step 2: Copy the SPA into `web/`**

Run from the repo root:

```bash
mkdir -p web/js web/icons
cp /tmp/ls-linux-fetch/ls-linux/web/index.html        web/index.html
cp /tmp/ls-linux-fetch/ls-linux/web/manifest.json     web/manifest.json
cp /tmp/ls-linux-fetch/ls-linux/web/styles.css        web/styles.css
cp /tmp/ls-linux-fetch/ls-linux/web/connections.css   web/connections.css
cp /tmp/ls-linux-fetch/ls-linux/web/blocklists.css    web/blocklists.css
cp /tmp/ls-linux-fetch/ls-linux/web/rules.css         web/rules.css
cp /tmp/ls-linux-fetch/ls-linux/web/traffic.css       web/traffic.css
cp /tmp/ls-linux-fetch/ls-linux/web/uPlot.min.css     web/uPlot.min.css
cp /tmp/ls-linux-fetch/ls-linux/web/js/app.js            web/js/app.js
cp /tmp/ls-linux-fetch/ls-linux/web/js/connections.js   web/js/connections.js
cp /tmp/ls-linux-fetch/ls-linux/web/js/blocklists.js    web/js/blocklists.js
cp /tmp/ls-linux-fetch/ls-linux/web/js/rules.js         web/js/rules.js
cp /tmp/ls-linux-fetch/ls-linux/web/js/traffic.js       web/js/traffic.js
cp /tmp/ls-linux-fetch/ls-linux/web/js/selection.js     web/js/selection.js
cp /tmp/ls-linux-fetch/ls-linux/web/js/datetime.js      web/js/datetime.js
cp /tmp/ls-linux-fetch/ls-linux/web/js/localization.js  web/js/localization.js
cp /tmp/ls-linux-fetch/ls-linux/web/js/uPlot.iife.min.js web/js/uPlot.iife.min.js
```

If any source file is missing (the upstream layout has shifted), stop and update this task — the vendoring needs to record the actual upstream tree, not what this plan assumed.

- [ ] **Step 3: Write the provenance record**

Create `web/VENDORED.md`:

```markdown
# Vendored: Little Snitch for Linux web UI

**Upstream:** https://github.com/obdev/littlesnitch-linux
**Commit:** <UPSTREAM_COMMIT>
**Fetched:** <UPSTREAM_DATE>
**License:** GPL-2.0-or-later
**Path inside upstream:** `web/`

## What we capture

This directory is a verbatim snapshot of the upstream `web/` directory at the
commit recorded above, with one mechanical edit applied via `./rebrand.sh`
(see Snitchwatch commit history for the diff).

## Re-syncing

```bash
git clone --depth 1 https://github.com/obdev/littlesnitch-linux /tmp/ls-linux
diff -ruN /tmp/ls-linux/web/ web/   # eyeball the upstream delta
# copy new/changed files in
./rebrand.sh                        # idempotent — safe to re-run
git diff                            # confirm only the rebrand strings flip
```

## What is NOT vendored

- Anything outside `web/` (build scripts, app shell, etc.). We replace those with our own bridge.
- Unit tests — upstream tests target the LS data layer, not ours.
- License files — GPL-2.0 obligations are tracked at the repo root in `LICENSE`.

## Snapshot file list

- index.html
- manifest.json
- styles.css, connections.css, blocklists.css, rules.css, traffic.css, uPlot.min.css
- js/{app,connections,blocklists,rules,traffic,selection,datetime,localization}.js
- js/uPlot.iife.min.js
```

Replace `<UPSTREAM_COMMIT>` and `<UPSTREAM_DATE>` with the values captured in Step 1.

- [ ] **Step 4: Commit the snapshot as one atomic commit**

```bash
git add web/VENDORED.md web/index.html web/manifest.json web/*.css web/js/*.js
git commit -m "vendor(web): import LS-for-Linux SPA at <short-commit>"
```

The commit message body is intentionally short — the diff is the snapshot, so the message just records provenance.

- [ ] **Step 5: Sanity check the snapshot loads in a browser**

Run from repo root:

```bash
python3 -m http.server -d web 8765 &
SERVER_PID=$!
sleep 1
curl -fsS http://127.0.0.1:8765/index.html | head -c 200
kill $SERVER_PID
```

Expected: the first 200 chars of `index.html` (HTML doctype + opening tags). If `curl` fails or the file is empty, the vendoring step is broken — fix it before proceeding.

---

### Task 2: Write the rebrand script

**Files:**
- Create: `web/rebrand.sh`

The script must be idempotent — running it twice produces the same output as running it once. That property lets us re-sync the upstream by dropping new files in and re-running `./rebrand.sh`.

- [ ] **Step 1: Create the script**

Create `web/rebrand.sh`:

```bash
#!/usr/bin/env bash
# Idempotent rebrand pass for the vendored Little Snitch for Linux UI.
#
# Run from the `web/` directory or the repo root — both work. The script
# only edits files under `web/` and never touches anything outside it.
#
# Idempotency:
#   - All substitutions are guarded with grep so a no-op run produces no diff.
#   - The order of substitutions is fixed; running twice yields the same tree.
#
# Reproducibility:
#   - No GNU-only sed flags. Works on macOS BSD sed and Linux GNU sed.

set -euo pipefail

# Resolve the web/ directory regardless of where the script is invoked from.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WEB_DIR="$SCRIPT_DIR"
cd "$WEB_DIR"

# In-place sed wrapper that works on BSD and GNU sed.
sed_inplace() {
  local pattern="$1"
  local file="$2"
  if [ "$(uname)" = "Darwin" ]; then
    sed -i '' "$pattern" "$file"
  else
    sed -i "$pattern" "$file"
  fi
}

# A substitution table: each entry is "pattern|replacement|file-glob".
# Globs are evaluated with `find` so they walk subdirectories.
substitutions=(
  's|Little Snitch for Linux|Snitchwatch|g|*.html *.json *.js *.css'
  's|Little Snitch|Snitchwatch|g|*.html *.json *.js'
  's|littlesnitch-linux|snitchwatch|g|*.html *.json *.js *.css'
  's|com\\.obdev\\.littlesnitch|org.snitchwatch|g|*.json'
  's|littlesnitch-192\\.png|snitchwatch-192.png|g|*.html *.json'
  's|littlesnitch-512\\.png|snitchwatch-512.png|g|*.html *.json'
  's|littlesnitch\\.svg|snitchwatch.svg|g|*.html *.json'
)

for entry in "${substitutions[@]}"; do
  pattern="$(echo "$entry" | cut -d'|' -f1-2)"
  globs="$(echo "$entry" | cut -d'|' -f4)"
  for glob in $globs; do
    find . -type f -name "$glob" -print0 | while IFS= read -r -d '' f; do
      sed_inplace "$pattern/" "$f" 2>/dev/null || true
    done
  done
done

echo "rebrand.sh: done. Re-runnable; this output is identical on subsequent runs."
```

- [ ] **Step 2: Make it executable**

```bash
chmod +x web/rebrand.sh
```

- [ ] **Step 3: Run the rebrand once and inspect the diff**

```bash
./web/rebrand.sh
git diff --stat web/
```

Expected: a handful of files changed, with the brand strings replaced. No binary files touched.

- [ ] **Step 4: Run the rebrand a second time and assert no diff**

```bash
./web/rebrand.sh
git diff web/
```

Expected: empty. If the second run produces a diff, the script is not idempotent — fix the substitution that's not guarded.

- [ ] **Step 5: Commit the rebrand**

```bash
git add web/rebrand.sh web/index.html web/manifest.json web/js/ web/*.css
git commit -m "feat(web): apply Snitchwatch rebrand to vendored UI

Idempotent script at web/rebrand.sh. Re-running upstream sync is:
  cp upstream/web/* web/ && ./web/rebrand.sh && git diff"
```

---

### Task 3: Add placeholder Snitchwatch icons

**Files:**
- Create: `web/icons/snitchwatch.svg`
- Create: `web/icons/snitchwatch-192.png`
- Create: `web/icons/snitchwatch-512.png`

Per the design spec branding section, the v1 icon is a re-rendered eye-silhouette so we are not riding the LS visual identity. These files are placeholders — a real designer can replace them in a follow-up.

- [ ] **Step 1: Author the SVG source**

Create `web/icons/snitchwatch.svg`:

```svg
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512">
  <rect width="512" height="512" rx="96" fill="#0d6abf"/>
  <ellipse cx="256" cy="256" rx="180" ry="100" fill="none" stroke="#f6f8fc" stroke-width="24"/>
  <circle cx="256" cy="256" r="56" fill="#f6f8fc"/>
  <circle cx="256" cy="256" r="22" fill="#0d6abf"/>
</svg>
```

- [ ] **Step 2: Rasterise the PNGs**

Run from the repo root:

```bash
command -v rsvg-convert >/dev/null || { echo "install librsvg2-tools (dnf install librsvg2-tools)"; exit 1; }
rsvg-convert -w 192 -h 192 web/icons/snitchwatch.svg -o web/icons/snitchwatch-192.png
rsvg-convert -w 512 -h 512 web/icons/snitchwatch.svg -o web/icons/snitchwatch-512.png
```

If `rsvg-convert` is unavailable, an alternative using `inkscape` or `convert` (ImageMagick) is acceptable as long as the output dimensions match.

- [ ] **Step 3: Verify the files exist and are non-empty**

Use Glob to confirm both PNGs exist, then `stat -c %s web/icons/snitchwatch-192.png` to confirm a non-zero size.

- [ ] **Step 4: Commit the icons**

```bash
git add web/icons/snitchwatch.svg web/icons/snitchwatch-192.png web/icons/snitchwatch-512.png
git commit -m "feat(web): add v1 placeholder Snitchwatch icon set"
```

---

## Part B — Serve the SPA from the bridge

### Task 4: Add `rust-embed` and `mime_guess` to the bridge

**Files:**
- Modify: `crates/snitchwatch-bridge/Cargo.toml`

- [ ] **Step 1: Add the deps**

Edit `crates/snitchwatch-bridge/Cargo.toml`. Inside the `[dependencies]` table, add:

```toml
rust-embed = { version = "8", features = ["interpolate-folder-path"] }
mime_guess = "2"
```

- [ ] **Step 2: Verify the workspace builds**

```bash
cargo check -p snitchwatch-bridge
```

Expected: clean build, no warnings.

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-bridge/Cargo.toml Cargo.lock
git commit -m "feat(bridge): add rust-embed and mime_guess deps for static asset serving"
```

---

### Task 5: Implement the embedded asset handler

**Files:**
- Create: `crates/snitchwatch-bridge/src/web_assets.rs`
- Modify: `crates/snitchwatch-bridge/src/lib.rs`

- [ ] **Step 1: Add the module declaration**

Edit `crates/snitchwatch-bridge/src/lib.rs`. Add:

```rust
pub mod web_assets;
```

next to the other `pub mod` lines.

- [ ] **Step 2: Write the failing test file**

Create `crates/snitchwatch-bridge/src/web_assets.rs`:

```rust
//! Compile-time embedded `web/` directory served by the axum router.
//!
//! `rust-embed` walks `web/` at build time, hashes each file, and stores it
//! in the resulting binary. At runtime we resolve a request path to an
//! embedded asset, sniff the content type from the extension, and stream
//! the bytes back. Unknown paths fall through to `index.html` so the SPA's
//! client-side routing keeps working.

use axum::{
    body::Body,
    extract::Path,
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../web/"]
pub struct WebAssets;

pub async fn serve_index() -> Response {
    serve_path("index.html")
}

pub async fn serve_asset(Path(path): Path<String>) -> Response {
    serve_path(&path)
}

pub async fn serve_fallback() -> Response {
    // SPA fallback — anything we don't recognize gets the index so client-side
    // routing in app.js handles it.
    serve_path("index.html")
}

fn serve_path(path: &str) -> Response {
    match WebAssets::get(path) {
        Some(asset) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let header_value = HeaderValue::from_str(mime.as_ref())
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, header_value)
                .body(Body::from(asset.data.into_owned()))
                .unwrap()
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("not found"))
            .unwrap(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn serve_index_returns_html_with_correct_mime() {
        let response = serve_index().await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap(),
            "text/html"
        );
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body_str = std::str::from_utf8(&body).unwrap();
        assert!(body_str.contains("Snitchwatch"), "rebrand should be applied");
        assert!(!body_str.contains("Little Snitch"), "no leftover LS branding");
    }

    #[tokio::test]
    async fn serve_asset_returns_javascript_for_app_js() {
        let response = serve_asset(Path("js/app.js".to_string())).await;
        assert_eq!(response.status(), StatusCode::OK);
        let mime = response
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        // mime_guess returns application/javascript or text/javascript depending on
        // the table version — accept either to keep the test resilient.
        assert!(
            mime.contains("javascript"),
            "got {mime}, expected a javascript mime"
        );
    }

    #[tokio::test]
    async fn missing_asset_falls_back_to_404() {
        let response = serve_asset(Path("does/not/exist.txt".to_string())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn fallback_returns_index_for_spa_routes() {
        let response = serve_fallback().await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body_str = std::str::from_utf8(&body).unwrap();
        assert!(body_str.contains("<html"));
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail at compile time**

```bash
cargo test -p snitchwatch-bridge web_assets
```

Expected: tests compile and run. They should all PASS — the implementation is in the same file as the tests. If they fail, the bug is either in `serve_path` (mime header building, asset lookup) or in the rebrand from Task 2 (the assertion `contains("Snitchwatch")` and `!contains("Little Snitch")` cross-checks rebrand correctness).

- [ ] **Step 4: Commit**

```bash
git add crates/snitchwatch-bridge/src/web_assets.rs crates/snitchwatch-bridge/src/lib.rs
git commit -m "feat(bridge): embed web/ via rust-embed and serve from axum

The handler returns index.html for SPA fallback paths, sniffs MIME from the
file extension, and streams bytes from the binary with no runtime IO."
```

---

### Task 6: Wire the static routes into the axum router

**Files:**
- Modify: `crates/snitchwatch-bridge/src/ws_server.rs`

- [ ] **Step 1: Import the asset handlers**

Edit `crates/snitchwatch-bridge/src/ws_server.rs`. Near the existing `use crate::ws_messages::...` line add:

```rust
use crate::web_assets::{serve_asset, serve_fallback, serve_index};
```

- [ ] **Step 2: Mount the routes**

Locate the `serve` method on `WsServer`. Replace its `Router::new()` chain with:

```rust
    pub async fn serve(self, listener: TcpListener) -> std::io::Result<()> {
        let app = Router::new()
            .route("/stream", get(ws_handler))
            .route("/", get(serve_index))
            .route("/assets/*path", get(serve_asset))
            .fallback(serve_fallback)
            .with_state(self.handles);

        info!(addr = ?listener.local_addr()?, "WS+HTTP server starting");
        axum::serve(listener, app).await
    }
```

(The `with_state` call still applies — only `/stream` consumes it, the static handlers ignore the state, which axum permits.)

- [ ] **Step 3: Add an integration test that exercises the routes**

Append to the existing `tests` module at the bottom of `ws_server.rs`:

```rust
    #[tokio::test]
    async fn server_serves_index_html_at_root() {
        use axum::body::to_bytes;
        use axum::http::Request;
        use tower::ServiceExt;

        let (broadcast_tx, _) = broadcast::channel(16);
        let (inbound_tx, _) = mpsc::channel(16);
        let handles = WsHandles {
            broadcast: broadcast_tx,
            inbound: inbound_tx,
        };

        let app = Router::new()
            .route("/stream", get(ws_handler))
            .route("/", get(serve_index))
            .route("/assets/*path", get(serve_asset))
            .fallback(serve_fallback)
            .with_state(handles);

        let response = app
            .oneshot(Request::builder().uri("/").body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert!(std::str::from_utf8(&body).unwrap().contains("Snitchwatch"));
    }

    #[tokio::test]
    async fn server_serves_asset_js() {
        use axum::http::Request;
        use tower::ServiceExt;

        let (broadcast_tx, _) = broadcast::channel(16);
        let (inbound_tx, _) = mpsc::channel(16);
        let handles = WsHandles {
            broadcast: broadcast_tx,
            inbound: inbound_tx,
        };
        let app = Router::new()
            .route("/stream", get(ws_handler))
            .route("/", get(serve_index))
            .route("/assets/*path", get(serve_asset))
            .fallback(serve_fallback)
            .with_state(handles);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/assets/js/app.js")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }
```

This test uses `tower::ServiceExt::oneshot` to drive the router without binding a real listener. Add `tower = { version = "0.5", features = ["util"] }` to `[dev-dependencies]` if it isn't already there.

- [ ] **Step 4: Run the tests**

```bash
cargo test -p snitchwatch-bridge ws_server
```

Expected: 3 passed (the existing `server_binds_to_ephemeral_port` plus the two new ones).

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/ws_server.rs crates/snitchwatch-bridge/Cargo.toml
git commit -m "feat(bridge): mount static web/ routes alongside /stream WebSocket"
```

---

## Part C — Stable bind + browser smoke test

### Task 7: Default the WebSocket bind to a fixed `127.0.0.1:3031`

**Files:**
- Modify: `crates/snitchwatch-bridge-cli/src/lib.rs`

- [ ] **Step 1: Locate `BridgeConfig::default`**

Read the file and locate the `impl Default for BridgeConfig` block. The current `ws_bind` field is `"127.0.0.1:0".parse().unwrap()` (ephemeral).

- [ ] **Step 2: Flip the default**

Replace the `ws_bind` line in the `Default` impl with:

```rust
            ws_bind: "127.0.0.1:3031".parse().expect("hardcoded socket addr is valid"),
```

Leave `BridgeConfig::from_env` unchanged — it already reads `SNITCHWATCH_WS_BIND` and falls back to the default. Tests that need an ephemeral port set the env var explicitly.

- [ ] **Step 3: Update the test setup**

Find any test in this file (or in the integration tests under `tests/`) that calls `BridgeConfig::default()` without overriding `ws_bind`. Each test that runs concurrently must set `ws_bind` to `"127.0.0.1:0".parse().unwrap()` explicitly, otherwise test runs collide on port 3031.

In `tests/bridge_protocol_test.rs`, locate the `BridgeConfig` construction and ensure it uses:

```rust
let config = BridgeConfig {
    ws_bind: "127.0.0.1:0".parse().unwrap(),
    ..BridgeConfig::default()
};
```

(Same pattern for `grpc_bind`, which Plan 2 already established as `127.0.0.1:0` in tests.)

- [ ] **Step 4: Run the test suite**

```bash
cargo test --workspace
```

Expected: all tests still pass. If any test fails because port 3031 is in use, the test is missing the `ws_bind` override from Step 3.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge-cli/src/lib.rs tests/bridge_protocol_test.rs
git commit -m "feat(bridge-cli): default WebSocket bind to 127.0.0.1:3031 for browser debugging

Tests still bind to :0 to avoid port collisions. M6 will flip the default
back to ephemeral as part of the public-release tightening pass."
```

---

### Task 8: Update the CLI startup banner

**Files:**
- Modify: `crates/snitchwatch-bridge-cli/src/main.rs`

- [ ] **Step 1: Print a browser-paste-friendly URL**

Find the line that currently prints `WS_LISTEN_ADDR=...`. Add a sibling print:

```rust
    println!("WS_LISTEN_ADDR={}", running.ws_addr);
    println!("GRPC_LISTEN_ADDR={}", running.grpc_addr);
    println!();
    println!("→ open http://{}/ in your browser", running.ws_addr);
```

The blank line + arrow are intentional — they make the URL stand out in a developer's terminal.

- [ ] **Step 2: Sanity check the binary**

```bash
cargo run -p snitchwatch-bridge-cli &
BRIDGE_PID=$!
sleep 2
kill $BRIDGE_PID
```

Expected: stdout contains `WS_LISTEN_ADDR=127.0.0.1:3031`, `GRPC_LISTEN_ADDR=127.0.0.1:...`, and the `→ open http://...` line.

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-bridge-cli/src/main.rs
git commit -m "feat(bridge-cli): print browser-paste-friendly URL on startup"
```

---

### Task 9: Author the Playwright smoke test scaffolding

**Files:**
- Create: `tests/web_smoke/package.json`
- Create: `tests/web_smoke/playwright.config.ts`
- Create: `tests/web_smoke/.gitignore`
- Modify: `.gitignore`

- [ ] **Step 1: Create the test workspace**

Create `tests/web_smoke/package.json`:

```json
{
  "name": "snitchwatch-web-smoke",
  "version": "0.0.0",
  "private": true,
  "description": "Playwright smoke tests for the bridge-served vendored UI.",
  "scripts": {
    "test": "playwright test",
    "test:headed": "playwright test --headed",
    "install-browsers": "playwright install firefox"
  },
  "devDependencies": {
    "@playwright/test": "^1.47.0"
  }
}
```

- [ ] **Step 2: Create the Playwright config**

Create `tests/web_smoke/playwright.config.ts`:

```ts
import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: './tests',
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 60_000,
  reporter: [['list']],
  use: {
    baseURL: process.env.SNITCHWATCH_WEB_BASE ?? 'http://127.0.0.1:3031',
    trace: 'retain-on-failure',
    actionTimeout: 10_000,
    navigationTimeout: 15_000,
  },
  projects: [
    {
      name: 'firefox',
      use: { ...devices['Desktop Firefox'] },
    },
  ],
});
```

- [ ] **Step 3: Create the gitignore**

Create `tests/web_smoke/.gitignore`:

```
node_modules/
playwright-report/
test-results/
.cache/
```

- [ ] **Step 4: Update the root gitignore**

Edit `.gitignore` at the repo root. Append:

```
tests/web_smoke/node_modules/
tests/web_smoke/playwright-report/
tests/web_smoke/test-results/
```

- [ ] **Step 5: Install the dev dependencies**

```bash
cd tests/web_smoke && npm install && npx playwright install firefox
```

Expected: Playwright + Firefox installed under `tests/web_smoke/node_modules/`. If `npm` is not available locally, the test runs in CI only — note that and proceed.

- [ ] **Step 6: Commit**

```bash
git add tests/web_smoke/package.json tests/web_smoke/playwright.config.ts tests/web_smoke/.gitignore .gitignore
git commit -m "test(web-smoke): scaffold Playwright workspace under tests/web_smoke"
```

---

### Task 10: Write the index-loads smoke test

**Files:**
- Create: `tests/web_smoke/tests/loads_index.spec.ts`

- [ ] **Step 1: Author the test**

Create `tests/web_smoke/tests/loads_index.spec.ts`:

```ts
import { test, expect } from '@playwright/test';
import { spawn, ChildProcess } from 'node:child_process';
import { setTimeout as delay } from 'node:timers/promises';

let bridge: ChildProcess;

test.beforeAll(async () => {
  bridge = spawn('cargo', ['run', '-q', '-p', 'snitchwatch-bridge-cli'], {
    cwd: '../..',
    env: {
      ...process.env,
      SNITCHWATCH_WS_BIND: '127.0.0.1:3031',
      SNITCHWATCH_GRPC_BIND: '127.0.0.1:0',
      RUST_LOG: 'warn',
    },
    stdio: 'inherit',
  });
  // Give the bridge a few seconds to compile + bind. The first run may take
  // longer because of cargo compilation; subsequent runs reuse the artifact.
  await delay(15_000);
});

test.afterAll(async () => {
  if (bridge && !bridge.killed) bridge.kill('SIGTERM');
});

test('loads the Snitchwatch index page', async ({ page }) => {
  await page.goto('/');
  await expect(page).toHaveTitle(/Snitchwatch/i);
  await expect(page.locator('body')).not.toContainText(/Little Snitch/i);
});

test('loads the app.js asset', async ({ page }) => {
  const response = await page.goto('/assets/js/app.js');
  expect(response?.status()).toBe(200);
  const body = await response?.text();
  expect(body?.length ?? 0).toBeGreaterThan(0);
});

test('exposes the WebSocket endpoint', async ({ page }) => {
  // The /stream endpoint upgrades to WebSocket on a real client; with HTTP
  // GET it returns a 426 / 400. We just want to assert the route exists and
  // is not the SPA fallback.
  const response = await page.goto('/stream');
  // Either is acceptable — both prove the WS handler matched, not the fallback.
  expect([400, 426, 101]).toContain(response?.status() ?? 0);
});
```

- [ ] **Step 2: Run the smoke test**

```bash
cd tests/web_smoke && npx playwright test
```

Expected: 3 passed. If `cargo run` fails to build, the bridge has a compile error from earlier tasks — fix it before continuing.

If the test reports `ECONNREFUSED`, the 15-second `delay` was too short for a cold cargo build. Bump the delay to 60s on the first run, then trim it back once the binary is cached.

- [ ] **Step 3: Commit**

```bash
git add tests/web_smoke/tests/loads_index.spec.ts
git commit -m "test(web-smoke): assert vendored UI loads with rebrand applied"
```

---

### Task 11: Write the AskRule round-trip browser test

**Files:**
- Create: `tests/web_smoke/tests/round_trips_ask_rule.spec.ts`

This test drives the mock_opensnitchd helper to fire a `Connection` at the bridge over gRPC, then asserts the row appears in the live UI's Connections table.

- [ ] **Step 1: Add a tiny test helper binary that drives the mock**

The Playwright tests can't directly invoke the Rust mock library. We need a tiny helper binary the test can `spawn` to fire one AskRule and exit.

Create `tests/web_smoke/helpers/fire_ask_rule.rs`:

```rust
//! Test helper: dial the bridge's gRPC server, fire one AskRule, print the
//! returned Rule action and exit. Used by the Playwright web smoke tests.
//!
//! Usage:
//!   cargo run --quiet --manifest-path tests/web_smoke/helpers/Cargo.toml \
//!     -- --grpc 127.0.0.1:50321 --process /usr/bin/curl --host example.com --port 443

use clap::Parser;
use snitchwatch_proto::protocol::{ui_client::UiClient, Connection};

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    grpc: String,
    #[arg(long)]
    process: String,
    #[arg(long)]
    host: String,
    #[arg(long, default_value_t = 443)]
    port: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let endpoint = tonic::transport::Endpoint::from_shared(format!("http://{}", args.grpc))?;
    let channel = endpoint.connect().await?;
    let mut client = UiClient::new(channel);

    let conn = Connection {
        protocol: "tcp".into(),
        dst_host: args.host.clone(),
        dst_ip: "0.0.0.0".into(),
        dst_port: args.port,
        process_path: args.process.clone(),
        ..Default::default()
    };
    let rule = client.ask_rule(conn).await?.into_inner();
    println!("rule.action={}", rule.action);
    Ok(())
}
```

Create `tests/web_smoke/helpers/Cargo.toml`:

```toml
[package]
name = "fire_ask_rule"
version = "0.0.0"
edition = "2021"
publish = false

[[bin]]
name = "fire_ask_rule"
path = "fire_ask_rule.rs"

[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
tokio = { version = "1.40", features = ["macros", "rt-multi-thread"] }
tonic = "0.12"
snitchwatch-proto = { path = "../../../crates/snitchwatch-proto" }
```

This is a standalone binary outside the workspace so it doesn't bloat `cargo build --workspace`. The test invokes it via `cargo run --manifest-path ...`.

- [ ] **Step 2: Author the round-trip test**

Create `tests/web_smoke/tests/round_trips_ask_rule.spec.ts`:

```ts
import { test, expect } from '@playwright/test';
import { spawn, ChildProcess, spawnSync } from 'node:child_process';
import { setTimeout as delay } from 'node:timers/promises';

let bridge: ChildProcess;
let grpcAddr = '';

test.beforeAll(async () => {
  bridge = spawn('cargo', ['run', '-q', '-p', 'snitchwatch-bridge-cli'], {
    cwd: '../..',
    env: {
      ...process.env,
      SNITCHWATCH_WS_BIND: '127.0.0.1:3031',
      SNITCHWATCH_GRPC_BIND: '127.0.0.1:50321',
      RUST_LOG: 'warn',
    },
    stdio: 'inherit',
  });
  await delay(15_000);
  grpcAddr = '127.0.0.1:50321';
});

test.afterAll(async () => {
  if (bridge && !bridge.killed) bridge.kill('SIGTERM');
});

test('AskRule from mock daemon shows up in the Connections list', async ({ page }) => {
  await page.goto('/');
  await page.waitForLoadState('networkidle');

  // Spawn the helper binary in the background — it will block on the AskRule
  // until the user clicks Allow/Deny in the UI.
  const helper = spawn(
    'cargo',
    [
      'run',
      '-q',
      '--manifest-path',
      'tests/web_smoke/helpers/Cargo.toml',
      '--',
      '--grpc',
      grpcAddr,
      '--process',
      '/usr/bin/curl',
      '--host',
      'example.com',
      '--port',
      '443',
    ],
    { cwd: '../..', stdio: 'pipe' },
  );

  // Wait for the row to show up in the Connections list.
  await expect(page.getByText('example.com')).toBeVisible({ timeout: 10_000 });
  await expect(page.getByText('curl')).toBeVisible();

  // Click Allow in the inspector pane. (The exact selector depends on the
  // vendored UI; see web/js/connections.js for the inspector button id.)
  await page.getByRole('button', { name: /allow/i }).first().click();

  // The helper exits when the bridge responds with the synthesized Rule.
  const code: number = await new Promise(res => helper.on('exit', c => res(c ?? -1)));
  expect(code).toBe(0);
});
```

- [ ] **Step 3: Run the test**

```bash
cd tests/web_smoke && npx playwright test round_trips_ask_rule
```

Expected: 1 passed. If the `getByRole('button', { name: /allow/i })` selector fails because the vendored UI uses different button text, inspect the page (`npx playwright test --headed --debug`) and update the selector to match the actual rendered button.

- [ ] **Step 4: Commit**

```bash
git add tests/web_smoke/tests/round_trips_ask_rule.spec.ts tests/web_smoke/helpers/
git commit -m "test(web-smoke): round-trip AskRule from mock daemon through live UI"
```

---

## Part D — Polish

### Task 12: Add `just` recipes for the smoke + rebrand workflows

**Files:**
- Modify: `justfile`

- [ ] **Step 1: Append the recipes**

Edit `justfile`. Append:

```makefile
# Re-run the idempotent rebrand pass over the vendored web/ tree.
web-rebrand:
    ./web/rebrand.sh
    @git diff --stat web/

# Run the Playwright smoke tests against a freshly built bridge.
web-smoke:
    cd tests/web_smoke && npx playwright test

# Install the Playwright Firefox channel into tests/web_smoke/node_modules.
web-smoke-install:
    cd tests/web_smoke && npm install && npx playwright install firefox
```

- [ ] **Step 2: Verify the recipes parse**

```bash
just --list
```

Expected: the three new recipes appear in the list. If `just` complains about syntax, the most likely cause is leading spaces — recipe bodies must use literal tabs.

- [ ] **Step 3: Commit**

```bash
git add justfile
git commit -m "chore(just): add web-rebrand, web-smoke, web-smoke-install recipes"
```

---

### Task 13: Update the README with browser-tab instructions

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Add a "Try it in your browser" section**

Edit `README.md`. After the existing build instructions section, insert:

```markdown
## Try it in your browser (M2 milestone)

Snitchwatch's M2 milestone serves the vendored Little Snitch for Linux UI directly
from the bridge — no Tauri shell yet, just a browser tab.

1. Build the bridge once:
   ```bash
   cargo build -p snitchwatch-bridge-cli
   ```
2. Run it:
   ```bash
   cargo run -p snitchwatch-bridge-cli
   ```
3. The bridge prints two listen addresses on startup:
   ```text
   WS_LISTEN_ADDR=127.0.0.1:3031
   GRPC_LISTEN_ADDR=127.0.0.1:NNNNN

   → open http://127.0.0.1:3031/ in your browser
   ```
4. Open that URL in Firefox (or any modern browser). The Connections, Rules,
   Blocklists, and Traffic tabs render against the vendored SPA.

To exercise the live AskRule round trip without a real opensnitchd, point the
helper binary at the printed `GRPC_LISTEN_ADDR`:

```bash
cargo run --quiet --manifest-path tests/web_smoke/helpers/Cargo.toml -- \
  --grpc 127.0.0.1:NNNNN --process /usr/bin/curl --host example.com --port 443
```

The browser tab shows the pending row. Click Allow or Deny in the inspector and
the helper exits with the synthesized rule.
```

- [ ] **Step 2: Replace any stale "M1 only" language**

Search the rest of `README.md` for "M1" or "milestone". If any phrase implies the project is still at M1 (e.g., "currently at M1 — bridge core only"), update it to mention M2.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs(readme): add M2 browser-tab walkthrough"
```

---

### Task 14: Mark M2 done in the design spec milestone table

**Files:**
- Modify: `docs/superpowers/specs/2026-04-10-snitchwatch-design.md`

- [ ] **Step 1: Update the milestone table**

Read the design spec around line 539 (the milestone table). Edit the M2 row to prepend `✅ ` to the milestone name (matching the existing style for M0/M1/M1.5):

```markdown
| **✅ M2 — Vendored UI** | Pull `web/`, run rebrand script, serve it from the bridge, point a real browser at it. No Tauri yet — just a browser tab. | Open `http://127.0.0.1:3031/` in Firefox, see the LS UI rendered, see live connections from real opensnitchd, click Allow/Deny in the inspector and have it work. |
```

Note the URL change from `http://127.0.0.1:NNNN/` to the now-fixed `http://127.0.0.1:3031/`.

- [ ] **Step 2: Run the workspace check one more time**

```bash
just check
```

Expected: clean. `just check` runs `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`.

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-04-10-snitchwatch-design.md
git commit -m "docs(spec): mark M2 vendored UI milestone done"
```

---

## Acceptance Criteria

The plan is complete when ALL of the following are true:

1. `web/` contains the vendored LS-for-Linux SPA, with `web/VENDORED.md` recording the upstream commit + fetch date + license.
2. `./web/rebrand.sh` is idempotent — running it twice produces no diff on the second run.
3. `cargo build -p snitchwatch-bridge` embeds the rebranded `web/` tree into the binary via `rust-embed`.
4. `cargo test -p snitchwatch-bridge web_assets` reports 4 passed (including the rebrand cross-check).
5. `cargo test -p snitchwatch-bridge ws_server` reports 3 passed (the existing ephemeral-bind test plus the two new HTTP route tests).
6. `cargo run -p snitchwatch-bridge-cli` defaults to `WS_LISTEN_ADDR=127.0.0.1:3031` and prints the browser-paste URL.
7. `cd tests/web_smoke && npx playwright test loads_index` reports 3 passed (index, asset, ws-route).
8. `cd tests/web_smoke && npx playwright test round_trips_ask_rule` reports 1 passed.
9. `just check` is clean: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`.
10. `README.md` has a "Try it in your browser" section showing the M2 walkthrough.
11. The design spec milestone table marks M2 with `✅`.
12. Every task in this plan is committed as its own atomic commit on `main`.

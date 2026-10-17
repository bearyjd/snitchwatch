# r16 small follow-ups (#172, #170, #165)

## #172 collapse window arithmetic
Every `+`/`-` with `COLLAPSE_WINDOW_MS`/`TWIN_WINDOW_MS` in
`connections/collapse.rs` (`takes`, `twin_ask`, `expire`) is now saturating,
including the `first_seen - WINDOW` subtraction (overflows at `i64::MIN`).
Test: `window_arithmetic_at_the_i64_extremes_does_not_overflow` (panicked
with "attempt to add with overflow" before the fix).

## #170
a. `geo::resolver::discover_and_open` took `HOME.unwrap_or("/tmp")`. It now
   uses `paths::home_base()` (HOME, else the checked per-user no-HOME root,
   else the refused base), the same base `paths::data_dir()` uses.
   Tests: `home_from` fallback cases; geo candidate path never under /tmp.
   The wiring in `discover_and_open` itself reads the process env and has no
   unit test.
b. `settings::write_settings` now goes through `private_files::write_private`
   (`ensure_private_dir` on the parent each write, then `O_NOFOLLOW|O_NONBLOCK`
   open, `refuse_unless_ours`, truncate only after the checks, 0600).
   Tests: foreign-owned parent refused (uses `foreign_owned_dir`, root-safe),
   symlink and hard link refused, mode 0600/0700, overwrite shrinks the file.
   Mutants (private copy, all killed): drop `ensure_private_dir`; drop
   `refuse_unless_ours`; plain open instead of `open_no_follow`; drop
   `set_len(0)`. (A first run showed `refuse_unless_ours` surviving; the
   hard-link test was added to kill it.)

## #165 legacy web shell
Daemon behaviour, vendor/opensnitch/daemon:
- `rule/rule.go:22` `Allow = Action("allow")`.
- `main.go:433` `applyDefaultAction`: `uiClient.DefaultAction() == rule.Allow`
  accepts, anything else drops.
- `main.go:541` `r.Action == rule.Allow` accepts, anything else drops.
- `ui/config_utils.go:179` and `ui/notifications.go:361` copy the configured
  `DefaultAction` string verbatim (`rule.Action(...)`), no case folding.
So only the exact string `allow` is allowed. `web/js/connections.js`
`emojiFor` and the default-rule text now compare with `=== 'allow'`.
Test: none added. `tests/web_smoke` needs a Playwright install and has no
spec that renders a rule with a custom action; `emojiFor` is not exported
from the browser module, so there is no cheap unit seam.

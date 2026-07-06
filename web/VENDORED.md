# Vendored: Little Snitch for Linux web UI

**Upstream:** https://github.com/obdev/littlesnitch-linux
**Commit:** f4c2ce2dc51d811505844f5ca5509fd2a50fc97f
**Fetched:** 2026-04-10T10:13:08+02:00
**License:** GPL-2.0-or-later
**Path inside upstream:** `webroot/`

## What we capture

This directory is a verbatim snapshot of the upstream `webroot/` directory at the
commit recorded above, with one mechanical edit applied via `./rebrand.sh`
(see Snitchwatch commit history for the diff).

## Re-syncing

```bash
git clone --depth 1 https://github.com/obdev/littlesnitch-linux /tmp/ls-linux
diff -ruN /tmp/ls-linux/webroot/ web/   # eyeball the upstream delta
# copy new/changed files in
./rebrand.sh                        # idempotent — safe to re-run
git diff                            # confirm only the rebrand strings flip
```

## What is NOT vendored

- Anything outside `webroot/` (build scripts, app shell, etc.). We replace those with our own bridge.
- Unit tests — upstream tests target the LS data layer, not ours.
- License files — GPL-2.0 obligations are tracked at the repo root in `LICENSE`.

## Snapshot file list

- index.html
- manifest.json
- styles.css, connections.css, blocklists.css, rules.css, traffic.css, uPlot.min.css
- js/{app,connections,blocklists,rules,traffic,selection,datetime,localization}.js
- js/uPlot.iife.min.js
- js/sw.js (service worker — present in upstream webroot/, not listed in original plan)

## Layout deviation from plan

The upstream repo uses `webroot/` (not `web/`) as the SPA root, and all JS files
sit flat at `webroot/` rather than in a `webroot/js/` subdirectory. Files have been
copied into `web/js/` as the plan intended for the destination layout. The
`icons/` subdirectory mentioned in the plan does not exist upstream and was not
created. One extra file `sw.js` (service worker) was present in upstream and has
been included.

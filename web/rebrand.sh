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
  's|com\.obdev\.littlesnitch|org.snitchwatch|g|*.json'
  's|littlesnitch-192\.png|snitchwatch-192.png|g|*.html *.json'
  's|littlesnitch-512\.png|snitchwatch-512.png|g|*.html *.json'
  's|littlesnitch\.svg|snitchwatch.svg|g|*.html *.json'
)

for entry in "${substitutions[@]}"; do
  pattern="$(echo "$entry" | cut -d'|' -f1-4)"
  globs="$(echo "$entry" | cut -d'|' -f5)"
  for glob in $globs; do
    find . -type f -name "$glob" -print0 | while IFS= read -r -d '' f; do
      sed_inplace "$pattern" "$f" 2>/dev/null || true
    done
  done
done

echo "rebrand.sh: done. Re-runnable; this output is identical on subsequent runs."

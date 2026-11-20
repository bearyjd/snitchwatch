#!/usr/bin/env bash
# Read-only evidence capture for the file_drift open questions Q1, Q2 and Q3
# (docs/superpowers/plans/2026-11-19-privileged-file-drift.md, Open questions).
#
# Run as root ON A BOOTED BAZZITE/ostree HOST, ideally on an untouched image first.
# It changes nothing: it runs `rpm -Va`, `rpm-ostree status`, and a few read-only
# ostree/mount queries, each under a timeout, and writes everything to one directory.
#
#   sudo ./file-drift-evidence.sh [outdir]
#
# Bring back summary.txt (and, if small, the stderr files). Do not paste raw
# rpm-va.out; it can be large and lists file paths.
set -u
umask 077

TS=$(date +%Y%m%d-%H%M%S)
OUT=${1:-"./drift-evidence-$TS"}
RPM_TIMEOUT=${RPM_TIMEOUT:-900}     # generous: we want the real wall time, not a kill
OSTREE_TIMEOUT=${OSTREE_TIMEOUT:-300}
mkdir -p "$OUT"

note() { printf '%s\n' "$*" | tee -a "$OUT/summary.txt"; }

if [ "$(id -u)" -ne 0 ]; then
    echo "warning: not root; rpm -Va will miss unreadable files and results are not representative" >&2
fi

# run NAME TIMEOUT cmd...  -> NAME.out, NAME.err, and one summary line (exit code, seconds, size)
run() {
    local name=$1 limit=$2
    shift 2
    local start end rc
    start=$(date +%s.%N)
    timeout "$limit" "$@" >"$OUT/$name.out" 2>"$OUT/$name.err"
    rc=$?
    end=$(date +%s.%N)
    note "$(printf '%-22s exit=%s secs=%.1f stdout_bytes=%s stdout_lines=%s stderr_bytes=%s' \
        "$name" "$rc" "$(echo "$end - $start" | bc -l 2>/dev/null || echo 0)" \
        "$(wc -c <"$OUT/$name.out")" "$(wc -l <"$OUT/$name.out")" "$(wc -c <"$OUT/$name.err")")"
    note "  argv: $*"
}

note "== file_drift evidence $TS =="
note "host: $(uname -srm)"
note "os-release: $(. /etc/os-release 2>/dev/null && echo "${PRETTY_NAME:-unknown}")"
note "rpm: $(rpm --version 2>&1)"
note "rpm-ostree: $(rpm-ostree --version 2>&1 | head -2 | tr '\n' ' ')"
note "ostree: $(ostree --version 2>&1 | head -2 | tr '\n' ' ')"
note ""

# ---- Q3 / Q2: the exact argv the scanner runs ------------------------------------------
note "== Q2/Q3: rpm -Va with the scanner's argv (budget under test: 180 s; output cap: 1 MiB) =="
run rpm-va "$RPM_TIMEOUT" rpm -Va --nomtime --noscripts --nodeps

# Exit-code semantics: how many distinct failing packages vs the exit code seen.
note "stdout over 1 MiB cap: $([ "$(wc -c <"$OUT/rpm-va.out")" -gt 1048576 ] && echo YES || echo no)"
note "stderr empty (scanner rejects a non-zero run with stderr): $([ -s "$OUT/rpm-va.err" ] && echo NO-see-rpm-va.err || echo yes)"
note ""

note "-- attribute-letter breakdown (first field, per line) --"
awk 'NF>=2 { a=$1; if (a=="missing") a="missing"; c[a]++ } END { for (k in c) printf "%8d  %s\n", c[k], k }' \
    "$OUT/rpm-va.out" | sort -rn | head -25 | tee -a "$OUT/summary.txt"
note ""

note "-- top path prefixes (first two components) --"
awk '{ p=$NF; n=split(p, s, "/"); k="/" s[2] (n>2 ? "/" s[3] : ""); c[k]++ } END { for (k in c) printf "%8d  %s\n", c[k], k }' \
    "$OUT/rpm-va.out" | sort -rn | head -25 | tee -a "$OUT/summary.txt"
note ""

note "-- files-with-a-space or unparseable-looking lines (parser regression check) --"
awk 'NF>=2 && $NF !~ /^\// { n++ } END { printf "%d lines whose last token is not an absolute path\n", n+0 }' \
    "$OUT/rpm-va.out" | tee -a "$OUT/summary.txt"
note ""

# ---- Q1 prerequisite: real format of layered package entries ----------------------------
note "== rpm-ostree status --json (packages / requested-packages format) =="
run rpm-ostree-status 60 rpm-ostree status --json
if command -v python3 >/dev/null 2>&1; then
    python3 - "$OUT/rpm-ostree-status.out" >>"$OUT/summary.txt" 2>&1 <<'PY' || true
import json, sys
d = json.load(open(sys.argv[1]))
for dep in d.get("deployments", []):
    if dep.get("booted"):
        print("booted checksum:", dep.get("checksum"))
        print("packages sample:", dep.get("packages", [])[:5])
        print("requested-packages sample:", dep.get("requested-packages", [])[:5])
        print("layered count:", len(dep.get("packages", [])))
PY
    tail -5 "$OUT/summary.txt"
fi
note ""

# ---- Q1: what is checkable on this image ------------------------------------------------
note "== Q1: what the platform offers (read-only) =="
run findmnt-root 30 findmnt -no SOURCE,FSTYPE,OPTIONS /
run findmnt-usr 30 findmnt -no SOURCE,FSTYPE,OPTIONS /usr
run composefs-mounts 30 sh -c 'mount | grep -i composefs || true'
run prepare-root-conf 30 sh -c 'cat /etc/ostree/prepare-root.conf /usr/lib/ostree/prepare-root.conf 2>&1 || true'
run ostree-admin-status 60 ostree admin status
run ostree-diff-help 30 ostree diff --help
run ostree-fsck-help 30 ostree fsck --help
note ""

# Candidate (a): exploratory only. The exact argv/semantics are what we are trying to learn,
# so this tries the documented form against the booted deployment and records what happens.
# It is read-only. Skip with SKIP_OSTREE_DIFF=1 if it is slow on your host.
if [ -z "${SKIP_OSTREE_DIFF:-}" ]; then
    CHECKSUM=$(python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));print(next(x["checksum"] for x in d["deployments"] if x.get("booted")))' \
        "$OUT/rpm-ostree-status.out" 2>/dev/null || true)
    if [ -n "$CHECKSUM" ]; then
        note "== Q1 candidate (a): ostree diff <booted checksum> against the deployed tree (exploratory) =="
        run ostree-diff-usr "$OSTREE_TIMEOUT" ostree diff --fs-diff "$CHECKSUM" /usr
        note "  (a path-prefix mismatch here is itself a finding: record it, do not fix it on the host)"
    else
        note "could not read the booted checksum; skipping candidate (a)"
    fi
fi
note ""

note "== done =="
note "Bring back: $OUT/summary.txt"
note "Also useful if small: $OUT/rpm-va.err $OUT/rpm-ostree-status.out $OUT/findmnt-*.out $OUT/prepare-root-conf.out"
note "Do NOT bring rpm-va.out unless asked (large; lists paths)."
echo "wrote $OUT"

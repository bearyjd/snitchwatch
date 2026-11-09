#!/usr/bin/env python3
"""Read-only measurement helpers for the eBPF-vs-/proc comparison (Q05/Q06).

Subcommands (stdlib only, no root needed, nothing outside --out is written):
  sample        sample CPU/RSS/threads of a named process from /proc into a CSV
  summarize     mean/median/p95 CPU and RSS slope from a sample CSV
  unknown-rate  unknown-process rate from two cache-summary exports
  compare       proc-run vs ebpf-run manifests: CPU/RSS/unknown-rate + deltas

Plan: docs/superpowers/plans/2026-10-29-ebpf-q05-q06.md
"""
import argparse
import calendar
import csv
import io
import json
import math
import os
import stat
import statistics
import sys
import time
from pathlib import Path

COLUMNS = ["t_s", "iso_utc", "name", "pid", "cpu_pct", "rss_kib", "threads",
           "load1", "forks_per_s"]
COMM_LEN = 15  # kernel truncates /proc/<pid>/comm to 15 chars
EXPORT_FORMAT = "snitchwatch-cache-summary"
RUN_FORMAT = "snitchwatch-measure-run"
DURATION_TOLERANCE = 0.01  # runs may differ by 1% in declared duration
# Coverage is judged on the sample distribution, not on max(t_s) alone: the
# timestamps must span (max - min) >= 50% of durationS, number >= 3, and have no
# gap wider than 50% of durationS. A lone late sample, or just the two
# endpoints, therefore cannot stand in for a run.
MIN_SPAN_FRACTION = 0.5
MAX_GAP_FRACTION = 0.5
MIN_SAMPLES = 3
MAX_DURATION_S = 366 * 86400.0  # one year; larger is a typo or hostile
# The sampler stamps each row with iso_utc (UTC, whole seconds) taken right
# after the monotonic t_s, so iso_utc - t_s estimates the run's wall-clock
# start. Every row's estimate must lie within this many seconds of the export
# window's fromMs (at least WALL_MIN_SLACK_S, for the 1 s stamp resolution).
WALL_MIN_SLACK_S = 2.0
# Per-sample sanity bounds (well above any real host: 10000 cores, 1 PiB RSS),
# so per-timestamp sums of a bounded number of rows stay finite.
MAX_CPU_PCT = 1e6
MAX_RSS_KIB = 2 ** 40
ISO_FORMAT = "%Y-%m-%dT%H:%M:%SZ"


def read_process(proc, pid):
    """Return (cpu_ticks, rss_kib, threads) for pid. Raises OSError/ValueError."""
    stat = (proc / str(pid) / "stat").read_text()
    # comm may contain spaces and parentheses: split after the LAST ')'.
    rest = stat[stat.rindex(")") + 2:].split()
    ticks = int(rest[11]) + int(rest[12])  # utime + stime (fields 14, 15)
    rss = threads = None
    for line in (proc / str(pid) / "status").read_text().splitlines():
        if line.startswith("VmRSS:"):
            rss = int(line.split()[1])
        elif line.startswith("Threads:"):
            threads = int(line.split()[1])
    if rss is None or threads is None:
        raise ValueError(f"pid {pid}: VmRSS/Threads missing")
    return ticks, rss, threads


def find_pids(proc, name):
    """PIDs whose comm (15-char truncated) or argv[0] basename equals name."""
    pids = []
    for entry in sorted(proc.iterdir(), key=lambda p: p.name):
        if not entry.name.isdigit():
            continue
        try:
            comm = (entry / "comm").read_text().strip()
            argv0 = (entry / "cmdline").read_bytes().split(b"\0")[0].decode(errors="replace")
        except OSError:
            continue
        if comm == name[:COMM_LEN] or os.path.basename(argv0) == name:
            pids.append(int(entry.name))
    return pids


def cpu_percent(prev_ticks, cur_ticks, dt, clk_tck):
    """Percent of one core over dt seconds; None if not computable."""
    if dt <= 0 or cur_ticks < prev_ticks:
        return None
    return (cur_ticks - prev_ticks) / clk_tck / dt * 100.0


def _global(proc):
    forks = load1 = None
    try:
        for line in (proc / "stat").read_text().splitlines():
            if line.startswith("processes "):
                forks = int(line.split()[1])
        load1 = float((proc / "loadavg").read_text().split()[0])
    except (OSError, ValueError, IndexError):
        pass
    return forks, load1


def _fmt(v):
    return "" if v is None else repr(round(v, 3)) if isinstance(v, float) else str(v)


def _open_regular(path, flags, mode=0o600, dir_fd=None):
    """os.open that never blocks on a FIFO, never follows a symlink, and
    returns (fd, st) only for a regular file; otherwise closes and raises."""
    fd = os.open(path, flags | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, mode,
                 dir_fd=dir_fd)
    try:
        st = os.fstat(fd)
        if not stat.S_ISREG(st.st_mode):
            raise OSError(f"{path}: not a regular file")
        return fd, st
    except BaseException:
        os.close(fd)
        raise


def open_private(out, force=False):
    """Open `out` for writing as a private (0600) regular file.

    Default: exclusive creation, so an existing file (or any symlink, even a
    dangling one) is refused and never touched. With force, an existing
    REGULAR, singly-linked file is truncated and chmod'ed to 0600; symlinks,
    FIFOs/devices and hard-linked files (shared inode) are refused before any
    change is made.
    """
    flags = os.O_WRONLY | os.O_CREAT
    if not force:
        flags |= os.O_EXCL
    fd, st = _open_regular(out, flags)  # FileExistsError / ELOOP on symlink
    try:
        if force:
            if st.st_nlink > 1:
                raise OSError(f"{out}: has {st.st_nlink} hard links; refusing to modify "
                              "a shared file")
            os.fchmod(fd, 0o600)
            os.ftruncate(fd, 0)
    except BaseException:
        os.close(fd)
        raise
    return fd


def read_regular_text(path):
    """Read a user-supplied file as UTF-8 text; regular files only (no FIFO hang)."""
    fd, _ = _open_regular(path, os.O_RDONLY)
    with os.fdopen(fd, encoding="utf-8", newline="") as fh:
        return fh.read()


def _read_inside(root, rel):
    """Read root/rel as text, opening each component relative to its parent
    directory fd with O_NOFOLLOW: no symlink anywhere below root is followed,
    so the file read is the one inside root (no check-then-open window).
    Returns (text, (st_dev, st_ino))."""
    dfd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        for part in rel.parts[:-1]:
            nfd = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
                          | os.O_CLOEXEC, dir_fd=dfd)
            os.close(dfd)
            dfd = nfd
        fd, st = _open_regular(rel.parts[-1], os.O_RDONLY, dir_fd=dfd)
    finally:
        os.close(dfd)
    with os.fdopen(fd, encoding="utf-8", newline="") as fh:
        return fh.read(), (st.st_dev, st.st_ino)


def run(name, interval, duration, out, proc, clk_tck, sleep=time.sleep,
        now=time.monotonic, force=False):
    """Sample until duration elapses; returns the number of rows written."""
    out.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd = open_private(out, force)
    start = now()
    prev = {}  # pid -> (ticks, t)
    prev_forks = None
    prev_forks_t = None
    rows = 0
    with os.fdopen(fd, "w", newline="") as fh:
        writer = csv.writer(fh)
        writer.writerow(COLUMNS)
        while True:
            t = now()
            forks, load1 = _global(proc)
            fork_rate = None
            if forks is not None and prev_forks is not None and t > prev_forks_t:
                fork_rate = (forks - prev_forks) / (t - prev_forks_t)
            prev_forks, prev_forks_t = forks, t
            iso = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
            seen = []
            for pid in find_pids(proc, name):
                try:
                    ticks, rss, threads = read_process(proc, pid)
                except (OSError, ValueError):
                    continue  # exited between listing and reading
                cpu = None
                if pid in prev:
                    cpu = cpu_percent(prev[pid][0], ticks, t - prev[pid][1], clk_tck)
                prev[pid] = (ticks, t)
                seen.append((pid, cpu, rss, threads))
            if not seen:
                seen = [(None, None, None, None)]
            for pid, cpu, rss, threads in seen:
                writer.writerow([_fmt(t - start), iso, name, _fmt(pid), _fmt(cpu),
                                 _fmt(rss), _fmt(threads), _fmt(load1), _fmt(fork_rate)])
                rows += 1
            fh.flush()
            if t - start + interval > duration:
                return rows
            sleep(interval)


def summarize(rows):
    """Stats over sample rows (dicts as read by csv.DictReader)."""
    cpu = sorted(float(r["cpu_pct"]) for r in rows if r.get("cpu_pct") not in (None, ""))
    pts = [(float(r["t_s"]) / 3600.0, int(r["rss_kib"]) / 1024.0)
           for r in rows if r.get("rss_kib") not in (None, "")]
    out = {"samples": len(rows), "cpu_samples": len(cpu), "cpu_mean": None,
           "cpu_median": None, "cpu_p95": None, "rss_start_mib": None,
           "rss_end_mib": None, "rss_peak_mib": None, "rss_slope_mib_per_h": None}
    if cpu:
        out["cpu_mean"] = statistics.fmean(cpu)
        out["cpu_median"] = statistics.median(cpu)
        # p95 = upper nearest-rank: index int(0.95*n), clamped to the last sample.
        out["cpu_p95"] = cpu[min(len(cpu) - 1, int(0.95 * len(cpu)))]
    if pts:
        out["rss_start_mib"], out["rss_end_mib"] = pts[0][1], pts[-1][1]
        out["rss_peak_mib"] = max(p[1] for p in pts)
        mx = statistics.fmean(p[0] for p in pts)
        my = statistics.fmean(p[1] for p in pts)
        var = sum((p[0] - mx) ** 2 for p in pts)
        if var > 0:
            out["rss_slope_mib_per_h"] = sum((p[0] - mx) * (p[1] - my) for p in pts) / var
    return out


def _check_export(doc, label):
    if not isinstance(doc, dict) or doc.get("format") != EXPORT_FORMAT:
        raise ValueError(f"{label}: not a {EXPORT_FORMAT} document")
    if doc.get("schemaVersion") != 1:
        raise ValueError(f"{label}: unsupported schemaVersion {doc.get('schemaVersion')!r}")
    groups = doc.get("groups")
    if not isinstance(groups, list):
        raise ValueError(f"{label}: groups missing")
    total = 0
    for g in groups:
        c = g.get("count") if isinstance(g, dict) else None
        if type(c) is not int or c < 0:
            raise ValueError(f"{label}: group count must be a non-negative integer")
        total += c
    return total


GROUP_LIMITS = ("groupLimit", "artifactLimit", "wireLimit")


def _truncation_warnings(doc, label):
    """Every reported omission or limit, or a note that coverage is unreadable."""
    cov = doc.get("coverage")
    if not isinstance(cov, dict):
        return [f"{label}: coverage missing; completeness cannot be judged"]
    out = []
    ro = cov.get("rowOmissions")
    if not isinstance(ro, dict):
        out.append(f"{label}: rowOmissions missing; completeness cannot be judged")
    else:
        rows = sum(v for v in ro.values() if type(v) is int)
        if rows:
            out.append(f"{label}: {rows} rows omitted before grouping (rowOmissions)")
    unexamined = cov.get("unexaminedRetained", 0)
    if type(unexamined) is not int or unexamined:
        out.append(f"{label}: {unexamined} retained rows unexamined")
    go = cov.get("groupOmissions")
    if not isinstance(go, dict):
        out.append(f"{label}: groupOmissions missing; completeness cannot be judged")
    else:
        for kind in GROUP_LIMITS:
            item = go.get(kind)
            if not isinstance(item, dict) or type(item.get("groups")) is not int \
                    or type(item.get("observations")) is not int:
                out.append(f"{label}: groupOmissions.{kind} missing or malformed")
            elif item["groups"] or item["observations"]:
                out.append(f"{label}: groupOmissions.{kind} dropped {item['groups']} "
                           f"groups / {item['observations']} observations")
    return out


def unknown_rate(total_doc, missing_doc):
    """Unknown-process rate = observations with no process path / all observations.

    total_doc: export for the window with no application filter.
    missing_doc: export for the SAME window with the `missingPath` application
    filter. Counts are observations (cache rows), not sessions.
    """
    total = _check_export(total_doc, "total")
    unknown = _check_export(missing_doc, "missing-path")
    if not (missing_doc.get("filterKinds") or {}).get("application"):
        raise ValueError("missing-path export was not produced with the application filter")
    if (total_doc.get("filterKinds") or {}).get("application"):
        raise ValueError("total export must not use the application filter")
    if any("appBasename" in g for g in missing_doc["groups"]):
        raise ValueError("missing-path export discloses a name: wrong filter")
    tf = total_doc.get("filterKinds") or {}
    mf = missing_doc.get("filterKinds") or {}
    if tf.get("verdicts") or mf.get("verdicts"):
        raise ValueError("a verdict filter makes the rate invalid; export without it")
    rest = lambda f: {k: v for k, v in f.items() if k != "application"}  # noqa: E731
    if rest(tf) != rest(mf):
        raise ValueError("the exports' filters differ beyond the missing-path filter")
    if total_doc.get("requestedWindow") != missing_doc.get("requestedWindow"):
        raise ValueError("the two exports cover different requested windows")
    if unknown > total:
        raise ValueError(f"unknown ({unknown}) exceeds total ({total}): exports are inconsistent")
    warnings = (_truncation_warnings(total_doc, "total")
                + _truncation_warnings(missing_doc, "missing-path"))
    if warnings:
        warnings.append("the rate is NOT over the full window and may be biased")
    return {"total": total, "unknown": unknown,
            "rate": (unknown / total) if total else None,
            "truncated": bool(warnings), "warnings": warnings}


def _load_run(path, label):
    """Summarize one run manifest (paths inside it resolve next to it)."""
    doc = _load(path)
    if not isinstance(doc, dict) or doc.get("format") != RUN_FORMAT:
        raise ValueError(f"{label}: not a {RUN_FORMAT} document")
    if doc.get("schemaVersion") != 1:
        raise ValueError(f"{label}: unsupported schemaVersion {doc.get('schemaVersion')!r}")
    dur = doc.get("durationS")
    if isinstance(dur, bool) or not isinstance(dur, (int, float)) \
            or not math.isfinite(dur) or not 0 < dur <= MAX_DURATION_S:
        raise ValueError(f"{label}: durationS missing or not a finite positive number <= one year")
    workload = doc.get("workload")
    if not isinstance(workload, str) or not workload:
        raise ValueError(f"{label}: workload missing")
    root = Path(path).parent
    texts, ids = {}, set()
    for key in ("samplesCsv", "unknownTotal", "unknownMissing"):
        if not isinstance(doc.get(key), str) or not doc[key]:
            raise ValueError(f"{label}: {key} missing")
        rel = Path(doc[key])
        if rel.is_absolute() or ".." in rel.parts or not rel.parts:
            raise ValueError(f"{label}: {key} must be a relative path inside the manifest directory")
        try:
            texts[key], ident = _read_inside(root, rel)
        except OSError as e:
            raise ValueError(f"{label}: {key} must be a regular file inside the manifest "
                             f"directory reached without symlinks: {e}") from None
        ids.add(ident)
    rows = list(csv.DictReader(io.StringIO(texts["samplesCsv"])))
    points, ts = _check_rows(rows, label, dur)
    name = _run_name(rows, label)
    stats = summarize(points)
    if not stats["cpu_samples"]:
        raise ValueError(f"{label}: no cpu samples in {doc['samplesCsv']}")
    total_doc = json.loads(texts["unknownTotal"])
    missing_doc = json.loads(texts["unknownMissing"])
    _check_window(total_doc, f"{label} total", dur)
    _check_window(missing_doc, f"{label} missing-path", dur)
    _check_wall_clock(rows, ts, total_doc, label, dur)
    filters = {"total": _filters(total_doc, f"{label} total"),
               "missing": _filters(missing_doc, f"{label} missing-path")}
    unk = unknown_rate(total_doc, missing_doc)
    return {"duration_s": dur, "workload": workload, "name": name, "cpu_mean": stats["cpu_mean"],
            "cpu_p95": stats["cpu_p95"], "rss_start_mib": stats["rss_start_mib"],
            "rss_end_mib": stats["rss_end_mib"], "unknown_rate": unk["rate"],
            "filters": filters, "files": ids, "warnings": unk["warnings"]}


def _finite(value, what, label, nonneg=True):
    try:
        v = float(value)
    except (TypeError, ValueError):
        raise ValueError(f"{label}: {what} is not a number: {value!r}") from None
    if not math.isfinite(v) or (nonneg and v < 0):
        raise ValueError(f"{label}: {what} must be finite and non-negative: {value!r}")
    return v


def _check_rows(rows, label, dur):
    """Validate sample values, aggregate per timestamp, check coverage.

    Every t_s must be finite, in order and within [0, durationS * (1 +
    tolerance)]. The sampler writes one row per matching PID at each tick, all
    with the same t_s, so rows are grouped by t_s: a timestamp yields one point
    whose cpu_pct and rss_kib are the SUMS over its rows, and only if every row
    at that timestamp has both values (CPU and RSS then come from the same
    process set). Only such points count toward sample count, span and gaps,
    so duplicate timestamps count once. A pid may appear at most once per
    timestamp (a repeated (t_s, pid) row would double the totals), and each
    cpu_pct/rss_kib must not exceed MAX_CPU_PCT/MAX_RSS_KIB; sums must be
    finite. Returns (points, per-row t_s).
    """
    ts = [_finite(r.get("t_s"), "t_s", label) for r in rows]
    if any(t > dur * (1 + DURATION_TOLERANCE) for t in ts):
        raise ValueError(f"{label}: t_s beyond durationS {dur:g}s")
    if any(b < a for a, b in zip(ts, ts[1:])):
        raise ValueError(f"{label}: t_s rows out of order")
    groups, seen = {}, set()
    for t, r in zip(ts, rows):
        if (t, r.get("pid")) in seen:
            raise ValueError(f"{label}: duplicate pid {r.get('pid')!r} at t_s {t:g}")
        seen.add((t, r.get("pid")))
        nums = []
        for col, limit in (("cpu_pct", MAX_CPU_PCT), ("rss_kib", MAX_RSS_KIB)):
            v = r.get(col)
            n = None if v in (None, "") else _finite(v, col, label)
            if n is not None and n > limit:
                raise ValueError(f"{label}: {col} {v} exceeds the sanity bound {limit:g}")
            nums.append(n)
        groups.setdefault(t, []).append(nums)
    points = [{"t_s": t, "cpu_pct": sum(n[0] for n in g), "rss_kib": sum(n[1] for n in g)}
              for t, g in groups.items() if all(None not in n for n in g)]
    if not all(math.isfinite(p["cpu_pct"]) and math.isfinite(p["rss_kib"]) for p in points):
        raise ValueError(f"{label}: per-timestamp cpu_pct/rss_kib sum is not finite")
    counted = [p["t_s"] for p in points]
    if len(counted) < MIN_SAMPLES:
        raise ValueError(f"{label}: only {len(counted)} distinct timestamps with cpu and rss "
                         f"for every process; need >= {MIN_SAMPLES} samples")
    if counted[-1] - counted[0] < MIN_SPAN_FRACTION * dur:
        raise ValueError(f"{label}: sample span {counted[-1] - counted[0]:g}s is below "
                         f"{MIN_SPAN_FRACTION:.0%} of durationS {dur:g}s")
    gap = max(b - a for a, b in zip(counted, counted[1:]))
    if gap > MAX_GAP_FRACTION * dur:
        raise ValueError(f"{label}: sample gap {gap:g}s exceeds "
                         f"{MAX_GAP_FRACTION:.0%} of durationS {dur:g}s")
    return points, ts


def _run_name(rows, label):
    """The sampled process name: present on every row and the same throughout."""
    names = {r.get("name") for r in rows}
    if None in names or "" in names:
        raise ValueError(f"{label}: sample process name missing")
    if len(names) != 1:
        raise ValueError(f"{label}: sample process names differ within the run: {sorted(names)}")
    return names.pop()


def _check_wall_clock(rows, ts, doc, label, dur):
    """The CSV's wall clock (iso_utc - t_s per row) must agree with the export
    window start; window presence/length is checked by _check_window."""
    start = _finite(doc["requestedWindow"]["fromMs"], "window fromMs", label) / 1000.0
    slack = max(WALL_MIN_SLACK_S, DURATION_TOLERANCE * dur)
    for t, r in zip(ts, rows):
        try:
            wall = calendar.timegm(time.strptime(r.get("iso_utc") or "", ISO_FORMAT))
        except ValueError:
            raise ValueError(f"{label}: iso_utc missing or not {ISO_FORMAT}: "
                             f"{r.get('iso_utc')!r}") from None
        if abs(wall - t - start) > slack:
            raise ValueError(f"{label}: CSV wall-clock start {wall - t:.0f} disagrees with "
                             f"export window fromMs {start:.0f}s by more than {slack:g}s")


def _check_window(doc, label, dur):
    """The export's requestedWindow [fromMs,toMs) must be durationS long (+-1%)."""
    w = doc.get("requestedWindow") if isinstance(doc, dict) else None
    if not isinstance(w, dict) or "fromMs" not in w or "toMs" not in w:
        raise ValueError(f"{label}: requestedWindow.fromMs/toMs missing; cannot check window")
    length = (_finite(w["toMs"], "window toMs", label)
              - _finite(w["fromMs"], "window fromMs", label)) / 1000.0
    if abs(length - dur) > DURATION_TOLERANCE * dur:
        raise ValueError(f"{label}: export window {length:g}s does not match durationS {dur:g}s")


def _filters(doc, label):
    f = doc.get("filterKinds") if isinstance(doc, dict) else None
    if f is None:
        return {}
    if not isinstance(f, dict) or not all(isinstance(k, str) for k in f):
        raise ValueError(f"{label}: filterKinds must be an object keyed by strings")
    return f


DELTA_KEYS = ("cpu_mean", "cpu_p95", "rss_start_mib", "rss_end_mib", "unknown_rate")


def compare(proc_manifest, ebpf_manifest):
    """Side-by-side stats for two runs and ebpf-minus-proc deltas.

    Refuses runs whose declared duration (within 1%), workload or sampled
    process name differ: such a comparison would measure the load, not the
    collector. CPU per timestamp is summed over all PIDs sampled then.
    """
    if Path(proc_manifest).resolve() == Path(ebpf_manifest).resolve():
        raise ValueError("cannot compare a manifest with itself")
    runs = {"proc": _load_run(proc_manifest, "proc"), "ebpf": _load_run(ebpf_manifest, "ebpf")}
    p, e = runs["proc"], runs["ebpf"]
    if abs(p["duration_s"] - e["duration_s"]) > DURATION_TOLERANCE * max(p["duration_s"], e["duration_s"]):
        raise ValueError(f"mismatched duration: proc {p['duration_s']}s vs ebpf {e['duration_s']}s")
    if p["workload"] != e["workload"]:
        raise ValueError(f"mismatched workload: proc {p['workload']!r} vs ebpf {e['workload']!r}")
    if p["name"] != e["name"]:
        raise ValueError(f"mismatched sample process name: proc {p['name']!r} vs ebpf {e['name']!r}")
    # Exports with different filters/scope are different populations: refuse.
    if p["filters"] != e["filters"]:
        raise ValueError(f"mismatched export filters: proc {p['filters']} vs ebpf {e['filters']}")
    # Identity is (st_dev, st_ino) of the opened file, so hard links alias too.
    if p["files"] & e["files"]:
        raise ValueError("proc and ebpf manifests reference the same artifact file(s)")
    for r in (p, e):
        del r["files"]
    runs["biased"] = bool(p["warnings"] or e["warnings"])
    runs["delta"] = {k: None if p[k] is None or e[k] is None
                     or (k == "unknown_rate" and runs["biased"]) else e[k] - p[k]
                     for k in DELTA_KEYS}
    return runs


def _load(path):
    return json.loads(read_regular_text(path))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("sample")
    s.add_argument("--name", required=True)
    s.add_argument("--interval", type=float, default=60.0)
    s.add_argument("--duration", type=float, required=True, help="seconds")
    s.add_argument("--out", type=Path, required=True)
    s.add_argument("--proc", type=Path, default=Path("/proc"))
    s.add_argument("--force", action="store_true",
                   help="overwrite an existing regular file (symlinks still refused)")
    m = sub.add_parser("summarize")
    m.add_argument("csv", type=Path)
    u = sub.add_parser("unknown-rate")
    u.add_argument("--total", required=True)
    u.add_argument("--missing-path", required=True)
    c = sub.add_parser("compare")
    c.add_argument("proc", type=Path, help="run manifest of the /proc run")
    c.add_argument("ebpf", type=Path, help="run manifest of the eBPF run")
    a = ap.parse_args(argv)
    if a.cmd == "sample":
        if a.interval <= 0 or a.duration < 0:
            ap.error("interval must be > 0 and duration >= 0")
        try:
            n = run(a.name, a.interval, a.duration, a.out, a.proc,
                    os.sysconf("SC_CLK_TCK"), force=a.force)
        except OSError as e:
            print(f"error: cannot write {a.out}: {e} (existing files need --force)",
                  file=sys.stderr)
            return 1
        print(f"wrote {n} rows to {a.out}")
    elif a.cmd == "summarize":
        try:
            rows = list(csv.DictReader(io.StringIO(read_regular_text(a.csv))))
        except OSError as e:
            print(f"error: {e}", file=sys.stderr)
            return 1
        print(json.dumps(summarize(rows), indent=2))
    elif a.cmd == "compare":
        try:
            print(json.dumps(compare(a.proc, a.ebpf), indent=2, allow_nan=False))
        except (ValueError, OSError, csv.Error, TypeError, KeyError,
                OverflowError, AttributeError) as e:
            print(f"error: {e}", file=sys.stderr)
            return 1
    else:
        try:
            print(json.dumps(unknown_rate(_load(a.total), _load(a.missing_path)), indent=2))
        except (ValueError, OSError) as e:
            print(f"error: {e}", file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

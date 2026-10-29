#!/usr/bin/env python3
"""Read-only measurement helpers for the eBPF-vs-/proc comparison (Q05/Q06).

Subcommands (stdlib only, no root needed, nothing outside --out is written):
  sample        sample CPU/RSS/threads of a named process from /proc into a CSV
  summarize     mean/median/p95 CPU and RSS slope from a sample CSV
  unknown-rate  unknown-process rate from two cache-summary exports

Plan: docs/superpowers/plans/2026-10-29-ebpf-q05-q06.md
"""
import argparse
import csv
import json
import os
import statistics
import sys
import time
from pathlib import Path

COLUMNS = ["t_s", "iso_utc", "name", "pid", "cpu_pct", "rss_kib", "threads",
           "load1", "forks_per_s"]
COMM_LEN = 15  # kernel truncates /proc/<pid>/comm to 15 chars
EXPORT_FORMAT = "snitchwatch-cache-summary"


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


def run(name, interval, duration, out, proc, clk_tck, sleep=time.sleep, now=time.monotonic):
    """Sample until duration elapses; returns the number of rows written."""
    out.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd = os.open(out, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
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


def _omitted(doc):
    cov = doc.get("coverage") or {}
    ro = cov.get("rowOmissions") or {}
    return sum(v for v in ro.values() if type(v) is int), cov.get("unexaminedRetained", 0)


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
    if total_doc.get("requestedWindow") != missing_doc.get("requestedWindow"):
        raise ValueError("the two exports cover different requested windows")
    if unknown > total:
        raise ValueError(f"unknown ({unknown}) exceeds total ({total}): exports are inconsistent")
    warnings = []
    for label, doc in (("total", total_doc), ("missing-path", missing_doc)):
        rows, unexamined = _omitted(doc)
        if rows or unexamined:
            warnings.append(f"{label}: {rows} rows omitted, {unexamined} retained rows "
                            "unexamined; the rate is not over the full window")
    return {"total": total, "unknown": unknown,
            "rate": (unknown / total) if total else None, "warnings": warnings}


def _load(path):
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("sample")
    s.add_argument("--name", required=True)
    s.add_argument("--interval", type=float, default=60.0)
    s.add_argument("--duration", type=float, required=True, help="seconds")
    s.add_argument("--out", type=Path, required=True)
    s.add_argument("--proc", type=Path, default=Path("/proc"))
    m = sub.add_parser("summarize")
    m.add_argument("csv", type=Path)
    u = sub.add_parser("unknown-rate")
    u.add_argument("--total", required=True)
    u.add_argument("--missing-path", required=True)
    a = ap.parse_args(argv)
    if a.cmd == "sample":
        if a.interval <= 0 or a.duration < 0:
            ap.error("interval must be > 0 and duration >= 0")
        n = run(a.name, a.interval, a.duration, a.out, a.proc, os.sysconf("SC_CLK_TCK"))
        print(f"wrote {n} rows to {a.out}")
    elif a.cmd == "summarize":
        with open(a.csv, newline="") as fh:
            print(json.dumps(summarize(list(csv.DictReader(fh))), indent=2))
    else:
        try:
            print(json.dumps(unknown_rate(_load(a.total), _load(a.missing_path)), indent=2))
        except (ValueError, OSError) as e:
            print(f"error: {e}", file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

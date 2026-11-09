#!/usr/bin/env python3
"""Offline tests for measure.py against synthetic /proc trees and exports."""
import csv
import json
import signal
import os
import stat
import sys
import tempfile
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import measure  # noqa: E402


def write_proc(root, pid, comm, utime, stime, rss_kib, threads, argv0=None):
    d = Path(root) / str(pid)
    d.mkdir(parents=True, exist_ok=True)
    # comm deliberately contains a space and ')' to exercise the parser.
    fields = ["S", "1", "1", "1", "0", "-1", "0", "0", "0", "0", "0",
              str(utime), str(stime)] + ["0"] * 8
    (d / "stat").write_text(f"{pid} ({comm}) " + " ".join(fields) + "\n")
    (d / "status").write_text(
        f"Name:\t{comm}\nVmRSS:\t{rss_kib} kB\nThreads:\t{threads}\n")
    (d / "comm").write_text(comm[:15] + "\n")
    (d / "cmdline").write_bytes(((argv0 or comm) + "\0--x\0").encode())


def read_rows(path):
    with open(path, newline="") as fh:
        return list(csv.DictReader(fh))


def write_global(root, forks, load1="0.50"):
    (Path(root) / "stat").write_text(f"cpu 1 2 3\nprocesses {forks}\n")
    (Path(root) / "loadavg").write_text(f"{load1} 0.4 0.3 1/100 999\n")


class Sampling(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = self.tmp.name

    def tearDown(self):
        self.tmp.cleanup()

    def test_parse_stat_with_odd_comm(self):
        write_proc(self.root, 10, "open snitch) d", 120, 30, 2048, 4)
        ticks, rss, threads = measure.read_process(Path(self.root), 10)
        self.assertEqual((ticks, rss, threads), (150, 2048, 4))

    def test_find_pids_exact_and_truncated_comm(self):
        write_proc(self.root, 10, "opensnitchd", 1, 1, 1, 1)
        write_proc(self.root, 11, "opensnitchd-helper", 1, 1, 1, 1)
        write_proc(self.root, 12, "snitchwatch-bridge", 1, 1, 1, 1,
                   argv0="/proc/self/exe")
        (Path(self.root) / "self").mkdir()
        self.assertEqual(measure.find_pids(Path(self.root), "opensnitchd"), [10])
        # comm is truncated to 15 chars by the kernel
        self.assertEqual(measure.find_pids(Path(self.root), "snitchwatch-bridge"), [12])

    def test_find_pids_matches_argv0_basename(self):
        write_proc(self.root, 20, "tokio-rt-worker", 1, 1, 1, 1,
                   argv0="/usr/bin/snitchwatch-bridge")
        self.assertEqual(measure.find_pids(Path(self.root), "snitchwatch-bridge"), [20])

    def test_cpu_percent(self):
        self.assertAlmostEqual(measure.cpu_percent(100, 150, 1.0, 100), 50.0)
        self.assertAlmostEqual(measure.cpu_percent(100, 100, 2.0, 100), 0.0)
        self.assertIsNone(measure.cpu_percent(100, 150, 0.0, 100))
        # counter went backwards (pid reuse): no value, never negative
        self.assertIsNone(measure.cpu_percent(500, 100, 1.0, 100))

    def test_run_writes_csv_with_cpu_delta_and_private_mode(self):
        write_proc(self.root, 10, "opensnitchd", 100, 0, 1000, 2)
        write_global(self.root, 1000)
        clock = {"t": 0.0, "n": 0}

        def now():
            return clock["t"]

        def sleep(s):
            clock["t"] += s
            clock["n"] += 1
            write_proc(self.root, 10, "opensnitchd", 100 + 50 * clock["n"], 0,
                       1000 + 100 * clock["n"], 2)
            write_global(self.root, 1000 + 10 * clock["n"])

        out = Path(self.root) / "out" / "run.csv"
        n = measure.run("opensnitchd", 1.0, 3.0, out, Path(self.root), 100, sleep, now)
        self.assertEqual(n, 4)
        self.assertEqual(stat.S_IMODE(os.stat(out).st_mode), 0o600)
        rows = read_rows(out)
        self.assertEqual(len(rows), 4)
        self.assertEqual(rows[0]["cpu_pct"], "")  # no previous sample
        self.assertAlmostEqual(float(rows[1]["cpu_pct"]), 50.0)
        self.assertEqual(rows[3]["rss_kib"], "1300")
        self.assertEqual(rows[1]["forks_per_s"], "10.0")
        self.assertEqual(rows[1]["pid"], "10")

    def test_run_records_absent_process_without_crashing(self):
        write_global(self.root, 1)
        clock = {"t": 0.0}

        def sleep(s):
            clock["t"] += s

        out = Path(self.root) / "o.csv"
        n = measure.run("opensnitchd", 1.0, 1.0, out, Path(self.root), 100,
                        sleep, lambda: clock["t"])
        rows = read_rows(out)
        self.assertEqual(n, 2)
        self.assertEqual(rows[0]["pid"], "")
        self.assertEqual(rows[0]["rss_kib"], "")

    def test_summarize(self):
        rows = [
            {"t_s": str(i * 3600), "cpu_pct": c, "rss_kib": str(1024 * (100 + 10 * i))}
            for i, c in enumerate(["", "10", "20", "30", "40"])
        ]
        s = measure.summarize(rows)
        self.assertAlmostEqual(s["cpu_mean"], 25.0)
        self.assertAlmostEqual(s["cpu_median"], 25.0)
        self.assertAlmostEqual(s["rss_slope_mib_per_h"], 10.0)
        self.assertEqual(s["rss_peak_mib"], 140.0)
        self.assertEqual(s["samples"], 5)


def export(groups, completeness="incomplete", omissions=0, app_filter=False,
           window=(10, 30), verdicts=False):
    return {
        "format": "snitchwatch-cache-summary",
        "schemaVersion": 1,
        "requestedWindow": {"fromMs": window[0], "toMs": window[1]},
        "filterKinds": {"timeWindow": True, "application": app_filter, "verdicts": verdicts},
        "coverage": {
            "completeness": completeness,
            "unexaminedRetained": omissions,
            "rowOmissions": {"inspectionLimit": omissions, "fieldTooLong": 0,
                             "inputBudget": 0, "invalidTimestamp": 0},
            "groupOmissions": {k: {"groups": 0, "observations": 0}
                               for k in ("groupLimit", "artifactLimit", "wireLimit")},
        },
        "groups": [dict({"id": f"g{i:06d}", "appRef": f"a-{i}"}, **g)
                   for i, g in enumerate(groups, 1)],
    }


class OutputSafety(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        write_global(self.root, 1)

    def tearDown(self):
        self.tmp.cleanup()

    def _run(self, out, force=False):
        return measure.run("opensnitchd", 1.0, 0.0, out, self.root, 100,
                           lambda s: None, lambda: 0.0, force=force)

    def test_existing_file_refused_and_untouched(self):
        out = self.root / "keep.csv"
        out.write_text("precious")
        out.chmod(0o644)
        with self.assertRaises(FileExistsError):
            self._run(out)
        self.assertEqual(out.read_text(), "precious")
        self.assertEqual(stat.S_IMODE(os.stat(out).st_mode), 0o644)

    def test_symlink_refused_even_with_force(self):
        target = self.root / "target.txt"
        target.write_text("victim")
        link = self.root / "link.csv"
        link.symlink_to(target)
        for force in (False, True):
            with self.assertRaises(OSError):
                self._run(link, force=force)
        self.assertEqual(target.read_text(), "victim")

    def test_dangling_symlink_refused(self):
        link = self.root / "dangling.csv"
        link.symlink_to(self.root / "nowhere")
        with self.assertRaises(OSError):
            self._run(link)
        self.assertFalse((self.root / "nowhere").exists())

    def test_force_overwrites_regular_file_and_fixes_mode(self):
        out = self.root / "old.csv"
        out.write_text("old data " * 2000)
        out.chmod(0o644)
        self._run(out, force=True)
        self.assertNotIn("old data", out.read_text())
        self.assertEqual(stat.S_IMODE(os.stat(out).st_mode), 0o600)
        self.assertTrue(out.read_text().startswith("t_s,"))

    def test_force_refuses_non_regular_file(self):
        with self.assertRaisesRegex(OSError, "not a regular file"):
            self._run(Path("/dev/null"), force=True)

    def test_force_refuses_hardlinked_file_and_leaves_both_names(self):
        out = self.root / "a.csv"
        out.write_text("shared content")
        out.chmod(0o644)
        other = self.root / "b.csv"
        os.link(out, other)
        with self.assertRaisesRegex(OSError, "link"):
            self._run(out, force=True)
        self.assertEqual(other.read_text(), "shared content")
        self.assertEqual(stat.S_IMODE(os.stat(other).st_mode), 0o644)

    def _with_alarm(self, fn):
        def boom(*_):
            raise TimeoutError("blocked on FIFO")
        old = signal.signal(signal.SIGALRM, boom)
        signal.alarm(3)
        try:
            return fn()
        finally:
            signal.alarm(0)
            signal.signal(signal.SIGALRM, old)

    def test_fifo_refused_promptly_with_and_without_force(self):
        fifo = self.root / "pipe.csv"
        os.mkfifo(fifo)
        for force in (False, True):
            with self.assertRaises(OSError):
                self._with_alarm(lambda: self._run(fifo, force=force))

    def test_reading_a_fifo_is_refused_promptly(self):
        fifo = self.root / "in.json"
        os.mkfifo(fifo)
        with self.assertRaisesRegex(OSError, "regular"):
            self._with_alarm(lambda: measure.read_regular_text(fifo))

    def test_read_regular_text_reads_regular_file(self):
        f = self.root / "ok.json"
        f.write_text('{"a": 1}')
        self.assertEqual(measure.read_regular_text(f), '{"a": 1}')

    def test_new_file_is_private(self):
        out = self.root / "new.csv"
        self._run(out)
        self.assertEqual(stat.S_IMODE(os.stat(out).st_mode), 0o600)


class UnknownRate(unittest.TestCase):
    def test_rate_is_missing_over_total_by_observation_count(self):
        total = export([{"count": 6, "appBasename": "curl"}, {"count": 3}, {"count": 1}])
        missing = export([{"count": 3}, {"count": 1}], app_filter=True)
        r = measure.unknown_rate(total, missing)
        self.assertEqual((r["total"], r["unknown"]), (10, 4))
        self.assertAlmostEqual(r["rate"], 0.4)
        self.assertEqual(r["warnings"], [])

    def test_zero_total_has_no_rate(self):
        r = measure.unknown_rate(export([]), export([], app_filter=True))
        self.assertIsNone(r["rate"])

    def test_unknown_cannot_exceed_total(self):
        with self.assertRaises(ValueError):
            measure.unknown_rate(export([{"count": 1}]),
                                 export([{"count": 2}], app_filter=True))

    def test_missing_export_must_be_app_filtered(self):
        with self.assertRaises(ValueError):
            measure.unknown_rate(export([{"count": 2}]), export([{"count": 1}]))

    def test_missing_export_groups_never_disclose_a_name(self):
        with self.assertRaises(ValueError):
            measure.unknown_rate(export([{"count": 2}]),
                                 export([{"count": 1, "appBasename": "x"}], app_filter=True))

    def test_windows_must_match(self):
        with self.assertRaises(ValueError):
            measure.unknown_rate(export([{"count": 2}], window=(1, 2)),
                                 export([{"count": 1}], app_filter=True, window=(1, 3)))

    def test_rejects_wrong_format_or_version(self):
        bad = export([{"count": 1}])
        bad["schemaVersion"] = 2
        with self.assertRaises(ValueError):
            measure.unknown_rate(bad, export([], app_filter=True))
        bad2 = export([{"count": 1}])
        bad2["format"] = "other"
        with self.assertRaises(ValueError):
            measure.unknown_rate(bad2, export([], app_filter=True))

    def test_bad_counts_rejected(self):
        for c in (-1, 1.5, True, "3", None):
            with self.assertRaises(ValueError, msg=repr(c)):
                measure.unknown_rate(export([{"count": c}]), export([], app_filter=True))

    def test_negative_count_rejected_even_when_totals_look_consistent(self):
        with self.assertRaises(ValueError):
            measure.unknown_rate(export([{"count": -1}, {"count": 5}]),
                                 export([], app_filter=True))

    def test_each_omission_kind_warns_alone(self):
        for rows, unexamined in ((3, 0), (0, 3)):
            doc = export([{"count": 4}])
            doc["coverage"]["rowOmissions"]["inspectionLimit"] = rows
            doc["coverage"]["unexaminedRetained"] = unexamined
            r = measure.unknown_rate(doc, export([{"count": 1}], app_filter=True))
            want = "omitted before grouping" if rows else "unexamined"
            hits = [w for w in r["warnings"] if want in w]
            self.assertEqual(len(hits), 1, (rows, unexamined, r["warnings"]))

    def test_group_omissions_warn_and_flag_each_kind(self):
        for kind in ("groupLimit", "artifactLimit", "wireLimit"):
            for side in ("total", "missing"):
                total = export([{"count": 4}])
                missing = export([{"count": 1}], app_filter=True)
                doc = total if side == "total" else missing
                doc["coverage"]["groupOmissions"][kind] = {"groups": 2, "observations": 9}
                r = measure.unknown_rate(total, missing)
                self.assertTrue(r["truncated"], (kind, side))
                self.assertTrue(any(kind in w for w in r["warnings"]), (kind, side))

    def test_group_omission_with_only_groups_nonzero_still_flags(self):
        total = export([{"count": 4}])
        total["coverage"]["groupOmissions"]["wireLimit"] = {"groups": 1, "observations": 0}
        self.assertTrue(measure.unknown_rate(
            total, export([{"count": 1}], app_filter=True))["truncated"])

    def test_group_omission_observations_only_flags(self):
        total = export([{"count": 4}])
        total["coverage"]["groupOmissions"]["groupLimit"] = {"groups": 0, "observations": 5}
        self.assertTrue(measure.unknown_rate(
            total, export([{"count": 1}], app_filter=True))["truncated"])

    def test_missing_coverage_object_is_truncated_not_a_crash(self):
        total = export([{"count": 4}])
        del total["coverage"]
        r = measure.unknown_rate(total, export([{"count": 1}], app_filter=True))
        self.assertTrue(r["truncated"])

    def test_verdict_filter_on_both_sides_rejected(self):
        with self.assertRaises(ValueError):
            measure.unknown_rate(export([{"count": 4}], verdicts=True),
                                 export([{"count": 1}], app_filter=True, verdicts=True))

    def test_clean_exports_not_truncated(self):
        r = measure.unknown_rate(export([{"count": 4}]),
                                 export([{"count": 1}], app_filter=True))
        self.assertFalse(r["truncated"])
        self.assertEqual(r["warnings"], [])

    def test_row_omissions_set_truncated_flag(self):
        r = measure.unknown_rate(export([{"count": 4}], omissions=2),
                                 export([{"count": 1}], app_filter=True))
        self.assertTrue(r["truncated"])

    def test_missing_coverage_data_is_treated_as_truncated(self):
        total = export([{"count": 4}])
        del total["coverage"]["groupOmissions"]
        r = measure.unknown_rate(total, export([{"count": 1}], app_filter=True))
        self.assertTrue(r["truncated"])

    def test_verdict_filter_on_either_side_rejected(self):
        with self.assertRaises(ValueError):
            measure.unknown_rate(export([{"count": 4}], verdicts=True),
                                 export([{"count": 1}], app_filter=True))
        with self.assertRaises(ValueError):
            measure.unknown_rate(export([{"count": 4}]),
                                 export([{"count": 1}], app_filter=True, verdicts=True))

    def test_filter_kinds_must_otherwise_match(self):
        total = export([{"count": 4}])
        missing = export([{"count": 1}], app_filter=True)
        missing["filterKinds"]["timeWindow"] = False
        with self.assertRaises(ValueError):
            measure.unknown_rate(total, missing)
        total2 = export([{"count": 4}])
        total2["filterKinds"]["extra"] = True
        with self.assertRaises(ValueError):
            measure.unknown_rate(total2, export([{"count": 1}], app_filter=True))

    def test_omissions_produce_warning(self):
        total = export([{"count": 4}], omissions=7)
        r = measure.unknown_rate(total, export([{"count": 1}], app_filter=True))
        self.assertTrue(any("omitted" in w or "unexamined" in w for w in r["warnings"]))

    def test_real_fixture_shape_is_accepted(self):
        fx = Path(__file__).resolve().parents[2] / "tests/fixtures/analysis-export/summary-v1.json"
        data = json.loads(fx.read_text())
        miss = json.loads(json.dumps(data))
        miss["filterKinds"]["application"] = True
        miss["groups"] = [g for g in miss["groups"] if "appBasename" not in g]
        r = measure.unknown_rate(data, miss)
        self.assertEqual((r["total"], r["unknown"]), (3, 1))


START_MS = 1_700_000_000_000  # export window start; CSV iso_utc derives from it


class Compare(unittest.TestCase):
    def make_run(self, root, label, cpus, rss, unknown=10, dur=86400, workload="w1",
                 truncated=False, span=None, ts=None, filters=None, win_s=None,
                 name="d", pids=None, iso_shift=0, iso=None):
        root = Path(root)
        with open(root / f"{label}.csv", "w", newline="") as fh:
            w = csv.writer(fh)
            w.writerow(measure.COLUMNS)
            span = dur if span is None else span
            for i, (c, r) in enumerate(zip(cpus, rss)):
                t = span * i / max(1, len(cpus) - 1) if ts is None else ts[i]
                try:
                    wall = START_MS / 1000 + float(t) + iso_shift
                    stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(wall))
                except (ValueError, OverflowError):
                    stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(START_MS / 1000))
                stamp = stamp if iso is None else iso[i]
                nm = name[i] if isinstance(name, list) else name
                w.writerow([t, stamp, nm, 1 if pids is None else pids[i], c, r, 4, "", ""])
        doc = lambda n, flt: {"format": measure.EXPORT_FORMAT, "schemaVersion": 1,  # noqa: E731
                              "filterKinds": {"application": flt, **(filters or {})},
                              "requestedWindow": ({"fromMs": START_MS,
                                                 "toMs": START_MS + int((dur if win_s is None else win_s) * 1000)}
                                                if win_s != "none" else {"a": 1}),
                              "coverage": {"rowOmissions": {}, "unexaminedRetained": 0,
                                           "groupOmissions": {k: {"groups": 0, "observations": 0}
                                                              for k in measure.GROUP_LIMITS}},
                              "groups": [{"count": n}]}
        total, miss = doc(100, False), doc(unknown, True)
        if truncated:
            miss["coverage"]["rowOmissions"] = {"x": 5}
        (root / f"{label}-t.json").write_text(json.dumps(total))
        (root / f"{label}-m.json").write_text(json.dumps(miss))
        man = root / f"{label}.json"
        man.write_text(json.dumps({
            "format": measure.RUN_FORMAT, "schemaVersion": 1, "label": label,
            "durationS": dur, "workload": workload, "samplesCsv": f"{label}.csv",
            "unknownTotal": f"{label}-t.json", "unknownMissing": f"{label}-m.json"}))
        return man

    def test_compare_reports_both_runs_and_deltas(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0, 2.0, 3.0], [102400, 150000, 204800], 20)
            b = self.make_run(d, "ebpf", [0.5, 1.0, 1.5], [102400, 120000, 153600], 10)
            r = measure.compare(a, b)
        self.assertAlmostEqual(r["proc"]["cpu_mean"], 2.0)
        self.assertAlmostEqual(r["ebpf"]["cpu_mean"], 1.0)
        self.assertEqual((r["proc"]["rss_start_mib"], r["proc"]["rss_end_mib"]), (100.0, 200.0))
        self.assertAlmostEqual(r["proc"]["unknown_rate"], 0.2)
        self.assertAlmostEqual(r["delta"]["cpu_mean"], -1.0)
        self.assertAlmostEqual(r["delta"]["rss_end_mib"], -50.0)
        self.assertAlmostEqual(r["delta"]["unknown_rate"], -0.1)
        self.assertIn("cpu_p95", r["delta"])

    def test_mismatched_duration_refused(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3, dur=43200)
            with self.assertRaisesRegex(ValueError, "duration"):
                measure.compare(a, b)

    def test_mismatched_workload_refused(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3, workload="w2")
            with self.assertRaisesRegex(ValueError, "workload"):
                measure.compare(a, b)

    def test_missing_metadata_refused(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3)
            doc = json.loads(b.read_text())
            del doc["workload"]
            b.write_text(json.dumps(doc))
            with self.assertRaisesRegex(ValueError, "workload"):
                measure.compare(a, b)

    def test_wrong_format_refused(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = Path(d) / "bad.json"
            b.write_text("{}")
            with self.assertRaisesRegex(ValueError, "not a"):
                measure.compare(a, b)

    def test_cli_error_exit_code_on_mismatch(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3, workload="w2")
            self.assertEqual(measure.main(["compare", str(a), str(b)]), 1)
            c = self.make_run(d, "ebpf2", [1.0] * 3, [1024] * 3)
            self.assertEqual(measure.main(["compare", str(a), str(c)]), 0)

    def edit(self, man, **kw):
        doc = json.loads(man.read_text())
        doc.update(kw)
        man.write_text(json.dumps(doc))

    def test_non_finite_duration_refused(self):
        for bad in ("NaN", "Infinity", "-Infinity"):
            with tempfile.TemporaryDirectory() as d:
                a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
                b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3)
                txt = b.read_text().replace("86400", bad)
                b.write_text(txt)
                with self.assertRaisesRegex(ValueError, "durationS"):
                    measure.compare(a, b)

    def test_manifest_paths_must_stay_inside_manifest_dir(self):
        for bad in ("../outside.csv", "/etc/passwd", "sub/../../x.csv"):
            with tempfile.TemporaryDirectory() as d:
                a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
                b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3)
                self.edit(b, samplesCsv=bad)
                with self.assertRaisesRegex(ValueError, "samplesCsv"):
                    measure.compare(a, b)

    def test_zero_cpu_samples_refused(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [""] * 3, [1024] * 3)
            with self.assertRaisesRegex(ValueError, "samples"):
                measure.compare(a, b)

    def test_short_sample_span_refused(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0, 2.0, 3.0], [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0, 2.0, 3.0], [1024] * 3, span=1000)
            with self.assertRaisesRegex(ValueError, "span"):
                measure.compare(a, b)

    def test_truncated_export_nulls_unknown_delta_and_marks_biased(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3, truncated=True)
            r = measure.compare(a, b)
        self.assertIsNone(r["delta"]["unknown_rate"])
        self.assertIs(r["biased"], True)
        self.assertIsNotNone(r["delta"]["cpu_mean"])

    def test_clean_runs_not_biased(self):
        with tempfile.TemporaryDirectory() as d:
            r = measure.compare(self.make_run(d, "proc", [1.0] * 3, [1024] * 3),
                                self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3))
        self.assertIs(r["biased"], False)

    def test_bad_csv_rows_exit_1_not_traceback(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3)
            (Path(d) / "ebpf.csv").write_text("t_s,cpu_pct,rss_kib\nabc,1,2\n")
            self.assertEqual(measure.main(["compare", str(a), str(b)]), 1)
            (Path(d) / "ebpf.csv").write_bytes(b"t_s\n\x00\n")
            self.assertEqual(measure.main(["compare", str(a), str(b)]), 1)
            (Path(d) / "ebpf.csv").write_text("cpu_pct,rss_kib\n1,2\n")
            self.assertEqual(measure.main(["compare", str(a), str(b)]), 1)

    def test_comparing_a_manifest_with_itself_refused(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            with self.assertRaisesRegex(ValueError, "itself"):
                measure.compare(a, a)
            self.assertEqual(measure.main(["compare", str(a), str(Path(d) / "." / "proc.json")]), 1)

    def test_intermediate_symlink_escape_refused(self):
        with tempfile.TemporaryDirectory() as d, tempfile.TemporaryDirectory() as out:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3)
            (Path(out) / "ebpf.csv").write_text((Path(d) / "ebpf.csv").read_text())
            os.symlink(out, Path(d) / "link")
            self.edit(b, samplesCsv="link/ebpf.csv")
            with self.assertRaisesRegex(ValueError, "samplesCsv"):
                measure.compare(a, b)

    def test_lone_late_sample_refused(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0], [1024], ts=[86400])
            with self.assertRaisesRegex(ValueError, "samples"):
                measure.compare(a, b)

    def test_endpoint_only_and_gappy_coverage_refused(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3, ts=[0, 1000, 86400])
            with self.assertRaisesRegex(ValueError, "gap"):
                measure.compare(a, b)

    def test_non_finite_or_negative_timestamps_refused(self):
        for bad in ("nan", "inf", "-inf"):
            with tempfile.TemporaryDirectory() as d:
                a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
                b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3, ts=[0, bad, 86400])
                with self.assertRaisesRegex(ValueError, "t_s"):
                    measure.compare(a, b)

    def test_non_finite_or_negative_cpu_rss_refused(self):
        for cpus, rss in (([1.0, "nan", 1.0], [1024] * 3), ([1.0, "inf", 1.0], [1024] * 3),
                          ([1.0, -1.0, 1.0], [1024] * 3), ([1.0] * 3, [1024, -5, 1024]),
                          ([1.0] * 3, [1024, "nan", 1024])):
            with tempfile.TemporaryDirectory() as d:
                a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
                b = self.make_run(d, "ebpf", cpus, rss)
                with self.assertRaisesRegex(ValueError, "cpu_pct|rss_kib"):
                    measure.compare(a, b)

    def test_mismatched_export_filters_refused(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3, filters={"scope": True})
            with self.assertRaisesRegex(ValueError, "filter"):
                measure.compare(a, b)

    def test_malformed_filter_kinds_exit_1(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3)
            t = Path(d) / "ebpf-t.json"
            doc = json.loads(t.read_text())
            doc["filterKinds"] = ["application"]
            t.write_text(json.dumps(doc))
            self.assertEqual(measure.main(["compare", str(a), str(b)]), 1)

    def test_absurd_duration_refused(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3, dur=1e30)
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3, dur=1e30)
            with self.assertRaisesRegex(ValueError, "durationS"):
                measure.compare(a, b)
            self.assertEqual(measure.main(["compare", str(a), str(b)]), 1)

    def test_overflow_and_attribute_errors_exit_1(self):
        for exc in (OverflowError("x"), AttributeError("x")):
            orig = measure.compare
            measure.compare = lambda *a, e=exc: (_ for _ in ()).throw(e)
            try:
                self.assertEqual(measure.main(["compare", "a", "b"]), 1)
            finally:
                measure.compare = orig

    def test_export_window_must_match_duration(self):
        for win in (43200, "none"):
            with tempfile.TemporaryDirectory() as d:
                a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
                b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3, win_s=win)
                with self.assertRaisesRegex(ValueError, "window"):
                    measure.compare(a, b)

    def test_out_of_order_timestamps_refused(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3, ts=[0, 86400, 43200])
            with self.assertRaisesRegex(ValueError, "order"):
                measure.compare(a, b)

    def test_blank_rows_do_not_count_toward_coverage(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", ["", "", 1.0], [1024, "", 1024],
                              ts=[0, 43200, 86400])
            with self.assertRaisesRegex(ValueError, "samples"):
                measure.compare(a, b)

    def test_leading_blank_cpu_row_is_tolerated(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", ["", 1.0, 1.0, 1.0], [1024] * 4,
                              ts=[0, 28800, 57600, 86400])
            self.assertIn("delta", measure.compare(a, b))

    def test_timestamps_outside_run_refused(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3, ts=[0, 43200, 90000])
            with self.assertRaisesRegex(ValueError, "t_s"):
                measure.compare(a, b)

    def test_shared_artifact_files_refused(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3)
            self.edit(b, samplesCsv="./proc.csv")
            with self.assertRaisesRegex(ValueError, "same"):
                measure.compare(a, b)

    # --- issue #350 ---
    def test_symlinked_artifact_inside_manifest_dir_refused(self):
        # No symlink is followed at all (per-component O_NOFOLLOW), so there is
        # no window between a containment check and the open.
        for rel in ("sub/ebpf.csv", "alias.csv"):
            with tempfile.TemporaryDirectory() as d:
                a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
                b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3)
                (Path(d) / "real").mkdir()
                os.replace(Path(d) / "ebpf.csv", Path(d) / "real" / "ebpf.csv")
                os.symlink("real", Path(d) / "sub")
                os.symlink("real/ebpf.csv", Path(d) / "alias.csv")
                self.edit(b, samplesCsv=rel)
                with self.assertRaisesRegex(ValueError, "samplesCsv"):
                    measure.compare(a, b)

    def test_swapped_component_after_resolve_is_not_followed(self):
        # Even if path resolution is fooled (dir swapped for a symlink after any
        # check), the open itself refuses the symlink.
        with tempfile.TemporaryDirectory() as d, tempfile.TemporaryDirectory() as out:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3)
            (Path(out) / "ebpf.csv").write_text((Path(d) / "ebpf.csv").read_text())
            os.symlink(out, Path(d) / "link")
            self.edit(b, samplesCsv="link/ebpf.csv")
            orig = Path.resolve
            Path.resolve = lambda self, strict=False: Path(os.path.abspath(self))
            try:
                with self.assertRaisesRegex(ValueError, "samplesCsv"):
                    measure.compare(a, b)
            finally:
                Path.resolve = orig

    def test_process_name_required_and_consistent(self):
        for name in ("", ["d", "d", "e"]):
            with tempfile.TemporaryDirectory() as d:
                a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
                b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3, name=name)
                with self.assertRaisesRegex(ValueError, "name"):
                    measure.compare(a, b)

    def test_process_name_must_match_across_runs(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3, name="opensnitchd")
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3, name="other")
            with self.assertRaisesRegex(ValueError, "name"):
                measure.compare(a, b)

    def test_simultaneous_pids_summed_per_timestamp(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0, 2.0] * 3, [1024, 2048] * 3,
                              ts=[0, 0, 43200, 43200, 86400, 86400], pids=[1, 2] * 3)
            b = self.make_run(d, "ebpf", [3.0] * 3, [3072] * 3)
            r = measure.compare(a, b)
        self.assertAlmostEqual(r["proc"]["cpu_mean"], 3.0)
        self.assertAlmostEqual(r["proc"]["rss_start_mib"], 3.0)
        self.assertAlmostEqual(r["proc"]["rss_end_mib"], 3.0)
        self.assertAlmostEqual(r["delta"]["cpu_mean"], 0.0)

    def test_timestamp_with_incomplete_process_set_not_counted(self):
        # pid 2 has no cpu yet at t=0: neither CPU nor RSS is taken from t=0,
        # so RSS start comes from the same process set as the CPU values.
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0, "", 1.0, 1.0, 1.0, 1.0, 1.0, 1.0],
                              [1024, 99999, 1024, 1024, 1024, 1024, 1024, 1024],
                              ts=[0, 0, 28800, 28800, 57600, 57600, 86400, 86400],
                              pids=[1, 2] * 4)
            b = self.make_run(d, "ebpf", [2.0] * 3, [2048] * 3)
            r = measure.compare(a, b)
        self.assertAlmostEqual(r["proc"]["cpu_mean"], 2.0)
        self.assertAlmostEqual(r["proc"]["rss_start_mib"], 2.0)

    def test_duplicate_timestamps_count_once_toward_min_samples(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0] * 4, [1024] * 4,
                              ts=[0, 0, 43200, 43200], pids=[1, 2, 1, 2])
            with self.assertRaisesRegex(ValueError, "samples"):
                measure.compare(a, b)

    def test_export_window_must_agree_with_csv_wall_clock(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3, iso_shift=3600)
            with self.assertRaisesRegex(ValueError, "wall-clock"):
                measure.compare(a, b)

    def test_missing_or_bad_iso_utc_refused(self):
        for iso in (["", "", ""], ["x", "x", "x"]):
            with tempfile.TemporaryDirectory() as d:
                a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
                b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3, iso=iso)
                with self.assertRaisesRegex(ValueError, "iso_utc"):
                    measure.compare(a, b)

    def test_hard_linked_shared_artifact_refused(self):
        with tempfile.TemporaryDirectory() as d:
            a = self.make_run(d, "proc", [1.0] * 3, [1024] * 3)
            b = self.make_run(d, "ebpf", [1.0] * 3, [1024] * 3)
            os.unlink(Path(d) / "ebpf-t.json")
            os.link(Path(d) / "proc-t.json", Path(d) / "ebpf-t.json")
            with self.assertRaisesRegex(ValueError, "same"):
                measure.compare(a, b)


if __name__ == "__main__":
    unittest.main()

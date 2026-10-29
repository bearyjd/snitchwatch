#!/usr/bin/env python3
"""Offline tests for measure.py against synthetic /proc trees and exports."""
import csv
import json
import os
import stat
import sys
import tempfile
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


if __name__ == "__main__":
    unittest.main()

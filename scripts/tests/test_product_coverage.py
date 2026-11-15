# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("product_coverage", Path(__file__).parents[1] / "check-coverage.py")
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


class ProductCoverageTests(unittest.TestCase):
    def report(self, lines=80, regions=80):
        return {"data": [{"files": [{"filename": "/repo/crates/core/src/lib.rs", "summary": {"lines": {"covered": lines, "count": 100}, "regions": {"covered": regions, "count": 100}}}], "totals": {
            "lines": {"covered": lines, "count": 100}, "regions": {"covered": regions, "count": 100}}}]}

    def test_threshold_accepts_exactly_eighty_percent(self):
        self.assertEqual(policy.validate(self.report(), {"core"})[0], [])

    def test_regions_below_threshold_fail_even_with_high_line_coverage(self):
        errors, _ = policy.validate(self.report(lines=99, regions=79), {"core"})
        self.assertTrue(any("regions coverage" in error for error in errors))

    def test_lines_below_threshold_fail_even_with_high_region_coverage(self):
        errors, _ = policy.validate(self.report(lines=79, regions=99), {"core"})
        self.assertTrue(any("lines coverage" in error for error in errors))

    def test_omitting_a_product_crate_fails(self):
        errors, _ = policy.validate(self.report(), {"core", "mobile"})
        self.assertTrue(any("missing production crates: mobile" in error for error in errors))

    def test_test_assertions_cannot_be_used_as_product_source(self):
        report = self.report()
        report["data"][0]["files"].append({"filename": "/repo/crates/core/tests/integration.rs", "summary": {"lines": {"count": 30}}})
        self.assertTrue(policy.validate(report, {"core"})[0])

    def test_empty_or_impossible_totals_fail(self):
        for covered, count in [(0, 0), (101, 100), (-1, 100), (True, 100)]:
            report = self.report()
            report["data"][0]["totals"]["lines"] = {"covered": covered, "count": count}
            self.assertTrue(policy.validate(report, {"core"})[0])

    def test_missing_reports_fail(self):
        for report in [{}, {"data": []}, {"data": [{"files": []}]}]:
            self.assertTrue(policy.validate(report, {"core"})[0])

    def test_unrecognized_sources_cannot_inflate_overall_totals(self):
        report = self.report(lines=0, regions=0)
        report["data"][0]["files"][0]["summary"] = {key: {"covered": 0, "count": 10} for key in ["lines", "regions"]}
        report["data"][0]["files"].append({"filename": "/repo/tests/smoke.rs", "summary": {key: {"covered": 90, "count": 90} for key in ["lines", "regions"]}})
        report["data"][0]["totals"] = {key: {"covered": 90, "count": 100} for key in ["lines", "regions"]}
        self.assertTrue(policy.validate(report, {"core"})[0])

    def test_zero_count_filename_cannot_prove_a_crate_was_measured(self):
        report = self.report()
        report["data"][0]["files"].append({"filename": "/repo/crates/mobile/src/lib.rs", "summary": {key: {"covered": 0, "count": 0} for key in ["lines", "regions"]}})
        self.assertTrue(any("missing production crates: mobile" in error for error in policy.validate(report, {"core", "mobile"})[0]))

    def test_duplicate_source_and_inconsistent_totals_fail(self):
        report = self.report()
        report["data"][0]["files"].append(report["data"][0]["files"][0])
        self.assertTrue(policy.validate(report, {"core"})[0])
        report = self.report()
        report["data"][0]["totals"]["lines"]["covered"] = 99
        self.assertTrue(policy.validate(report, {"core"})[0])

    def gold_report(self, branches=80):
        report = self.report(lines=90, regions=90)
        for summary in [report["data"][0]["files"][0]["summary"], report["data"][0]["totals"]]:
            summary["branches"] = {"covered": branches, "count": 100}
        return report

    def test_gold_requires_ninety_percent_lines_and_regions_and_eighty_branches(self):
        errors, metrics = policy.validate(self.gold_report(), {"core"}, 90, 80)
        self.assertEqual(errors, [])
        self.assertEqual(metrics["branches"]["percent"], 80)
        for metric in ["lines", "regions", "branches"]:
            report = self.gold_report()
            for summary in [report["data"][0]["files"][0]["summary"], report["data"][0]["totals"]]:
                summary[metric]["covered"] -= 1
            self.assertTrue(any(f"{metric} coverage" in error
                                for error in policy.validate(report, {"core"}, 90, 80)[0]))

    def test_gold_rejects_absent_empty_or_inconsistent_branch_instrumentation(self):
        for replacement in [None, {"covered": 0, "count": 0}, {"covered": 101, "count": 100}]:
            report = self.gold_report()
            for summary in [report["data"][0]["files"][0]["summary"], report["data"][0]["totals"]]:
                if replacement is None:
                    del summary["branches"]
                else:
                    summary["branches"] = replacement
            self.assertTrue(policy.validate(report, {"core"}, 90, 80)[0])
        report = self.gold_report()
        report["data"][0]["totals"]["branches"]["covered"] += 1
        self.assertTrue(any("branches totals disagree" in error
                            for error in policy.validate(report, {"core"}, 90, 80)[0]))

    def test_nonproduct_branch_counters_cannot_inflate_gold(self):
        report = self.gold_report(branches=79)
        report["data"][0]["files"].append({"filename": "/repo/tests/smoke.rs", "summary": {
            "lines": {"count": 0, "covered": 0}, "regions": {"count": 0, "covered": 0},
            "branches": {"count": 100, "covered": 100}}})
        report["data"][0]["totals"]["branches"] = {"count": 200, "covered": 179}
        self.assertTrue(any("test/non-product" in error
                            for error in policy.validate(report, {"core"}, 90, 80)[0]))

    def test_invalid_thresholds_cannot_disable_the_gate(self):
        for minimum, branches in [(101, 80), (-1, 80), (90, -1), (90, 101), (True, 80)]:
            self.assertTrue(policy.validate(self.gold_report(), {"core"}, minimum, branches)[0])

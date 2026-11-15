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

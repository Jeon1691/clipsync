# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Require measured source coverage across all Rust products.

LLVM executable regions provide the statement-coverage evidence used by this
project. Requiring both source lines and regions avoids accepting a high line
percentage caused by several unexecuted expressions on the same source line.
Only test code is excluded by the measurement; product crates remain included.
"""
import argparse
import json
from pathlib import Path
import posixpath
import sys


MINIMUM = 80


def counter(metric):
    if not isinstance(metric, dict):
        return None
    covered, count = metric.get("covered"), metric.get("count")
    if (isinstance(covered, bool) or isinstance(count, bool)
            or not isinstance(covered, int) or not isinstance(count, int)
            or not 0 <= covered <= count):
        return None
    return covered, count


def validate(report, packages, minimum=MINIMUM, minimum_branches=None):
    errors = []
    thresholds = {"lines": minimum, "regions": minimum}
    if minimum_branches is not None:
        thresholds["branches"] = minimum_branches
    if any(isinstance(value, bool) or not isinstance(value, int) or not 0 <= value <= 100
           for value in thresholds.values()):
        return ["coverage thresholds must be integer percentages from 0 to 100"], {}
    data = report.get("data", []) if isinstance(report, dict) else []
    if not isinstance(data, list) or len(data) != 1 or not isinstance(data[0], dict):
        return ["expected one LLVM workspace report"], {}
    unit = data[0]
    files = unit.get("files", [])
    if not isinstance(files, list) or not files:
        return ["report contains no product files"], {}
    present, seen, product_sources = set(), set(), set()
    sums = {name: [0, 0] for name in thresholds}
    for source in files:
        if not isinstance(source, dict) or not isinstance(source.get("filename"), str):
            errors.append("invalid source record")
            continue
        name = posixpath.normpath(source["filename"].replace("\\", "/"))
        if name in seen:
            errors.append(f"duplicate source: {name}")
            continue
        seen.add(name)
        summary = source.get("summary", {})
        counts = {key: counter(summary.get(key)) for key in sums} if isinstance(summary, dict) else {}
        if any(counts.get(key) is None for key in sums):
            errors.append(f"invalid source counters: {name}")
            continue
        package, _, relative = name.rpartition("/crates/")[2].partition("/")
        product = package in packages and relative.startswith("src/")
        if not product:
            if any(value[1] for value in counts.values()):
                errors.append(f"test/non-product source included in product metrics: {name}")
            continue
        logical_source = f"{package}/{relative}"
        if logical_source in product_sources:
            errors.append(f"duplicate production source across build roots: {logical_source}")
            continue
        product_sources.add(logical_source)
        if all(counts[key][1] > 0 for key in ["lines", "regions"]):
            present.add(package)
        for key, (covered, count) in counts.items():
            sums[key][0] += covered
            sums[key][1] += count
    missing = packages - present
    if missing:
        errors.append("missing production crates: " + ", ".join(sorted(missing)))
    totals = unit.get("totals", {})
    metrics = {}
    for name, (covered, count) in sums.items():
        declared = counter(totals.get(name)) if isinstance(totals, dict) else None
        if declared != (covered, count):
            errors.append(f"{name} totals disagree with validated production files")
        if count <= 0:
            errors.append(f"empty product {name} totals")
            continue
        metrics[name] = {"covered": covered, "count": count, "percent": 100 * covered / count}
        if covered * 100 < thresholds[name] * count:
            errors.append(f"{name} coverage {metrics[name]['percent']:.2f}% is below {thresholds[name]}%")
    return errors, metrics


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("summary", type=Path)
    parser.add_argument("--minimum-percent", type=int, default=MINIMUM,
                        help="Required product line and executable-region percentage (default: 80)")
    parser.add_argument("--minimum-branch-percent", type=int,
                        help="Require branch instrumentation and this product branch percentage")
    args = parser.parse_args(argv)
    root = Path(__file__).resolve().parents[1]
    packages = {path.parent.name for path in (root / "crates").glob("*/Cargo.toml")}
    try:
        errors, metrics = validate(json.loads(args.summary.read_text()), packages,
                                   args.minimum_percent, args.minimum_branch_percent)
    except (OSError, ValueError, TypeError, AttributeError) as error:
        errors, metrics = [str(error)], {}
    for name, metric in metrics.items():
        minimum = args.minimum_branch_percent if name == "branches" else args.minimum_percent
        print(f"Product {name}: {metric['covered']}/{metric['count']} ({metric['percent']:.2f}%; required {minimum}%)")
    for error in errors:
        print(f"error: {error}", file=sys.stderr)
    return int(bool(errors))


if __name__ == "__main__":
    sys.exit(main())

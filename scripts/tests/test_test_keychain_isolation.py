# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Product builds never select the test-only keychain isolation.

`clipsync-storage`'s `test-isolated-keychain` feature stops test builds from
mirroring secrets into the OS keychain. It must reach a build only through a
`[dev-dependencies]` entry, which Cargo's version 2 feature resolver applies
to test targets and never to a `cargo build`/`cargo install` of a product.
"""

import json
from pathlib import Path
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]
STORAGE = "clipsync-storage"
FEATURE = "test-isolated-keychain"


def cargo(*args):
    return subprocess.run(
        ["cargo", *args, "--locked"], cwd=ROOT, check=True, capture_output=True, text=True,
    ).stdout


class TestKeychainIsolationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        metadata = json.loads(cargo("metadata", "--format-version", "1", "--no-deps"))
        cls.packages = metadata["packages"]

    def storage_dependencies(self):
        for package in self.packages:
            for dependency in package["dependencies"]:
                if dependency["name"] == STORAGE:
                    yield package, dependency

    def test_the_feature_exists_and_is_not_a_default(self):
        storage = next(package for package in self.packages if package["name"] == STORAGE)
        self.assertIn(FEATURE, storage["features"])
        self.assertNotIn(FEATURE, storage["features"].get("default", []))

    def test_only_dev_dependencies_enable_the_feature(self):
        enabling = [(package["name"], dependency["kind"])
                    for package, dependency in self.storage_dependencies()
                    if FEATURE in dependency["features"]]
        self.assertTrue(enabling, "no test build selects the isolated keychain")
        for name, kind in enabling:
            with self.subTest(package=name):
                self.assertEqual(kind, "dev", f"{name} enables {FEATURE} outside [dev-dependencies]")
        for package in self.packages:
            for feature, enables in package["features"].items():
                with self.subTest(package=package["name"], feature=feature):
                    self.assertFalse(
                        any(FEATURE in item for item in enables),
                        f"{package['name']}/{feature} forwards {FEATURE}",
                    )

    def test_product_dependency_graphs_leave_the_feature_off(self):
        products = sorted({package["name"] for package, dependency in self.storage_dependencies()
                           if dependency["kind"] is None})
        self.assertTrue(products, "no product depends on clipsync-storage")
        for product in products:
            with self.subTest(product=product):
                # Normal and build edges are what `cargo build -p <product>` resolves.
                tree = cargo("tree", "-p", product, "--target", "all", "-e", "normal,build,features",
                             "-i", STORAGE, "--prefix", "none")
                self.assertIn(STORAGE, tree)
                self.assertNotIn(f'feature "{FEATURE}"', tree)


if __name__ == "__main__":
    unittest.main()

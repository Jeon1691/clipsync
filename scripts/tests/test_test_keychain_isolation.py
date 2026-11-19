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


def cargo(*args, check=True):
    completed = subprocess.run(
        ["cargo", *args, "--locked"], cwd=ROOT, check=check, capture_output=True, text=True,
    )
    return completed.stdout if check else completed


def storage_tree(package, edges):
    """`cargo tree` of the paths from `package` to clipsync-storage over `edges`."""
    return cargo("tree", "-p", package, "--target", "all", "-e", edges,
                 "-i", STORAGE, "--prefix", "none", check=False)


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
        # Positive control: the same query with dev edges shows the feature, so
        # its absence below is not an artifact of the output format.
        with_dev = storage_tree(products[0], "normal,build,dev,features")
        self.assertIn(f'feature "{FEATURE}"', with_dev.stdout)
        for product in products:
            with self.subTest(product=product):
                # Normal and build edges are what `cargo build -p <product>` resolves.
                tree = storage_tree(product, "normal,build,features")
                self.assertEqual(tree.returncode, 0, tree.stderr)
                self.assertIn(STORAGE, tree.stdout)
                self.assertNotIn(f'feature "{FEATURE}"', tree.stdout)

    def test_every_crate_whose_tests_reach_storage_enables_the_feature(self):
        # Workspace runs merge features across crates, but `cargo test -p <crate>`
        # alone sees only that crate's own dev-dependencies.
        enabling = {package["name"] for package, dependency in self.storage_dependencies()
                    if dependency["kind"] == "dev" and FEATURE in dependency["features"]}
        reaching = [package["name"] for package in self.packages
                    if storage_tree(package["name"], "normal,build,dev").returncode == 0]
        self.assertIn(STORAGE, reaching)
        for name in reaching:
            with self.subTest(package=name):
                self.assertIn(name, enabling, f"{name}'s tests reach {STORAGE} without {FEATURE}")


if __name__ == "__main__":
    unittest.main()

# ClipSync-maintained dependency compatibility patches

These eight directories are **local compatibility and reproducibility forks maintained by ClipSync**.
They are not upstream releases or upstream-approved fixes. Cargo selects them
explicitly through `[patch.crates-io]`; package names and release versions remain
those of their source distributions. The upstreams remain the authorities for
new releases; ClipSync owns the changes and security fixes here until each patch
can be removed. No RustSec advisory is ignored.

`sources.json` pins every original crates.io archive by SHA-256, upstream Git
revision, repository path, and license. `patches/*.patch` records the complete
local difference from that archive. Unused upstream `Cargo.lock` files are omitted;
`Cargo.toml.orig` and `.cargo_vcs_info.json` are preserved as original provenance,
not descriptions of the local changes. Upstream copyright notices are preserved.
The first-party notice checker excludes `vendor/` for this reason. Local
modifications use each source's existing license expression.

| Source | Local maintenance scope | Retirement condition |
| --- | --- | --- |
| askama_derive 0.16.1 | Backport upstream [3863808](https://github.com/askama-rs/askama/commit/3863808cf594054f7644979ed92caaafc878f378): use maintained `toml` with its parse/serde features instead of `basic-toml`. | Compatible Askama release containing that commit. |
| uniffi_internal_macros 0.32.2 | Backport upstream [ef9625b](https://github.com/mozilla/uniffi-rs/commit/ef9625b68dd51207f55f024a27649e58edb52b09): emit `Node` descendant checks in declaration order using the existing `IndexSet` dependency. Preserve all struct/enum fields and recursive traversal; add stable expansion and semantic traversal tests. The original source and local changes use MPL-2.0. | Compatible UniFFI release containing that commit; 0.32.2 remains the latest release checked on 2026-11-15. |
| accesskit_unix 0.24.1 | Use zbus 6 types/builders and its default runtime on the existing dedicated accessibility thread; retain Tokio support and all AT-SPI interfaces. | Compatible AccessKit Unix release using zbus 6. |
| accesskit_atspi_common 0.21.1 | Use the merged zbus 6 wire types. | Compatible upstream translation layer using zbus 6. |
| atspi-common 0.13.0 | Use zbus 6 wire/name types, deferred builder errors, renamed message decoding API, and current XML validators. Preserve AT-SPI signatures and interface names. Port upstream tests; add malformed wire-name/path rejection checks. | Compatible AT-SPI release using zbus 6. |
| atspi-proxies 0.13.0 | Use zbus 6 proxy builders and wire types. Preserve all proxy interfaces. | Compatible AT-SPI proxies release using zbus 6. |
| zbus-lockstep 0.7.0 | Use zbus 6 and zbus_xml 6 signatures, XML iterators, and errors. Keep XML signature checks and add negative XML/interface/member/argument checks. | Compatible upstream validator release using zbus 6. |
| zbus-lockstep-macros 0.7.0 | Emit zbus 6 type paths and use zbus_xml 6. | Compatible upstream macro release using zbus 6. |

AccessKit's archives omit their workspace license files; `LICENSE-APACHE` and
`LICENSE-MIT` are copied from their pinned upstream revision
`f3026222559c7f5f46d8f25cb7404de68eb149f0`. The validator archive omits its test XML;
`zbus-lockstep/xml/test_definition_file.xml` is copied unchanged from upstream
revision `ac20711dd0ce42eb185f19c42d360c12b51aff1e`, path
`xml/test_definition_file.xml`. These additions are included in the recorded diffs.

ClipSync's maintainer must review these patches against upstream changes weekly
and before every release, apply relevant fixes, and re-run the checks below.
The first retirement review is due **2026-12-11**. Dependabot's registry updates
alone do not maintain these local sources. Do not update a catalog checksum or
diff without reviewing the corresponding source change. A new upstream version
must pass the same behavior tests before removing a patch.

```sh
# Uses original archives from Cargo's cache; omit --offline to download missing
# archives from the pinned static.crates.io URLs. No source file is modified.
python3 scripts/check-dependency-patches.py --offline
# Tests disposable copies: parser config, wire round trips, malformed inputs,
# XML signatures, compile-time validators, and deterministic Node traversal.
python3 scripts/test-dependency-patches.py --toolchain 1.100.0
```

See [the migration evidence](../docs/dependency-migration.md) for platform,
compiler, desktop, clipboard, mobile, and audit validation.

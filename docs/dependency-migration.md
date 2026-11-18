# Dependency migration for v0.2.0

The migration removes all six previously reported maintenance-advisory packages
from the resolved workspace dependency graph. It requires **Rust 1.95** because
eframe 0.37 requires that compiler. This changes the supported compiler contract;
it belongs in v0.2.0, not a patch release advertised as Rust 1.88 compatible.

| Removed package/advisory | Replacement path |
| --- | --- |
| basic-toml / RUSTSEC-2026-0374 | UniFFI 0.32.2 → Askama 0.16.1 → explicit local backport of Askama's upstream parser fix → maintained `toml`. |
| paste / RUSTSEC-2024-0436 | eframe 0.37 → wgpu 30 → maintained objc2 Metal bindings. The old `metal`/`paste` chain is absent. |
| ttf-parser / RUSTSEC-2026-0192 | eframe/epaint 0.37 use Skrifa. Wayland winit 0.30.13 uses its existing Crossfont title renderer, backed by Fontconfig/FreeType. |
| zbus_names / RUSTSEC-2026-0371 | AccessKit/AT-SPI compatibility patches → maintained zbus 6.3 `names` module. |
| zvariant / RUSTSEC-2026-0370 | AccessKit/AT-SPI compatibility patches → maintained zbus 6.3 wire implementation. |
| zvariant_derive / RUSTSEC-2026-0372 | zbus 6.3 macros replace the old standalone wire derives. |

The eight explicit source patches are **ClipSync-maintained compatibility and reproducibility forks**,
not a claim that the affected upstreams have published fixes. Their exact scope,
original archive hashes, pinned Git revisions, full local diffs, licenses,
maintenance owner, review cadence, and retirement conditions are in
[vendor/README.md](../vendor/README.md) and [sources.json](../vendor/sources.json).
They migrate real dependency APIs; they do not rename the affected packages or
suppress their advisories. Cargo's audit has limited visibility into local source
changes, so the source comparison and behavior tests are required additional
controls, not replaced by an empty audit report.

UniFFI 0.32.2 also emits `Node` descendant checks by iterating a randomly seeded
`HashSet`. Repeated compiler expansions at the same source path produce different
code; the Linux reconstruction check reproduced differing `uniffi-bindgen` and
mobile static-library bytes while the other outputs matched. The explicit
`uniffi_internal_macros` backport of upstream
[ef9625b](https://github.com/mozilla/uniffi-rs/commit/ef9625b68dd51207f55f024a27649e58edb52b09)
keeps the first occurrence of each field type in declaration order. This preserves
the emitted checks and traversal behavior. Its tests cover repeated expansion,
all struct and enum fields, duplicate field values, and recursive descendants.
The backport must be retired when a compatible released UniFFI includes the fix.

## Compatibility decisions

Both OpenGL (`glow`) and wgpu remain enabled. The desktop explicitly selects Glow
to preserve its previous renderer after eframe changed its default. The native
window still has the `ClipSync` title, decorations, 440×560 initial size, and
380×480 minimum size. The UI uses eframe's new `App::ui`/egui root-UI API.

The previous eframe defaults included bundled GUI fonts, X11, Wayland, native
accessibility, and clipboard support; those remain enabled. Winit's default
`ab_glyph` title renderer is replaced with its supported
`wayland-csd-adwaita-crossfont` feature, retaining rendered title text and native
font preferences. Linux builds need Fontconfig and FreeType development packages;
Linux installations need those libraries and an installed font. Eframe's new
optional system-font-fallback feature was not part of the previous release and
is not enabled: its Fontconfig version conflicts with winit's Crossfont backend.
Bundled GUI fonts remain available.

The Linux adapter retains activation/deactivation, all AT-SPI interfaces,
properties, actions, events, and the dedicated accessibility thread. Zbus 6's
runtime replaces manual ticking of zbus 5's internal executor. Its builder errors
are handled at `build()`, its unified error is propagated, and object registration
uses its new `Option` result without changing duplicate-interface behavior. The
AT-SPI XML signature validators remain active. Tests verify malformed names,
paths, types, XML signatures, and missing XML members are rejected.

UniFFI exports, callbacks, records, and errors remain in place. Mobile consumers
must regenerate Swift/Kotlin bindings with this checkout's UniFFI 0.32.2 generator
and rebuild their native library together. Old generated UniFFI 0.29 bindings
must not be mixed with the new native library. The native app repositories are
separate; successful binding generation does not establish an iOS/Android app
runtime result.

## Validation

Local validation on 2026-11-15 used Rust 1.100.0, with an actual Rust 1.95.0 check:

- macOS: workspace/all-target check, Clippy with warnings denied, and the actual
  Rust 1.95.0 workspace/all-target check pass.
- Linux aarch64 in a private Debian Bookworm container: desktop/all-target check,
  desktop build, and workspace/all-target Clippy with warnings denied pass.
- Upstream source tests pass: 45 Askama tests, 1,142 AT-SPI wire/schema tests,
  26 XML validator tests, and five UniFFI macro/traversal checks. Negative tests include invalid D-Bus names/paths,
  invalid message bodies, malformed XML types, and missing interfaces/members/
  arguments. No validator was removed.
- A private Xvfb and D-Bus session runs the real Linux desktop, confirms its
  native title with X11, and reads its labels, pairing buttons, and editable
  field through the system AT-SPI registry. Run
  `python3 scripts/test-linux-desktop.py target/debug/clipsync-desktop` on Linux
  with `dbus-run-session`, `xvfb-run`, `xwininfo`, `python3-pyatspi`, and fonts
  installed. The script seeds a synthetic identity under a temporary home and
  starts no login daemon.
- A separate private headless Weston/GL compositor renders the Wayland window
  with its Crossfont title and decorations. The captured screenshot was visually
  inspected: the title and GUI controls render correctly. Repeat with
  `python3 scripts/test-linux-wayland.py target/debug/clipsync-desktop target/wayland-smoke`
  on Linux with Weston, a software EGL renderer, and fonts installed.
- Workspace tests pass with `-- --test-threads=1`, including clipboard image/file
  format matrices, desktop headless rendering, and UniFFI conversions/callbacks.
  The default parallel run exposed daemon socket-readiness and relay-pairing
  timing failures; those results are not described as a parallel pass. The
  independent verification workstream owns those fixes.
- Swift and Kotlin binding generation succeeds from the built native library.
  Generated Swift typechecks with its generated C module map. Android app
  compilation/runtime was not performed here.
- `cargo audit --json` and `cargo audit --file fuzz/Cargo.lock --json` both report
  `vulnerabilities.count = 0` and `warnings = {}`. Neither audit used ignores at
  the time; the later, since retired, chrono exception is recorded in
  [security alerts](security-alerts.md#unmaintained-time-crates).
  The fuzz workspace does not depend on the desktop/mobile stack and its lockfile
  remains unchanged.

Hosted Windows/macOS/Linux checks and the final release/reproducible-build
verification remain release gates. The local checks above do not substitute for
those results.

## Automated compatibility checks

[The dependency patch workflow](../.github/workflows/dependency-patches.yml)
runs on every pull request, default-branch push, weekly schedule and manual
request. `Dependency source and upstream tests` verifies every original source
archive against the catalog checksum, applies its full recorded patch, and
compares every resulting source file with the vendored copy. It then executes
the existing upstream parser, AT-SPI wire/schema, XML and macro tests from
disposable copies. These checks fail the job on errors; no validator or
accessibility interface is disabled to obtain a pass.

The independent `vendor/test-workspace.Cargo.lock` freezes the disposable
workspace's upstream development dependencies. The test script copies that
committed lock on every run and invokes Cargo with `--locked`; the product
`Cargo.lock` is unchanged. To update the test graph intentionally, regenerate
that separate lock using the disposable workspace manifest, review its changes,
copy it back to `vendor/test-workspace.Cargo.lock`, and rerun both source and
behavior checks. Changing product dependencies alone does not silently refresh
this test resolution.

`Linux native dependency smoke` builds the actual desktop in the digest-pinned
Bookworm builder, retaining Fontconfig/FreeType, bundled fonts, X11, Wayland and
AT-SPI support. It runs the existing private Xvfb/D-Bus accessibility check and
headless Weston/GL screenshot check. System Python supplies the distro's
GI/pyatspi bindings. The Bookworm environment keeps the exercised Weston 10
command-line/screenshot interfaces; Ubuntu 24.04's [Weston 13](https://packages.ubuntu.com/noble/weston)
has different renderer options and is not substituted without validation.
Environment/package versions, logs and screenshots are retained as workflow
artifacts. The automated Wayland check establishes window survival, native title
and screenshot capture; a human must inspect the screenshot before claiming
visual correctness. This Linux smoke is not a complete accessibility audit or
Windows/macOS/mobile runtime evidence.

Configured checks are not passing evidence until their workflow run succeeds.
These controls also do not replace the maintainer's weekly upstream/security
review and patch-retirement decisions described in the vendor register.

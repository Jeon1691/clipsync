# Compiler and Clippy warning policy

The [three-platform CI workflow](../.github/workflows/ci.yml) rejects compiler
and Clippy warnings with:

```sh
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Contributions must fix new diagnostics or justify a narrow exception at its
source. The project does not disable warning groups or raise lint thresholds
to obtain a clean result. The separate minimum-Rust job continues to check the
workspace with Rust 1.95.

On 2026-11-15, the command above passed locally on macOS with Rust/Clippy
1.100.0 across the whole workspace and all targets. It produced no compiler,
Clippy, or Cargo manifest warnings. The Linux and Windows CI runs remain the
authority for those platform-specific configurations; a successful macOS run
alone does not establish their results.

## Deliberate exceptions

The remaining exceptions use `#[expect(..., reason = "...")]` on the affected
functions and two transport enums. Legacy macOS filename pasteboard reads also document
their compatibility reason at the deprecated API use. Unlike an
unconditional allowance, an expectation warns when the reviewed lint no longer
occurs, so changes to these choices require another review.

- `large_enum_variant`: `Incoming` and `Outgoing` carry control messages inline
  in queues bounded to 256 entries. Preserve the public incoming representation
  and avoid a new allocation for each control message. Their existing bounded
  memory use is intentional.
- `too_many_arguments`: pairing and initial reconnect handling borrow the
  existing session state explicitly. Their local expectations retain that
  state flow without introducing an unrelated context abstraction.

The code's supported behavior is unchanged by these choices. In particular,
the internal Rust IPC `Shutdown` request acknowledges without terminating the
engine. The CLI's `daemon stop` uses a separate service-management path.
Callers must not treat the internal request as a shutdown guarantee; supporting
or removing that operation remains a focused API decision.

## Historical inventory and cleanup

The previous inventory was reviewed on 2026-11-15 using Rust 1.100 CI for commit
`0cfc7d0edd7b317a50c9dc9fad1a79a77ac6cd09`:
[Linux, macOS, and Windows logs](https://github.com/Jeon1691/clipsync/actions/runs/43670736266).
ANSI color codes were removed and repeated diagnostics were deduplicated by
source file/line and lint. These were real warnings, not blanket false positives.

| Scope | Distinct warning locations |
| --- | ---: |
| Linux | 93 |
| macOS | 76 |
| Windows | 104 |
| Union of the three platforms | 113 |

The historical snapshot contains 10,686 Rust source lines. Platform counts
correspond to 0.71–0.97 warnings per 100 lines; the union corresponds to 1.06.
These figures provide context, not an automatic compliance threshold. The
[Best Practices criterion](https://www.bestpractices.dev/en/criteria/0?details=true&rationale=true#warnings_fixed)
requires addressing warnings and permits accepting some after review.

| Historical group | Union | Current disposition |
| --- | ---: | --- |
| `result_large_err` | 50 | Resolved upstream: tungstenite 0.28 boxed its large error variants, shrinking `tungstenite::Error` from 136 to 32 bytes. With the tokio-tungstenite 0.29 upgrade, `TransportError` and `CoreError` fell below the lint threshold, Clippy reported every local expectation as unfulfilled, and they were removed without changing either public error type. |
| `large_enum_variant` | 2 | Reviewed bounded queue layout retained through enum-specific expectations. |
| `too_many_arguments` | 1 | Explicit session-state handling retained through a function-specific expectation. |
| Other Clippy style diagnostics | 19 | Removed redundant returns, borrows, formatting, and a manual default implementation; used the supported I/O error constructor. |
| Rust unused/dead/unreachable diagnostics | 27 | Compiled command adapters, native sidecars, image helpers, imports, and service helpers only on the platforms that actually use them. Removed unused internal helper methods and stubs. Removed the unreachable generic fallback from Linux/Windows configurations while preserving each platform's existing selection. |
| Cargo unused declarations | 14 | Inherited authors/repository metadata in members, removed redundant defaults and unused workspace aliases, and removed proven unused direct dependencies. Target-specific dependencies now follow their source modules. UniFFI macro scaffolding and its binding-generator CLI remain enabled; the removed build dependency had no build script to consume it. |

The cleanup also removed additional unused dependencies exposed during the
strict run. The lockfile was regenerated from the existing versions; the
cleanup does not intentionally upgrade dependencies or remove accessibility,
native clipboard support, image formats, desktop integration, or mobile
bindings. Revisit the inventory and local exceptions when the compiler,
target features, or public error types change.

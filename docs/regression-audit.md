# Six-month regression-test audit — 2026-11-16

The complete ledger now registers **61 of 107 distinct corrected defects (57.01%)**,
meeting the Silver requirement of at least 50%. [The machine-readable ledger](regressions.json)
and [CI checker](../scripts/check-regressions.py) preserve the full denominator
and derive credit from concrete test references, not a manually entered Boolean.
Tests must pass in their owning CI jobs; discovery alone is not execution or proof
that an assertion reproduces a historical defect.

## Scope and counting rules

- Follow-up window: 2026-05-16 through 2026-11-16. The original frozen baseline
  used 2026-05-15 through 2026-11-15; no rows leave the denominator because the
  project began on 2026-09-16. The
  baseline reviewed all 61 non-merge commits through
  `639000d60f1af1cc856fe20c61b60c84f9b2ab7b` and PR #16's five commits through
  `3faac58836d94457c16e890f4d1380151236e5ea`. The source-history additions below
  also register the public PR #21 publication corrections and the integrated
  reconstruction, relay revocation/FIFO, and mobile fixture corrections through
  `2e42f06061a636a5f58b3aaf96a40d48fd75e00c`. Documentation confirmations,
  coverage expansions, compatibility features and release bumps add no defects.
  IDs follow ledger append order, not commit chronology; a cherry-pick does not
  create an additional defect.
- A row is one materially distinct corrected failure/security invariant.
  Multiple input strings, aliases, package paths, tests and merge commits do not
  increase the bug count. The three emSign advisories sharing CVE-2026-111943
  are counted as one defect.
- The broad denominator includes runtime/security, build, packaging, CI and
  upstream vulnerability corrections. Pure features, documentation refreshes,
  equivalent style cleanup and release bumps are consistently excluded.
- Credit requires a focused automated check. A passing smoke test, test count,
  audit command or nearby fixture does not automatically cover every related bug.
  Platform-specific checks count for their owning platform. Release-only checks
  are explicitly wired before publication and are not claimed to run in every PR job.
- F01–F84 remain in the CI ledger; append new independently corrected defects.
  Do not delete untested rows to increase the fraction. This frozen audit baseline
  is a project quality gate; review the time window explicitly in a later audit
  rather than silently changing the badge's six-month scope.

## Results and verification

| Scope | Registered defects | Complete defects | Fraction |
| --- | ---: | ---: | ---: |
| Broad ledger | 61 | 107 | 57.01% |
| Project runtime/security | 47 | 61 | 77.05% |
| All project-authored corrections | 61 | 94 | 64.89% |

The original baseline increased from **24/84 (28.57%)** to **44/84 (52.38%)** without
removing defects or treating test variants as extra bugs. The newly covered rows
are F05, F07–F09, F12–F13, F15–F17, F20, F29, F33–F34, F37–F39, F52, F54, F68 and F71.

The follow-up also discovered and fixed two new defects: concurrent test-fixture
collisions (F85) and the mobile constructor discarding initialization failures
(F86). Both have focused regressions. Appending them produces **46/86 (53.49%)**;
the original 84-row denominator and its uncredited rows remain intact.

The publication follow-up adds F87: an incomplete upload could previously leave
a public release exposed. The real publication shell now verifies all uploaded
assets in a draft before making it public. Its failure/order fixtures register
this additional defect, giving the then-current **47/87 (54.02%)** total.

The relay revocation correction (F88) closes the actual authenticated socket
before discarding its sender and rejects revoked reconnects before registration.
Its direct-sender and real-WebSocket regressions produce **48/88 (54.55%)**.
The FIFO correction (F89) is one credential-opening defect across token, TLS key
and certificate paths. With production source `822af5d1` and the new tests, all
three writer-less FIFO cases reached the five-second rejection deadline; the
nonblocking descriptor validation in `39e32b2` passes them and preserves symlink
support. These additions produce **49/89 (55.06%)** before the completeness
reconciliation below. Local RED/GREEN was observed on macOS; Linux execution
belongs to its CI run, and Unix FIFO cases are not registered for Windows.

A source-history completeness review also registers two distinct PR #21 defects
fixed by `2ebdde2`: valid Windows GNU binary checksum records were rejected (F90),
and an ancestry-only container guard could accept an old workflow commit after
the release tag moved (F91). The publication-shell replay rejects the Windows
`hash *basename` record before the fix; the current publisher accepts it while
binding the actual digest and exact basename. Real local Git fixtures reproduce
the retargeted tag accepted by the old guard and rejected by fresh-fetch source
validation. These are Unix publisher/control tests; a Windows archive format
is not a claim of execution on a Windows host.

The reconstruction-wrapper correction first recorded as `c847d2d`, integrated
as `c5ad280`, contains two materially distinct failures. F92 is report lifecycle:
a failed rerun overwrote the source identity while retaining earlier success
markers. F93 is environment identity: the multiarch base left Docker's target
architecture host-selected and unrecorded. Synthetic Docker fixtures replay the
old wrapper and protect the corrected per-run report, platform arguments and
platform record. They do not run independent builds or prove cross-architecture
output identity. Failed build/comparison variants are not extra defects. F90–F93
bring the ledger to **53/93 (56.99%)**.

F94 records the later mobile fixture readiness correction in `2e42f06`: the room
file could become visible before background pairing persisted its sync setting.
It remains **uncredited**. The targeted current fixture passes, but the old one
can pass intermittently and no deterministic focused RED/GREEN regression has
been established. This is a test-fixture defect, not a newly claimed mobile
product failure. Including it in the denominator yields the current
**53/94 (56.38%)**, rather than increasing credit from nearby passing tests.

The next additions are Linux backend metadata (F95), native clipboard FIFO
opening (F96), and coverage source-path classification (F97). Isolated Linux
backend fixtures fail the old unconditional metadata; both native FIFO readers
hit a five-second deadline before nonblocking descriptor validation. Adversarial
coverage reports expose test-path aliases and duplicate build roots accepted by
the old validator. The corrected real report retains identical totals. These
three distinct failures produce **56/97 (57.73%)**. FIFO variants and path aliases
are not counted as additional defects.

F98 records the actual early-joiner pairing failure discovered during coverage
work. A real protocol fence and a delayed create-response fixture reproduce lost
presence/SPAKE delivery. Both public Core clients now complete and persist the
same keys. Session authentication, bounded pending storage, replacement and
disconnect/expiry contracts protect the correction. One ordering/session-lifecycle
defect brings the complete ledger to **57/98 (58.16%)**.

The final reconciliation adds malformed extra-CA handling (F99), three
uncredited fixture corrections (F100–F102), UniFFI macro nondeterminism (F103),
lost reconstruction diagnostics (F104), and locale-dependent policy decoding
(F105). F99, F104 and F105 have focused locally reproduced failures. F103 has
three deterministic-emission and two semantic vendor regressions, but this
ledger conservatively gives no credit because its Rust inventory covers only
product-workspace packages; those tests run in the separate locked upstream CI
workspace. No unrelated test is substituted for them. The final total is
**60/105 (57.14%)**, retaining all earlier rows and honest missing-credit reasons.

F106 records the output-directory ownership correction after the next actual
hosted run passed both six-output and native-package comparisons but failed
strict host cleanup. A separate real Docker root producer/UID1001 consumer
reproduces the unlink failure and verifies the directory-only ownership handoff
without mode or content changes. Synthetic Python contracts protect the caller
handoff and reject malformed IDs before Docker. The complete ledger is now
**61/106 (57.55%)**; cleanup errors remain fatal and the full reconstruction
must finish successfully before the complete workflow is called passing.

F107 records the pairing persistence stall exposed while validating the remaining
dependency updates. On unchanged `360ebb4`, three concurrent copies of the real
relay e2e suite failed when synchronous room-key writes occupied runtime workers
for 8–10 seconds and the existing eight-second watchdog closed the connection.
Cryptographic confirmation had succeeded. The same workload passes all 30 tests
with [persistence on blocking workers](https://github.com/Jeon1691/clipsync/commit/bfccecc794d00618efdfe877c615b61bc4613d3d).
The worker owns the full zeroizing key object, and persistence errors propagate.
The [bounded macOS CI harness](../scripts/test-pairing-concurrency.py) retains
per-copy evidence and kills/reaps remaining processes on failure or timeout.
This native timing/load case remains **uncredited** pending a deterministic
focused regression; the complete total is **61/107 (57.01%)**. No watchdog
threshold, existing regression reference or coverage threshold is relaxed.

The scope table is derived from the ledger: runtime/security includes the
`Runtime` and `Runtime-PR16` rows; project-authored corrections include every
kind except `Dependency`. All F01–F88 rows and their existing references remain
unchanged. The initial baseline and intermediate fractions above are history,
not competing current totals.

Historical staging source, older packaging scripts and isolated reversals of
the relevant fix hunks fail the targeted cases; current source passes. The proof
is scoped: a hunk-level reversal is not described as a complete historical
application build. Linux service fixtures run the real public installer in
separate processes with isolated homes and fake service-manager programs; they
do not change the developer's login services. Native pasteboard fixtures use
private named boards. TLS tests isolate private trust roots in child processes
and do not alter the host trust store.

F54 runs the verified packaged Linux CLI in a digest-pinned minimal distroless
container without desktop/DBus libraries. It is part of release smoke testing
and a dependency of publication. Its local runtime validation used a verified
release binary; the candidate's release workflow provides the execution record.

**Conservative exceptions remain:** F02's new foreground-output contract test
has no established historical counterexample. F19 and F41's native compatibility
fixtures also pass old behavior on current AppKit because it synthesizes TIFF
and modern URLs. These rows receive no credit. Final file permissions alone do
not reproduce the old creation-time permission window (F51), so it stays uncredited.

## CI policy

```sh
python3 scripts/check-regressions.py --verify-tests --report target/regression-policy.json
python3 -m unittest discover -s scripts/tests -v
```

CI runs the policy after workspace tests on Linux, macOS and Windows and retains
per-platform JSON artifacts. It rejects a fraction below 50%, removed baseline
IDs, manual coverage flags, missing/ignored tests, wrong package/target references
and disabled or unwired workflow checks. Other-platform and release-only
registrations remain explicitly distinguished from local discovery/execution.
New bug fixes must update the ledger with a focused test or an honest missing-test
reason; new defects increase the denominator. Human source review still decides
whether the assertion meaningfully protects the stated behavior.

## Full defect ledger

| ID | Type | Fixes | Distinct corrected defect | Automated checks / limitation |
| --- | --- | --- | --- | --- |
| F01 | Runtime | [7fecc3a](https://github.com/Jeon1691/clipsync/commit/7fecc3a) | One-shot pull --wait ignored binary manifest/chunk transfers | **Registered** — [file_push_pull_writes_output](../crates/clipsync-relay/tests/e2e.rs) (linux, macos, windows); [image_push_pull_writes_output](../crates/clipsync-relay/tests/e2e.rs) (linux, macos, windows); Tests added later by8e39471 exercise both binary receive paths; one defect, two item types. |
| F02 | Runtime | [7fecc3a](https://github.com/Jeon1691/clipsync/commit/7fecc3a) | Foreground pairing code was not flushed when stdout was captured | **Uncredited**; The new foreground capture contract is tested, but no historical counterexample was established; it remains uncredited rather than counting nearby passing behavior as a regression. |
| F03 | Runtime | [8e39471](https://github.com/Jeon1691/clipsync/commit/8e39471) | One-shot push sent content before relay acceptance | **Uncredited**; File/text e2e tests are adjacent positive coverage; no test delays/rejects hello or asserts that sending waits for acceptance. |
| F04 | Runtime | [ddcbdaf](https://github.com/Jeon1691/clipsync/commit/ddcbdaf) | Unix background pairing worker died on terminal hangup | **Uncredited**; No test sends SIGHUP or closes its controlling terminal. |
| F05 | Runtime | [ddcbdaf](https://github.com/Jeon1691/clipsync/commit/ddcbdaf) | Background-worker startup failure discarded recorded/logged error details | **Registered** — [background_failure_preserves_the_child_relay_error](../crates/clipsync-cli/tests/background_create.rs) (linux, macos, windows); Failed worker error is observed through the actual CLI child process. |
| F06 | Runtime | [ddcbdaf](https://github.com/Jeon1691/clipsync/commit/ddcbdaf) | A transient pairing WebSocket connection failure was not retried | **Uncredited**; No first-connect-fails/second-connect-succeeds test. |
| F07 | Runtime | [ddcbdaf](https://github.com/Jeon1691/clipsync/commit/ddcbdaf) | Identity loaded from OS keychain was not cached for subsequent file-based/background access | **Registered** — [secrets::regression_tests::keyring_only_identity_is_cached_for_file_based_background_clients](../crates/clipsync-storage/src/secrets.rs) (macos, windows); Keyring-only identity is cached for a subsequent file-based background client; platform keyring tests run on macOS/Windows. |
| F08 | Runtime | [ddcbdaf](https://github.com/Jeon1691/clipsync/commit/ddcbdaf) | HTTP user agent retained hardcoded0.1.0 after upgrades | **Registered** — [regression_tests::http_requests_identify_the_current_package_version](../crates/clipsync-core/src/lib.rs) (linux, macos, windows); Real local HTTP request checks the current package version in User-Agent. |
| F09 | Runtime | [e9e55af](https://github.com/Jeon1691/clipsync/commit/e9e55af), [0767bb6](https://github.com/Jeon1691/clipsync/commit/0767bb6) | Corporate/private CA trust did not follow OS policy consistently for HTTPS/WSS | **Registered** — [private_ca_https_trust_preserves_hostname_and_issuer_checks](../crates/clipsync-transport/tests/tls_policy.rs) (linux, macos, windows); [private_ca_wss_trust_preserves_hostname_and_issuer_checks](../crates/clipsync-transport/tests/tls_policy.rs) (linux, macos, windows); Private-CA HTTPS/WSS tests check trusted issuer success, wrong hostname and unrelated issuer rejection in isolated child processes. Omitting the configured roots (the historical failing trust behavior) fails both positive fixtures; no system trust store is modified. |
| F10 | Runtime | [e9e55af](https://github.com/Jeon1691/clipsync/commit/e9e55af) | macOS system HTTP proxy configuration was ignored | **Uncredited**; No proxy integration fixture. |
| F11 | Runtime | [e9e55af](https://github.com/Jeon1691/clipsync/commit/e9e55af) | Error formatting omitted underlying network/TLS source-chain details | **Registered** — [error::tests::format_error_includes_source_chain](../crates/clipsync-core/src/error.rs) (linux, macos, windows); Added in the fix; nested error-chain text is asserted. |
| F12 | Runtime | [6bc13cd](https://github.com/Jeon1691/clipsync/commit/6bc13cd) | Routine launchctl unload/replacement commands polluted pairing output | **Registered** — [successful_service_install_does_not_leak_launchctl_output](../crates/clipsync-daemon/tests/service_regressions.rs) (macos); Generated service behavior is exercised with isolated process fixtures; historical fix-hunk reversal reproduces the defect. |
| F13 | Runtime | [75194ed](https://github.com/Jeon1691/clipsync/commit/75194ed) | Replacing a macOS LaunchAgent left it disabled at the next login | **Registered** — [replacing_launch_agent_leaves_it_enabled_for_the_next_login](../crates/clipsync-daemon/tests/service_regressions.rs) (macos); Generated service behavior is exercised with isolated process fixtures; historical fix-hunk reversal reproduces the defect. |
| F14 | Runtime | [75194ed](https://github.com/Jeon1691/clipsync/commit/75194ed) | Service executable retained a version-specific Homebrew Cellar path | **Registered** — [service::tests::stable_exe_rewrites_cellar_when_prefix_bin_exists](../crates/clipsync-daemon/src/service.rs) (linux, macos, windows); Added with fix; asserts stable prefix executable. |
| F15 | Runtime | [75194ed](https://github.com/Jeon1691/clipsync/commit/75194ed) | Background service lost HOME/PATH/custom home/private-CA environment | **Registered** — [launchd_install_preserves_complete_runtime_environment](../crates/clipsync-daemon/tests/service_regressions.rs) (macos); [systemd_install_preserves_complete_runtime_environment](../crates/clipsync-daemon/tests/service_regressions.rs) (linux); Platform-specific tests verify the complete environment emitted by actual service installation. |
| F16 | Runtime | [75194ed](https://github.com/Jeon1691/clipsync/commit/75194ed) | Linux daemon was not restarted after a successful unexpected exit | **Registered** — [systemd_install_restarts_the_daemon_after_successful_exit](../crates/clipsync-daemon/tests/service_regressions.rs) (linux); Generated service behavior is exercised with isolated process fixtures; historical fix-hunk reversal reproduces the defect. |
| F17 | Runtime | [559f660](https://github.com/Jeon1691/clipsync/commit/559f660) | Linux background service omitted graphical/session-bus environment | **Registered** — [systemd_install_passes_the_graphical_session_to_the_daemon](../crates/clipsync-daemon/tests/service_regressions.rs) (linux); Generated service behavior is exercised with isolated process fixtures; historical fix-hunk reversal reproduces the defect. |
| F18 | Runtime | [559f660](https://github.com/Jeon1691/clipsync/commit/559f660) | systemd environment values were emitted without required quoting | **Registered** — [service::tests::platform_systemd_env_quotes_session_bus](../crates/clipsync-daemon/src/service.rs) (linux); Test added later by40b39fb asserts exact quoted Environment line. |
| F19 | Runtime | [f1a1acd](https://github.com/Jeon1691/clipsync/commit/f1a1acd) | macOS image clipboard lacked TIFF representation needed by receiving applications | **Uncredited**; New native TIFF compatibility/advertisement fixtures also pass with PNG-only writes because current AppKit synthesizes TIFF. No defensible historical regression is claimed. |
| F20 | Runtime | [f1a1acd](https://github.com/Jeon1691/clipsync/commit/f1a1acd) | Command clipboard file URIs failed to percent-encode special characters | **Registered** — [command::regression_tests::command_file_clipboard_roundtrip_preserves_uri_special_characters](../crates/clipsync-clipboard/src/command.rs) (linux, macos); Command adapter subprocess fixtures assert real file/text output; historical fix-hunk reversal reproduces the defect. |
| F21 | Runtime | [f1a1acd](https://github.com/Jeon1691/clipsync/commit/f1a1acd) | Daemon began sync without consuming initial relay hello/queued peer state | **Uncredited**; No run_session/initial queued-text/initial-peer-state regression; one-shot e2e tests are a different path. |
| F22 | Runtime | [f1a1acd](https://github.com/Jeon1691/clipsync/commit/f1a1acd) | Failed local clipboard writes were silently treated as successful receive notifications | **Uncredited**; No failing ClipboardBackend fixture or notification assertion. |
| F23 | Runtime | [5a4b01c](https://github.com/Jeon1691/clipsync/commit/5a4b01c) | Sleep/wake clock jumps left a dead relay connection in place | **Registered** — [tests::clock_jump_after_sleep_forces_reconnect](../crates/clipsync-transport/src/lib.rs) (linux, macos, windows); Added in fix; thirty-second clock jump must reconnect. |
| F24 | Runtime | [5a4b01c](https://github.com/Jeon1691/clipsync/commit/5a4b01c) | A silent/stale socket was retained beyond heartbeat timeout | **Registered** — [tests::stale_socket_forces_reconnect](../crates/clipsync-transport/src/lib.rs) (linux, macos, windows); Added in fix; stale receive time with fresh watchdog must reconnect. |
| F25 | Runtime | [5a4b01c](https://github.com/Jeon1691/clipsync/commit/5a4b01c) | Initial reconnect backoff was unnecessarily multi-second | **Registered** — [tests::first_backoff_is_subsecond](../crates/clipsync-transport/src/lib.rs) (linux, macos, windows); Added in fix; first delay is constrained below one second. |
| F26 | Runtime | [5a4b01c](https://github.com/Jeon1691/clipsync/commit/5a4b01c) | Send failures did not end the active sync session to reconnect | **Uncredited**; No injected send-failure/run_session regression; timer helper tests cannot establish this behavior. |
| F27 | Runtime | [b472108](https://github.com/Jeon1691/clipsync/commit/b472108) | Windows background child inherited captured stdout and blocked the creator | **Registered** — [room_create_background_returns_then_pairs](../crates/clipsync-cli/tests/background_create.rs) (linux, macos, windows); Captured Command.output plus elapsed<10s assertion targets the handle-inheritance failure; CI later enabled Windows execution. |
| F28 | Runtime | [22d3e56](https://github.com/Jeon1691/clipsync/commit/22d3e56) | Cellar executable normalization missed Windows backslash paths | **Registered** — [service::tests::stable_exe_rewrites_cellar_when_prefix_bin_exists](../crates/clipsync-daemon/src/service.rs) (windows); On Windows the fixture PathBuf renders with backslashes; later three-platform CI applies it to that target. Same test covers a second distinct platform defect, not extra test count. |
| F29 | Runtime | [9c837f1](https://github.com/Jeon1691/clipsync/commit/9c837f1) | Linux non-file clipboard content caused file-URI errors instead of text fallback | **Registered** — [command::regression_tests::misleading_file_mime_falls_back_to_actual_text](../crates/clipsync-clipboard/src/command.rs) (linux, macos); [command::regression_tests::vanished_file_uri_falls_back_to_actual_text](../crates/clipsync-clipboard/src/command.rs) (linux, macos); Command adapter subprocess fixtures assert real file/text output; historical fix-hunk reversal reproduces the defect. |
| F30 | Runtime | [61bbfcc](https://github.com/Jeon1691/clipsync/commit/61bbfcc) | Short runtime stalls were mistaken for sleep and forced reconnects | **Registered** — [tests::stability_short_stall_is_not_sleep](../crates/clipsync-transport/src/lib.rs) (linux, macos, windows); Added in fix; three-second stall must not reconnect. |
| F31 | Runtime | [61bbfcc](https://github.com/Jeon1691/clipsync/commit/61bbfcc) | A long healthy session did not reset reconnection failure streak | **Registered** — [tests::stability_long_session_resets_backoff](../crates/clipsync-transport/src/lib.rs) (linux, macos, windows); [tests::stability_backoff_stays_bounded](../crates/clipsync-transport/src/lib.rs) (linux, macos, windows); Added in fix; uptime reset and bounded repeated failures are asserted. |
| F32 | Runtime | [61bbfcc](https://github.com/Jeon1691/clipsync/commit/61bbfcc) | Incomplete incoming transfers were retained beyond their lifetime | **Registered** — [staging::tests::stability_idle_transfers_are_pruned](../crates/clipsync-transfer/src/staging.rs) (linux, macos, windows); Added in fix; expired transfer removed and map emptied. |
| F33 | Runtime | [61bbfcc](https://github.com/Jeon1691/clipsync/commit/61bbfcc) | Concurrent unfinished transfer count could grow without a fixed bound | **Registered** — [regression_f33_fresh_transfers_evict_oldest_and_preserve_newest_data](../crates/clipsync-transfer/tests/regressions.rs) (linux, macos, windows); Focused behavior assertions; historical RED/GREEN evidence retained under coverage/regression-transfer/. Multiple cases still count as one distinct defect. |
| F34 | Runtime | [fc64071](https://github.com/Jeon1691/clipsync/commit/fc64071) | Image decoding had no explicit memory allocation ceiling | **Registered** — [item::allocation_regression_tests::compressed_image_cannot_allocate_beyond_the_decode_budget](../crates/clipsync-clipboard/src/item.rs) (linux, macos, windows); A valid compressed 36 MiB decoded PNG exceeds the 32 MiB allocation budget; unrestricted positive decode validates the input. |
| F35 | Runtime | [fc64071](https://github.com/Jeon1691/clipsync/commit/fc64071) | Peer-provided transfer IDs could escape the inbox path | **Registered** — [staging::tests::transfer_id_cannot_escape_inbox](../crates/clipsync-transfer/src/staging.rs) (linux, macos, windows); [sanitize::tests::transfer_id_must_be_ulid](../crates/clipsync-transfer/src/sanitize.rs) (linux, macos, windows); Added in fix; ../escape rejected and outside output must not exist. |
| F36 | Runtime | [fc64071](https://github.com/Jeon1691/clipsync/commit/fc64071) | File names accepted Windows reserved/device/alternate-stream and trailing-dot forms | **Registered** — [sanitize::tests::rejects_traversal](../crates/clipsync-transfer/src/sanitize.rs) (linux, macos, windows); Fix extends existing test with secret.txt:ads,CON,aux.txt,file.; one filename validation defect, not one bug per string. |
| F37 | Runtime | [fc64071](https://github.com/Jeon1691/clipsync/commit/fc64071) | Duplicate names in a received file manifest could overwrite each other | **Registered** — [regression_f37_duplicate_names_preserve_the_previous_commit](../crates/clipsync-transfer/tests/regressions.rs) (linux, macos, windows); Focused behavior assertions; historical RED/GREEN evidence retained under coverage/regression-transfer/. Multiple cases still count as one distinct defect. |
| F38 | Runtime | [fc64071](https://github.com/Jeon1691/clipsync/commit/fc64071) | Incoming chunks could exceed allowed count/size/cumulative transfer memory | **Registered** — [regression_f38_chunk_count_above_limit_is_rejected_and_poisoned](../crates/clipsync-transfer/tests/regressions.rs) (linux, macos, windows); [regression_f38_oversized_chunk_cannot_be_assembled](../crates/clipsync-transfer/tests/regressions.rs) (linux, macos, windows); [regression_f38_cumulative_size_rejects_one_byte_over_limit](../crates/clipsync-transfer/tests/regressions.rs) (linux, macos, windows); Focused behavior assertions; historical RED/GREEN evidence retained under coverage/regression-transfer/. Multiple cases still count as one distinct defect. |
| F39 | Runtime | [fc64071](https://github.com/Jeon1691/clipsync/commit/fc64071) | File commit accepted an excessive manifest file count or total payload | **Registered** — [regression_f39_excessive_file_count_leaves_no_output](../crates/clipsync-transfer/tests/regressions.rs) (linux, macos, windows); [regression_f39_excessive_payload_leaves_no_output](../crates/clipsync-transfer/tests/regressions.rs) (linux, macos, windows); Focused behavior assertions; historical RED/GREEN evidence retained under coverage/regression-transfer/. Multiple cases still count as one distinct defect. |
| F40 | Runtime | [399c845](https://github.com/Jeon1691/clipsync/commit/399c845) | Finder file paste did not reliably target the folder with focus | **Uncredited**; desktop_paste_lists_every_absolute_path verifies path/file data but does not perform Finder focused-folder paste or assert its destination; conservativelyNo. |
| F41 | Runtime | [399c845](https://github.com/Jeon1691/clipsync/commit/399c845) | Filename-list-only macOS clipboard copies were not read as files | **Uncredited**; A new filename-list-only native fixture remains useful characterization, but current AppKit synthesizes modern URLs even for the historical read path. It is not credited as a reproduced regression. |
| F42 | Runtime | [399c845](https://github.com/Jeon1691/clipsync/commit/399c845) | Images could not be pasted as files on Finder/Desktop | **Registered** — [macos::tests::desktop_image_paste_is_a_file_and_reads_back_as_the_same_image](../crates/clipsync-clipboard/src/macos.rs) (macos); Added in fix; requires staged file list and original image readback. Sidecar echo prevention is part of this new mechanism, not an extra historical bug. |
| F43 | Runtime | [399c845](https://github.com/Jeon1691/clipsync/commit/399c845) | JPEG bytes were exposed through a PNG pasteboard representation without transcoding | **Registered** — [macos::tests::desktop_jpeg_paste_keeps_original_bytes](../crates/clipsync-clipboard/src/macos.rs) (macos); Added in fix; requires JPEG sidecar preservation and an actual different PNG representation. |
| F44 | Runtime | [8b6c6ce](https://github.com/Jeon1691/clipsync/commit/8b6c6ce) | Both pairing participants reused the same GCM key and nonce | **Registered** — [pairing::tests::device_bind_nonces_separate_roles_and_legacy_indices](../crates/clipsync-core/src/pairing.rs) (linux, macos, windows); [pairing::tests::device_bind_new_a_and_legacy_b_interoperate](../crates/clipsync-core/src/pairing.rs) (linux, macos, windows); [pairing::tests::device_bind_legacy_a_and_new_b_interoperate](../crates/clipsync-core/src/pairing.rs) (linux, macos, windows); [pairing_device_bind_nonces_are_unique](../crates/clipsync-relay/tests/e2e.rs) (linux, macos, windows); Added in fix; separate sender domains/legacy interoperability and observed live exchange. |
| F45 | Runtime | [ce89c9b](https://github.com/Jeon1691/clipsync/commit/ce89c9b) | Compose deployed a public shared relay token-signing key | **Uncredited**; Env default tests do not load Docker Compose. The old unset-env path already generated fresh keys; restoring the Compose shared value would not fail them. No Compose-specific regression assertion. |
| F46 | Runtime | [ce89c9b](https://github.com/Jeon1691/clipsync/commit/ce89c9b) | Explicit empty/whitespace/short relay token secrets were accepted as signing keys | **Registered** — [short_explicit_token_secrets_are_rejected_without_disclosing_them](../crates/clipsync-relay/tests/config_env.rs) (linux, macos, windows); [minimum_length_token_secret_is_preserved](../crates/clipsync-relay/tests/config_env.rs) (linux, macos, windows); Added in fix; invalid key lengths rejected without disclosure. Input examples are one validation defect. |
| F47 | Runtime-PR16 | [d0501a7](https://github.com/Jeon1691/clipsync/commit/d0501a7) | Webhook DNS resolution/rebinding could bypass the literal-IP SSRF policy | **Registered** — [webhook::tests::rejects_private_or_mixed_dns_answers](../crates/clipsync-relay/src/webhook.rs) (linux, macos, windows); [webhook::tests::rejects_a_hostname_resolving_to_loopback](../crates/clipsync-relay/src/webhook.rs) (linux, macos, windows); [webhook::tests::client_pins_addresses_preserves_host_and_does_not_follow_redirects](../crates/clipsync-relay/src/webhook.rs) (linux, macos, windows); Includes pinned connect destinations and no implicit proxy/redirect bypass as one mediation defect. |
| F48 | Runtime-PR16 | [d0501a7](https://github.com/Jeon1691/clipsync/commit/d0501a7) | Non-public IPv4/IPv6 literals outside the old limited denylist were accepted by webhooks | **Registered** — [webhook::tests::blocks_non_public_literals](../crates/clipsync-relay/src/webhook.rs) (linux, macos, windows); Distinct literal-address validation path; parameterized special-range fixture. |
| F49 | Runtime-PR16 | [d0501a7](https://github.com/Jeon1691/clipsync/commit/d0501a7) | Device bearer credentials were serialized together with ordinary room state | **Registered** — [device_tokens_are_separate_and_replaceable](../crates/clipsync-storage/tests/credentials.rs) (linux, macos, windows); [legacy_state_migrates_without_losing_room_or_token](../crates/clipsync-storage/tests/credentials.rs) (linux, macos, windows); Replacement, absence from metadata, legacy migration, fail-closed missing/truncated files covered. Migration safeguards not counted as extra historical defects. |
| F50 | Runtime-PR16 | [d0501a7](https://github.com/Jeon1691/clipsync/commit/d0501a7) | Webhook authentication secret was serialized in ordinary configuration | **Registered** — [legacy_webhook_secret_migrates_and_is_removed_when_disabled](../crates/clipsync-storage/tests/credentials.rs) (linux, macos, windows); [failed_migration_preserves_legacy_secret](../crates/clipsync-storage/tests/credentials.rs) (linux, macos, windows); Tests actual separation and preserve legacy config after failed migration. |
| F51 | Runtime-PR16 | [d0501a7](https://github.com/Jeon1691/clipsync/commit/d0501a7) | Secret files were created/written before private permissions were applied | **Uncredited**; credentials_are_private asserts final0600, which old code also achieved; it does not observe the creation/write permission window. |
| F52 | Runtime-PR16 | [d0501a7](https://github.com/Jeon1691/clipsync/commit/d0501a7) | Overwriting a secret in place could destroy the previous credential on a failed/partial write | **Registered** — [partial_secret_write_does_not_destroy_the_previous_credential](../crates/clipsync-storage/tests/credentials.rs) (linux, macos); Unix injected partial-write failure preserves the prior real credential. |
| F53 | Runtime-PR16 | [d0501a7](https://github.com/Jeon1691/clipsync/commit/d0501a7) | App.init silently replaced unreadable/invalid configuration with defaults | **Registered** — [init_preserves_legacy_config_when_secret_migration_fails](../crates/clipsync-core/tests/config_migration.rs) (linux, macos, windows); [init_preserves_invalid_configuration_instead_of_resetting_it](../crates/clipsync-core/tests/config_migration.rs) (linux, macos, windows); One destructive fallback root cause, tested for invalidTOML and failed migration. |
| F54 | Build/CI | [9119714](https://github.com/Jeon1691/clipsync/commit/9119714) | Linux release depended on a DBus keyring unavailable on target systems | **Registered** — [Run verified Linux binary without desktop or DBus libraries](../.github/workflows/release.yml) (linux); Added release-only automated no-DBus runtime check; release smoke must precede publication. Registration is not a claim it ran in ordinary PR CI. |
| F55 | Build/CI | [23d7e01](https://github.com/Jeon1691/clipsync/commit/23d7e01) | Workspace lockfile package version lagged the0.1.1 manifest version | **Uncredited**; Existing --locked build verifies general consistency but no new dedicated regression was added for this fix. |
| F56 | Build/CI | [a667731](https://github.com/Jeon1691/clipsync/commit/a667731) | Windows CRLF checkout caused rustfmt CI failures | **Uncredited**; Line-ending configuration correction; no new focused regression beyond existing fmt command. |
| F57 | Build/CI | [5750bf6](https://github.com/Jeon1691/clipsync/commit/5750bf6) | Desktop lock entry retained an unused dependency and failed locked consistency | **Uncredited**; No new focused regression; ordinary locked build already existed. |
| F58 | Build/CI | [d1660a8](https://github.com/Jeon1691/clipsync/commit/d1660a8) | Checked-in winget portable installer structure/schema did not match accepted submission | **Uncredited**; No automated winget schema-validation test; PowerShell parser check only covers syntax. |
| F59 | Build/CI | [d1660a8](https://github.com/Jeon1691/clipsync/commit/d1660a8) | winget package omitted required VC++ runtime dependency | **Uncredited**; No fresh-Windows/no-runtime install regression test. |
| F60 | Build/CI | [cefdc04](https://github.com/Jeon1691/clipsync/commit/cefdc04) | File-format test attempted more files in one transfer than FILE_MAX_COUNT | **Uncredited**; Batching fixes test data; no separate automated test of batching boundary was added. ConservativelyNo for test-harness bug. |
| F61 | Build/CI | [b28a3fd](https://github.com/Jeon1691/clipsync/commit/b28a3fd) | Declared Rust1.80 minimum was lower than dependency requirements | **Uncredited**; New1.88 job validates supported compiler, but does not assert manifest minimum agrees with it. ConservativelyNo for inaccurate declaration. |
| F62 | Build/CI | [b28a3fd](https://github.com/Jeon1691/clipsync/commit/b28a3fd) | Fuzz invocation could silently regenerate/use a stale lockfile | **Registered** — [Fuzz protocol decoders](../.github/workflows/security.yml) (linux); Registration checks the job/step and required metadata command; execution belongs to the security CI job, not this checker. |
| F63 | Build/CI | [682f937](https://github.com/Jeon1691/clipsync/commit/682f937) | Release builds selected retired macos-14 runners | **Uncredited**; No newly added assertion against retired runners; successful workflow is repair verification, not an added regression test. |
| F64 | Build/CI | [682f937](https://github.com/Jeon1691/clipsync/commit/682f937) | Attestation filename suffix was not recognized as standard Sigstore JSON by downstream tooling | **Uncredited**; No suffix/asset recognition regression test; signature verification accepts an explicit filename independent of suffix. |
| F65 | Build/CI | [000d515](https://github.com/Jeon1691/clipsync/commit/000d515) | Format-matrix sender raced receiver registration and intermittently lost online-only transfers | **Uncredited**; Replaced sleep with metrics readiness in existing tests; no separate readiness helper failure/race regression added. ConservativelyNo. |
| F66 | Build/CI | [a3c92f5](https://github.com/Jeon1691/clipsync/commit/a3c92f5) | Workflow default tokens unnecessarily had write privileges outside publishing | **Uncredited**; CodeQL/Scorecard monitoring is static policy analysis, not an added focused regression test fixture. |
| F67 | Build/CI | [a3c92f5](https://github.com/Jeon1691/clipsync/commit/a3c92f5) | Build actions/container references were mutable rather than content pinned | **Uncredited**; Supply-chain correction included here for conservative completeness; no new focused pin-policy regression fixture. |
| F68 | Build/CI-PR16 | [d0501a7](https://github.com/Jeon1691/clipsync/commit/d0501a7) | Release packaging stripped symbols even when debug preservation was requested | **Registered** — [test_package_release.PackageReleaseTests.test_standard_no_strip_setting_survives_the_archive](../scripts/tests/test_package_release.py) (linux, macos); [test_package_release.PackageReleaseTests.test_requested_debug_information_survives_the_archive](../scripts/tests/test_package_release.py) (linux, macos); [test_package_release.PackageReleaseTests.test_explicit_debug_preservation_survives_the_archive](../scripts/tests/test_package_release.py) (linux, macos); Packaging tests inspect archived executable debug bytes and checksums, not source text. |
| F69 | Build/CI-PR16 | [8d1735a](https://github.com/Jeon1691/clipsync/commit/8d1735a) | Coverage branch instrumentation accidentally ran with stable rather than selected nightly | **Uncredited**; Toolchain env fix; no dedicated toolchain-selection assertion added. |
| F70 | Build/CI-PR16 | [8d1735a](https://github.com/Jeon1691/clipsync/commit/8d1735a) | Repeatability container git checkout failed safe-directory ownership checks | **Uncredited**; Container config fix; no separate ownership regression fixture. |
| F71 | Build/CI-PR16 | [a61aed0](https://github.com/Jeon1691/clipsync/commit/a61aed0) | Cross-target packaging failed on host strip errors after stricter packaging change | **Registered** — [test_package_release.PackageReleaseTests.test_cross_target_archive_survives_an_unsupported_host_strip](../scripts/tests/test_package_release.py) (linux, macos); An unsupported strip executable fails; the real packaging script must still produce the expected archive. |
| F72 | Build/CI-PR16 | [3faac58](https://github.com/Jeon1691/clipsync/commit/3faac58) | Source notice checker skipped extensionless PKGBUILD/pre-commit sources | **Uncredited**; Checker enumeration fixed and notices added; no checker self-test fixture for an unannotated extensionless file. |
| F73 | Dependency | [1ab6590](https://github.com/Jeon1691/clipsync/commit/1ab6590) | RUSTSEC-2026-0194: quick-xml quadratic duplicate-attribute checking | **Uncredited**; No added project-owned reproducer/regression. Routine cargo-audit monitoring and upstream fixes alone are not counted as project test additions. |
| F74 | Dependency | [1ab6590](https://github.com/Jeon1691/clipsync/commit/1ab6590) | RUSTSEC-2026-0195: quick-xml namespace declaration allocation DoS | **Uncredited**; No added project-owned reproducer/regression. Routine cargo-audit monitoring and upstream fixes alone are not counted as project test additions. |
| F75 | Dependency | [1ab6590](https://github.com/Jeon1691/clipsync/commit/1ab6590) | RUSTSEC-2026-0346: rustls sessionDebug leaked TLS secrets | **Uncredited**; No added project-owned reproducer/regression. Routine cargo-audit monitoring and upstream fixes alone are not counted as project test additions. |
| F76 | Dependency | [1ab6590](https://github.com/Jeon1691/clipsync/commit/1ab6590) | RUSTSEC-2026-0347: rustls ECH rejection leaked buffered application data | **Uncredited**; No added project-owned reproducer/regression. Routine cargo-audit monitoring and upstream fixes alone are not counted as project test additions. |
| F77 | Dependency | [1ab6590](https://github.com/Jeon1691/clipsync/commit/1ab6590) | RUSTSEC-2026-0348: rustls Brotli certificate decompression allocated excessive memory | **Uncredited**; No added project-owned reproducer/regression. Routine cargo-audit monitoring and upstream fixes alone are not counted as project test additions. |
| F78 | Dependency | [1ab6590](https://github.com/Jeon1691/clipsync/commit/1ab6590) | RUSTSEC-2026-0349: rustls unauthenticated ServerHello poisoned later key exchange | **Uncredited**; No added project-owned reproducer/regression. Routine cargo-audit monitoring and upstream fixes alone are not counted as project test additions. |
| F79 | Dependency | [1ab6590](https://github.com/Jeon1691/clipsync/commit/1ab6590) | CVE-2026-111943: emSign distrust bypass across rustls-webpki and both root-list crates | **Uncredited**; No added project-owned reproducer/regression. Routine cargo-audit monitoring and upstream fixes alone are not counted as project test additions. |
| F80 | Dependency | [1ab6590](https://github.com/Jeon1691/clipsync/commit/1ab6590) | RUSTSEC-2026-0386: Tokio AsyncFd constructors violated I/O safety | **Uncredited**; No added project-owned reproducer/regression. Routine cargo-audit monitoring and upstream fixes alone are not counted as project test additions. |
| F81 | Dependency | [1ab6590](https://github.com/Jeon1691/clipsync/commit/1ab6590) | RUSTSEC-2026-0486: rustix xattr zero-length buffer marked unwritten bytes initialized | **Uncredited**; No added project-owned reproducer/regression. Routine cargo-audit monitoring and upstream fixes alone are not counted as project test additions. |
| F82 | Dependency | [63f23ed](https://github.com/Jeon1691/clipsync/commit/63f23ed) | RUSTSEC-2026-0523: quinn-proto MAX_STREAM_DATA bypassed stream limits | **Uncredited**; No added project-owned reproducer/regression. Routine cargo-audit monitoring and upstream fixes alone are not counted as project test additions. |
| F83 | Dependency | [63f23ed](https://github.com/Jeon1691/clipsync/commit/63f23ed) | RUSTSEC-2026-0524: quinn-proto long Retry token caused panic/exhaustion | **Uncredited**; No added project-owned reproducer/regression. Routine cargo-audit monitoring and upstream fixes alone are not counted as project test additions. |
| F84 | Dependency | [63f23ed](https://github.com/Jeon1691/clipsync/commit/63f23ed) | RUSTSEC-2026-0538: quinn-udp receive loop could spin after truncated datagram | **Uncredited**; No added project-owned reproducer/regression. Routine cargo-audit monitoring and upstream fixes alone are not counted as project test additions. |
| F85 | Build/CI | [01d4a8b](https://github.com/Jeon1691/clipsync/commit/01d4a8b0655e07fecbb16b0f302516de439a9f0f) | Parallel clipboard regression fixtures reused temporary files, producing cross-test clipboard results | **Registered** — [command::regression_tests::concurrent_command_fixtures_never_share_files](../crates/clipsync-clipboard/src/command.rs); Hosted macOS CI reproduced the wrong-fixture result. Atomic allocation and exclusive directory creation isolate each fixture; the concurrent test preserves this contract. |
| F86 | Runtime | [01d4a8b](https://github.com/Jeon1691/clipsync/commit/01d4a8b0655e07fecbb16b0f302516de439a9f0f) | Mobile FFI constructor discarded initialization errors and returned a usable client after configuration failure | **Registered** — [tests::mobile_constructor_and_commands_are_exercised_in_an_isolated_host](../crates/clipsync-mobile/src/lib.rs); The isolated bad-config child fails against the ignored-error constructor and passes with propagation. F53 fixed App.init itself; this distinct public FFI caller still discarded the returned error. |
| F87 | Build/CI | [d9ec242](https://github.com/Jeon1691/clipsync/commit/d9ec24225f22128a1fa2ef3df5c12b02d782b394) | Public release exposed before complete uploaded asset verification | **Registered** — actual publication shell rejects changed uploaded evidence and verifies a complete draft before public transition; Linux/macOS fixture, Ubuntu publisher. |
| F88 | Runtime | [7eb0c5c](https://github.com/Jeon1691/clipsync/commit/7eb0c5c2a2e8603b621efeb3676d25a8e91abd63) | Revoked relay device retained its authenticated socket; rejected Hello registered before membership validation | **Registered** — direct sender and real-WebSocket RED/GREEN regressions verify Close, blocked sends/reconnect, owner liveness and persisted revocation. |
| F89 | Runtime | [39e32b2](https://github.com/Jeon1691/clipsync/commit/39e32b21437e02223b9c93256d072d30b8690652) | Writer-less credential FIFOs blocked relay startup before file-type rejection | **Registered** — three bounded [token/key/certificate FIFO tests](../crates/clipsync-relay/tests/secure_listener.rs) fail at five seconds against pre-fix code and pass with nonblocking descriptor validation; one defect, Linux/macOS scope. |
| F90 | Build/CI | [2ebdde2](https://github.com/Jeon1691/clipsync/commit/2ebdde2102b5028e6689df0335ca966074b6a98b) | Publisher rejected valid GNU Windows binary checksum markers | **Registered** — [Windows-record acceptance](../scripts/tests/test_release_checksums.py) and [actual publication-shell fixture](../scripts/tests/test_release_publication.py) bind the digest and basename; Linux/macOS publisher tests. |
| F91 | Build/CI | [2ebdde2](https://github.com/Jeon1691/clipsync/commit/2ebdde2102b5028e6689df0335ca966074b6a98b) | Retargeted release tag could publish the older container workflow source | **Registered** — [real local-Git retarget/default-branch fixtures](../scripts/tests/test_release_source.py) reject stale source before package writes and version promotion; Linux/macOS. |
| F92 | Build/CI | [c5ad280](https://github.com/Jeon1691/clipsync/commit/c5ad280e8cacff1dc3d598b8f25ac1dbff2648d3) | Failed reconstruction rerun mixed a new source identity with earlier success evidence | **Registered** — [failed-rerun and build/comparison fixtures](../scripts/tests/test_build_reconstruction.py) preserve old reports and prevent current success markers; synthetic orchestration, Linux/macOS. |
| F93 | Build/CI | [c5ad280](https://github.com/Jeon1691/clipsync/commit/c5ad280e8cacff1dc3d598b8f25ac1dbff2648d3) | Reconstruction target architecture remained host-selected and unrecorded | **Registered** — [default amd64 and explicit arm64 fixtures](../scripts/tests/test_build_reconstruction.py) check both builds/runs and the platform record; no cross-architecture build-result claim, Linux/macOS. |
| F94 | Build/CI | [2e42f06](https://github.com/Jeon1691/clipsync/commit/2e42f06061a636a5f58b3aaf96a40d48fd75e00c) | Mobile fixture checked pairing completion before sync configuration persistence | **Uncredited** — intermittent old-fixture behavior has no deterministic focused regression; a current targeted pass is not historical proof. |
| F95 | Runtime | [a0022e6](https://github.com/Jeon1691/clipsync/commit/a0022e65146742d61bfe45fb6e3a91218db0dbe7) | Linux backend metadata always identified xclip | **Registered** — [isolated backend/session contracts](../crates/clipsync-clipboard/src/detect.rs) reproduce old metadata errors on Linux. |
| F96 | Runtime | [a0022e6](https://github.com/Jeon1691/clipsync/commit/a0022e65146742d61bfe45fb6e3a91218db0dbe7) | Native clipboard file reads blocked on writer-less FIFOs | **Registered** — [arboard file reader](../crates/clipsync-clipboard/src/detect.rs) and [AppKit reader](../crates/clipsync-clipboard/src/macos.rs) have bounded real FIFO regressions, macOS. |
| F97 | Build/CI | [8488848](https://github.com/Jeon1691/clipsync/commit/8488848b10d6cdcdf2b99391b0507fddbbb6472e) | Coverage source classification accepted noncanonical aliases and duplicate build roots | **Registered** — [adversarial report fixtures](../scripts/tests/test_product_coverage.py) reject test aliases and duplicate product files without changing valid real totals, Linux/macOS. |

| F98 | Runtime | [4cca45c](https://github.com/Jeon1691/clipsync/commit/4cca45c6937226657d32b484f0d2b8e0c3a72e65) | Early joiner socket lost creator presence and its first SPAKE message | **Registered** — [actual WebSocket ordering](../crates/clipsync-relay/tests/pairing_order.rs) and [full Core exchange/persisted keys](../crates/clipsync-relay/src/ws_tests.rs) use deterministic fences and private credentials, all three CI platforms. |


| F99 | Runtime | [c335d3c](https://github.com/Jeon1691/clipsync/commit/c335d3cca102d177b36eb457b8f18627ea52039f) | Linux ignored malformed configured extra CA roots | **Registered** — [isolated single/mixed certificate fixtures](../crates/clipsync-transport/src/tls.rs) now reject every invalid trust anchor before client construction; valid private-CA, hostname and issuer contracts remain, all platforms. |
| F100 | Build/CI | [c335d3c](https://github.com/Jeon1691/clipsync/commit/c335d3cca102d177b36eb457b8f18627ea52039f) | Windows deadline fixture assumed nanosecond SystemTime precision | **Uncredited** — a representable microsecond step fixes the fixture while preserving the exact production boundary; no independent fixture regression is claimed. |
| F101 | Build/CI | 5294762, 248fc9e, 4cca45c | Loopback fixtures closed sockets before consumers received queued terminal frames | **Uncredited** — consumer fences preserve existing product assertions; no deterministic isolated old-fixture regression is claimed. |
| F102 | Build/CI | 5cb9042 | Hostile-pairing fixture required a best-effort outbound error after client termination | **Uncredited** — exact local cryptographic failure and unchanged persistence remain asserted; no separate fixture regression is claimed. |
| F103 | Dependency | [276c37c](https://github.com/Jeon1691/clipsync/commit/276c37c92f975ee745fc91c7a126165e34e5b341) | UniFFI randomized descendant-check emission prevented reproducible build output | **Uncredited by this inventory** — [three macro regressions](../vendor/uniffi_internal_macros/src/node.rs) and [two semantic contracts](../vendor/uniffi_internal_macros/tests/node_descendants.rs) run in the separate locked upstream workspace; actual RED/GREEN expansion and binary comparison evidence is retained. |
| F104 | Build/CI | [276c37c](https://github.com/Jeon1691/clipsync/commit/276c37c92f975ee745fc91c7a126165e34e5b341) | Independent-container failure cleanup discarded differing binary bytes | **Registered** — [actual publication-wrapper diagnostic lifecycle](../scripts/tests/test_build_reconstruction.py) preserves both differing payloads and bounded diagnostics without success markers, Linux/macOS; synthetic orchestration is not build reproducibility proof. |
| F105 | Build/CI | [8e63e94](https://github.com/Jeon1691/clipsync/commit/8e63e94894cc5a752627df975cdfc3429966fe43) | Process-locale decoding rejected valid UTF-8 regression sources on Windows | **Registered** — [strict UTF-8 evidence contracts](../scripts/tests/test_regression_policy.py) cover real ASCII-locale subprocesses, Windows1252 simulation and malformed-source rejection without modifying source bytes, all platforms. |

| F106 | Build/CI | [7f6e976](https://github.com/Jeon1691/clipsync/commit/7f6e9767a28a5989c41fde1811924c2fa8058101) | Root-owned exported directories broke host cleanup after matching builds | **Registered** — [caller ownership and malformed-ID contracts](../scripts/tests/test_build_reconstruction.py), plus separate real Docker permission RED/GREEN; modes and payload hashes stay unchanged, Linux/macOS orchestration. |
| F107 | Runtime | [bfccecc](https://github.com/Jeon1691/clipsync/commit/bfccecc794d00618efdfe877c615b61bc4613d3d) | Blocking pairing credential persistence triggered the live-connection watchdog under native keychain load | **Uncredited** — old-source concurrent e2e failure and corrected 30-test pass are retained; macOS CI runs the bounded workload, but scheduler/keychain timing is not a deterministic focused fixture. |

The [official Silver criterion](https://www.bestpractices.dev/en/criteria/1#regression_tests_added50)
defines the badge requirement. This ledger's defect taxonomy and conservative
qualifications are project self-assessment evidence, not an official taxonomy
imposed by OpenSSF. See [testing](testing.md) for the separate production-source
line/region/branch measurements; a regression-defect fraction is not statement coverage.

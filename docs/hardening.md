# Binary hardening evidence

On 2026-11-15 the five published v0.1.16 archives were downloaded, their
SHA-256 checksums and GitHub/Sigstore provenance verified against source
`639000d60f1af1cc856fe20c61b60c84f9b2ab7b`, and their executable headers examined.
The following mechanisms are present in the actual CLI binaries:

| Target | Observed mechanisms |
| --- | --- |
| Linux x86_64 and aarch64 | ELF position-independent executable (ET_DYN), non-executable GNU stack, GNU_RELRO segment and immediate binding (full RELRO) |
| macOS arm64 and x86_64 | Mach-O MH_PIE set; MH_ALLOW_STACK_EXECUTION absent |
| Windows x86_64 | PE DYNAMIC_BASE (ASLR), HIGH_ENTROPY_VA and NX_COMPAT (DEP) |

These are compiler/linker mechanisms that limit exploitation of defects. They
do not establish absence of memory bugs, universal control-flow protection,
stack canaries or complete native-library instrumentation. The non-root
distroless container is a separate least-privilege measure, not the evidence for
these hardening criteria.

To reproduce, obtain the archives and matching attestation bundles from the
[v0.1.16 release](https://github.com/Jeon1691/clipsync/releases/tag/v0.1.16), verify
them using [packaging](packaging.md), extract the executable, then inspect with
LLVM `llvm-readobj --file-headers` and the platform-specific options
`--program-headers --dynamic-table` (ELF), `--macho-version-min` (Mach-O) or
`--coff-load-config` (PE). The relevant flags are in ELF program/dynamic headers,
Mach-O file flags and PE optional-header DLL characteristics. Repeat for a
changed compiler/linker or release configuration before asserting preservation.

Most first-party code uses memory-safe Rust; native clipboard/daemon adapters,
FFI, the OS and dependencies remain relevant exceptions. See [testing](testing.md)
for sanitizer scope. Do not add unsupported compiler flags or remove native
features merely to change a badge score.

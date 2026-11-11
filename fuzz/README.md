# Protocol fuzzing

The `protocol` target exercises untrusted binary chunk frames and JSON control
messages. It checks that decoding never panics and accepted inputs round-trip
without changing their contents.

Install the tooling and run from the repository root:

```sh
rustup toolchain install nightly
cargo install cargo-fuzz --version 0.13.2 --locked
mkdir -p fuzz/corpus/protocol
cargo +nightly fuzz run protocol fuzz/corpus/protocol fuzz/seeds/protocol -- -max_total_time=60 -max_len=1048576
```

The security workflow runs this target on pull requests, pushes, and weekly.
Copy any failing artifact into `seeds/protocol` when adding a regression case.

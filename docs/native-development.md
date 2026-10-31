# Native development

The Rust executable is an unreleased candidate. The root plugin manifest still
runs the Bash implementation; building Rust does not replace an installed plugin.
Windows source support is present but unverified: a named-pipe client with
overlapped I/O and server process checks, and console key input for confirmation.
It has not been compiled or run on Windows, and the plugin does not declare Windows.
It needs only the standard library and kernel32, so the GNU target is sufficient.
Other non-Unix platforms fail explicitly rather than running without protection.

## Build and checks

Rust 1.99.1, rustfmt and Clippy are selected by rust-toolchain.toml. The source-build
baseline is declared separately in Cargo.toml. This versioned project selection
does not require changing a global default toolchain.

```sh
cargo build --locked --release
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 -I -B tests/test_stage_native.py
cargo audit
```

The compatibility tests also require Bash, jq and the POSIX cksum utility. Package
staging tests require Python 3.11 or newer. These are development/reference-test
dependencies, not native runtime dependencies.
Install cargo-audit separately before running `sh scripts/check-rust.sh`; the
script does not install or silently skip missing tools. The workflow will remain
inactive until eligible isolated self-hosted runners are configured. There is no
GitHub-hosted fallback.

See [Rust practices](rust-practices.md) for error, unsafe, MSRV and dependency
review policy. Tests using mock sockets and PTYs do not establish real Herdr
integration or Windows support.

## Isolated candidate package

After a successful release build, Python 3.11 or newer can create a local package:

```sh
python3 scripts/stage-native.py \
  --binary target/release/last-pane-guard \
  --output target/native-candidate
```

The script verifies the executable's version and produces a host-specific
manifest, executable, license and checksum metadata. It neither installs nor
publishes the plugin. Repeating it with identical inputs is a no-op; a differing
existing destination is rejected. Choose a fresh destination for a changed build.

Do not link the candidate into an existing installation to test it. Plugin
registration is user-wide across sessions. Live testing requires a verified
isolated configuration/registry and an owned disposable server. Never point
experimental hooks at a production state directory. Linking does not build.

Public installation helpers and real release assets are separate from this
local staging step. A staged package is not evidence that a GitHub installation
or upgrade works. The root manifest switches only after those paths are tested.

## Runtime safety and compatibility

- One native process handles refresh, event or confirmation; no daemon is added.
- The Unix state directory key is the same POSIX checksum and byte length as the
  Bash version. State version 1, inode strings, recovery claims and directory locks
  remain compatible, including unknown state fields.
- Missing snapshots are retained for pending exit hooks. Only an authoritative
  matching object-not-found response proves disappearance. Transport, parsing,
  identity and permission errors are not absence.
- Claims are saved before create. A lost or invalid creation response is not
  retried automatically, even if that leaves recovery uncertain.
- IPC deadlines use a monotonic clock; loop-guard history uses epoch seconds.
- Confirmation rechecks generation, recorded replacement and additional panes.
  The API does not provide atomic compare-and-close: work added between the final
  list and close requests remains an upstream race limitation.
- State input is bounded at 64 MiB. Oversized or malformed state fails closed
  without replacement. Locks abandoned by forced termination are not stolen.
- Supported termination signals unwind normal hook ownership and terminal state.
  Forced termination cannot guarantee cleanup.

Do not infer performance from the choice of language. Compare release builds on
matched workloads and report observed timings, workload size and limitations.

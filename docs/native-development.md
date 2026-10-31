# Native development

The plugin is one Rust executable, `last-pane-guard`, with `refresh`, `event`
and `confirm` subcommands. Herdr builds it during `plugin install` through the
manifest's `[[build]]` command; `plugin link` does not build, so build a linked
checkout yourself after every change.

The manifest declares only macOS, the platform tested against a real Herdr
server. Windows source support is present but unverified: a named-pipe client
with overlapped I/O and server process checks, and console key input for
confirmation. It has not been compiled or run on Windows. It needs only the
standard library and kernel32, so the GNU target is sufficient. Other non-Unix
platforms fail explicitly rather than running without protection.

## Build and checks

Rust 1.99.1, rustfmt and Clippy are selected by rust-toolchain.toml. The source-build
baseline is declared separately in Cargo.toml. This versioned project selection
does not require changing a global default toolchain.

```sh
cargo build --locked --release
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo audit
```

The state tests also require the POSIX cksum utility, a test-only dependency.
Install cargo-audit separately before running `sh scripts/check-rust.sh`; the
script does not install or silently skip missing tools. The workflow will remain
inactive until eligible isolated self-hosted runners are configured. There is no
GitHub-hosted fallback.

See [Rust practices](rust-practices.md) for error, unsafe, MSRV and dependency
review policy. Tests using mock sockets and PTYs do not establish real Herdr
integration or Windows support.

## Live testing

Plugin registration is user-wide across sessions. Live testing requires a
verified isolated configuration/registry and an owned disposable server. Never
point experimental hooks at a production state directory.

## Runtime safety and compatibility

- One native process handles refresh, event or confirmation; no daemon is added.
- The Unix state directory key is the same POSIX checksum and byte length as
  0.1.0. State version 1, inode strings, recovery claims and directory locks
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

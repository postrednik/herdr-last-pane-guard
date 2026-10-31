# Rust implementation practices

The plugin is implemented in Rust. Supported platforms are those declared in the
manifest after live validation; source support alone is not platform support.

## Design and errors

- Prefer a small synchronous executable, explicit types, owned resources and a
  pure decision function. Keep transport, persistence and recovery decisions separate.
- Use a dedicated `thiserror` error enum in library code. Callers must distinguish
  API rejection, shutdown, stale generation, timeout and malformed data. Do not
  turn transport failures into empty snapshots. `anyhow`, if ever needed, belongs
  only at an application boundary, not in the public library contract.
- Avoid panics, unchecked indexing and unwraps on external data. Test assertions
  may unwrap known fixtures. Validate the wire envelope and method-specific shape.
- Diagnostics must not print raw configuration values, complete server responses,
  inherited environment or private paths. Preserve underlying error sources for
  programmatic use, but do not dump debug error chains from the executable.
- Keep public APIs narrow. Document non-obvious invariants, error conditions and
  platform limitations. Destructors must not panic.

## Unsafe and platform code

- Keep unsafe operations inside platform adapters. Every unsafe block must have
  a `SAFETY` comment explaining valid pointers, buffer lengths, ownership and
  lifetimes as applicable. `unsafe_op_in_unsafe_fn` and Clippy's
  `undocumented_unsafe_blocks` are denied.
- Prefer safe standard-library I/O and RAII handle ownership. FFI is justified
  only where native readiness, terminal or pipe APIs are required.
- Timeouts must cover connect, partial writes and partial reads using a monotonic
  deadline. Cancellation is not proof that a mutation was not delivered.
- Preserve in-flight native I/O storage until completion. Never detach a blocking
  mutation thread and pretend its caller's timeout canceled the operation.

## Formatting, linting and tests

Use the versioned toolchain pinned in rust-toolchain.toml: latest stable as checked
on 2026-10-16, Rust 1.99.1. Cargo.toml declares the same initial source-build
baseline. This new native implementation has no established older-compiler
compatibility requirement; the previously available Rust 1.89 was not a deliberate
user-support target. Distributed executables will not require an end-user compiler.
A lower MSRV would require separate dependency and test coverage, not just a smaller
manifest value. Run:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
cargo audit
```

The final audit command requires `cargo-audit` to be installed separately. A
missing audit executable or unavailable advisory database is a failed or skipped
check, not a successful security review. The check runner must not install tools
implicitly. CI uses preprovisioned tools and the same checks.

Clippy's default lint set is enforced. Selected pedantic numeric-conversion lints
are enabled to catch truncation and sign mistakes at native I/O boundaries. Do not
enable every restriction lint: some are intentionally opinionated or conflicting.
Any local suppression needs a concrete reason. Do not automatically rewrite code
with `clippy --fix` without reviewing the change.

Keep Cargo.lock in version control. Resolve dependencies deliberately, use locked
builds and test the resulting transitive dependency graph on the MSRV. Test pure
fixtures, actual subprocess concurrency and mock socket failures separately from
real Herdr integration. Native Windows tests are not interchangeable with WSL or
cross-compilation. Do not claim a platform passed if it was not run.

## Dependency review

As checked against crates.io on 2026-10-16, the latest non-yanked releases were
serde 1.0.229, serde_json 1.0.154, thiserror 2.0.21, libc 0.2.190 and tempfile
3.27.0. Their declared MSRVs were at most 1.77. These are observations, not a
substitute for a locked build. Recheck versions and advisories before updating.

RustSec's `cargo-audit` checks known vulnerabilities in Cargo.lock. `cargo-deny`
adds license, source and dependency-policy checks if those policies are needed.
Latest observed releases were cargo-audit 0.22.2 (MSRV 1.88) and cargo-deny 0.20.2
(MSRV 1.88). An advisory scan does not establish that code is free of unknown
vulnerabilities, malicious build scripts or application-level defects.

## Official references

- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/checklist.html)
- [Clippy usage](https://doc.rust-lang.org/clippy/usage.html)
- [Rustfmt](https://github.com/rust-lang/rustfmt)
- [Cargo rust-version and MSRV](https://doc.rust-lang.org/cargo/reference/rust-version.html)
- [Explicit unsafe operations](https://doc.rust-lang.org/edition-guide/rust-2024/unsafe-op-in-unsafe-fn.html)
- [thiserror](https://github.com/dtolnay/thiserror)
- [RustSec and audit tools](https://rustsec.org/)
- [crates.io API](https://crates.io/data-access)

Cargo 1.97 and newer also support CARGO_BUILD_WARNINGS=deny. The explicit
`-- -D warnings` Clippy invocation above remains the check script's interface.
The versioned project toolchain does not require changing a global Rust default.

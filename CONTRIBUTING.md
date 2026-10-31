# Contributing

Keep changes small. The plugin is a single native Rust executable. Follow
[Rust practices](docs/rust-practices.md) and
[native development](docs/native-development.md). Use the pinned toolchain,
locked dependencies, rustfmt, Clippy with denied warnings, tests and RustSec audit.

Run before submitting a change:

```sh
sh scripts/check-rust.sh
```

Add fixtures for new decision paths and mocked runtime regressions. Treat
configuration, labels, directories and event payloads as data, never code.
Do not infer closure from an API error other than the documented not-found code.
Preserve session isolation, atomic writes, idempotence, the loop guard and
compatibility with existing version 1 state.

Live tests must use only disposable workspaces you created. Linking enables a
plugin user-wide, so unlink it between unverified iterations. Never restart a
server containing someone else's work. Include exact client/server versions,
commands, expected behavior and actual results in bug reports, without secrets
or private terminal contents.

Changes are licensed under the project's MIT license.

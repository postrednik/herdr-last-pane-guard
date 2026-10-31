# Changelog

All notable changes to this project will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-10-31

### Changed

- The plugin is now a native Rust executable built from source during
  installation. Bash, `jq` and `nc` are no longer runtime requirements.
- Herdr API requests use the local socket directly instead of the Herdr CLI.
- The manifest declares macOS and Linux, the platforms tested with a real Herdr
  server.

### Added

- Direct Unix IPC with monotonic deadlines, bounded responses and endpoint
  generation checks before every request.
- Native decision, configuration, state, socket, subprocess and terminal tests,
  including an upgrade test for state written by 0.1.0.

### Compatibility

- Plugin id, configuration and state version 1 are unchanged; existing state and
  recovery history carry over.
- Windows source code cross-compiles for the GNU target but has never run on
  Windows.

## [0.1.0] - 2026-10-14

### Added

- Workspace and tab restoration after their last shell exits.
- Independent `ask`, `auto` and `off` modes for workspaces and tabs.
- Confirmation popup with keep-on-busy fallback and notifications.
- Last-known labels, directories, list positions and focus restoration.
- Per-session snapshots, atomic state writes and serialized recovery claims.
- Rapid-exit loop protection and intentional-close suppression.
- Bash 3.2 compatibility, fixture tests and mocked runtime checks.

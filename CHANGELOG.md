# Changelog

All notable changes to this project will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- A separate native Rust candidate with direct Unix IPC and compatible recovery state.
- Native decision, configuration, socket, subprocess and terminal regression tests.
- Versioned development toolchain, lint checks, dependency audit and local package staging.

The active plugin manifest still uses the Bash implementation. Native Windows,
public binary installation and production cutover are not included in this candidate.

## [0.1.0] - 2026-10-14

### Added

- Workspace and tab restoration after their last shell exits.
- Independent `ask`, `auto` and `off` modes for workspaces and tabs.
- Confirmation popup with keep-on-busy fallback and notifications.
- Last-known labels, directories, list positions and focus restoration.
- Per-session snapshots, atomic state writes and serialized recovery claims.
- Rapid-exit loop protection and intentional-close suppression.
- Bash 3.2 compatibility, fixture tests and mocked runtime checks.

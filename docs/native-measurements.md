# Initial native measurements

Observed on 2026-10-16 with Herdr 0.9.4, macOS/arm64 and a Rust 1.99.1 release
build. These are development measurements, not a released performance guarantee.

## Refresh latency

Both implementations queried the same isolated, real Herdr server. Each workspace
had one tab and one idle shell. The Bash reference used the real Herdr CLI; the
native implementation used direct socket requests. Each implementation had its own
initially empty state directory and the same configuration. No plugin hooks were
registered during measurement, so they did not compete with the measured refresh.

Each case ran once to initialize state, then eight more times. Timing includes
starting the hook process, RPCs, snapshot serialization and state persistence.
“First” means initial plugin state, not a cold operating-system filesystem cache.

| Workspaces | Implementation | First run (ms) | Warm median (ms) | Warm range (ms) |
| --- | --- | ---: | ---: | ---: |
| 1 | Bash/jq | 127.93 | 129.42 | 124.42–142.06 |
| 1 | Rust | 23.02 | 11.13 | 9.54–11.20 |
| 8 | Bash/jq | 196.04 | 226.01 | 201.27–307.91 |
| 8 | Rust | 20.54 | 13.14 | 11.29–16.76 |

Median reported child-process CPU time (user plus system) was 107.17/3.41 ms for
Bash/Rust with one workspace, and 178.01/3.97 ms with eight. This excludes the
already-running server's CPU consumption. Peak memory and OS-traced process counts
were not measured.

Limitations: small sample, Bash cases precede native cases, no cache flushing,
background host activity is uncontrolled, and no Linux or Windows measurements.
These numbers measure refresh, not end-to-end shell-exit restoration or popup
latency. They compare complete implementations, not Rust versus Go or the intrinsic
speed of programming languages.

## Isolated live smoke coverage

The staged native package also passed these real-server checks on Herdr 0.9.4:

- Workspace restoration after shell exit.
- Immediate replacement-shell exit suppressed by the loop guard.
- Tab restoration inside a surviving workspace with its custom label.
- Intentional workspace API closure left closed.
- Off mode suppressing restoration.
- Headless ask-mode failure retaining the restored workspace.

All 47 captured hook records were succeeded with exit code 0. The owned test
server then stopped with exit code 0 and its socket disappeared. This is not proof
of every hook's behavior during shutdown, because server logs are memory-only.
Real PTY input and terminal restoration are covered separately by subprocess/mock
integration tests; an interactive Herdr client popup acceptance run is still a
separate check. No production installation was switched for these measurements.

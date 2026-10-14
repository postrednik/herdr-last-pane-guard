# Last Pane Guard

A Herdr plugin that reopens a workspace or tab when its last shell exits.

Herdr can remove the last pane, its tab, and its workspace after `exit` or EOF.
Last Pane Guard does **not prevent closure**. It restores the missing container
with its last known label, directory, position, and focus, then either keeps it
or asks whether you want to keep it.

Intentional `workspace close`, `tab close`, and `pane close` API operations are
not restored. Detection does not depend on the order of close events.

## Requirements

- Herdr **0.9.4** or later (0.9.4 is the tested baseline).
- Bash 3.2 or later and `jq` 1.6 or later.
- macOS or Linux with standard POSIX command-line tools.
- Optional `nc` with Unix socket support (`-U`) for restoring list position.
  Without it, the new shell is kept but its position may differ.

macOS 15 and later include `jq`. On Linux, install `bash` and `jq` with your
package manager. There is no build step, background service, or extra runtime.
Windows is not supported: the plugin relies on Bash, Unix sockets and POSIX
filesystem behavior.

## Install

```sh
herdr plugin install postrednik/herdr-last-pane-guard
herdr plugin action invoke postrednik.last-pane-guard.refresh
```

The refresh action initializes the current session immediately. Herdr does not
run startup hooks when a plugin is installed, linked, or enabled. Run the action
in each already-running session that should be protected. Future server starts
and live handoffs initialize through the startup hook automatically.

For local development, from a checkout:

```sh
herdr plugin link "$PWD"
herdr plugin action invoke postrednik.last-pane-guard.refresh
```

Plugins are user-wide. Linking or enabling one affects all running Herdr sessions.
Do not test an unreviewed plugin against workspaces containing important work.

## Configure

Find the configuration directory:

```sh
herdr plugin config-dir postrednik.last-pane-guard
```

Create `config.env` in that directory:

```dotenv
RESTORE_WORKSPACE=ask
RESTORE_TAB=ask
LOOP_GUARD_SECONDS=5
```

| Setting | Values | Default |
| --- | --- | --- |
| `RESTORE_WORKSPACE` | `ask`, `auto`, `off` | `ask` |
| `RESTORE_TAB` | `ask`, `auto`, `off` | `ask` |
| `LOOP_GUARD_SECONDS` | Nonnegative integer, at most 2147483647 | `5` |

- **ask:** restore immediately, then show a popup: “Keep it? [Y/n]”. Enter or `y`
  keeps it. `n` closes the recorded replacement. If another modal is open or the
  popup cannot open, keep the replacement and send a notification instead.
- **auto:** restore immediately and send a notification.
- **off:** do not restore; record the decision in the plugin log.
- The loop guard suppresses another restoration if the replacement shell exits
  in less than the configured number of seconds. `0` disables this protection.

Configuration is read on each hook invocation. Missing files use defaults.
Invalid values log a warning and fall back to that key's default. Use plain
`KEY=VALUE`, not shell assignments: quoting, expansion, commands, and inline
comments are not evaluated. Blank lines, full-line `#` comments, surrounding
whitespace and CRLF line endings are accepted. With duplicate keys, the last
value wins, including falling back to the default if that value is invalid.

## How it works

The plugin caches workspace, tab and pane metadata on startup and structural or
focus events. Each session has its own state directory, identified by the socket
path. Socket generations keep stale state from a previous server from being
reused. Writes are atomic and serialized with a bounded directory lock.

On `pane.exited`, the plugin reads the **previous** snapshot and separately checks
whether the workspace and tab still exist. It restores only the missing container.
API failures are not treated as absence. A persisted claim prevents duplicate
restoration even when hooks run concurrently. Missing metadata is logged and
skipped rather than guessed.

Snapshot refreshes retain missing metadata so an unrelated focus event cannot
race ahead of the exit handler and erase the only recovery information. Tab
positions come from list order, not their stable public numbers.

The confirmation popup is bound to the recorded restoration, not the currently
focused pane. It refuses to close an object if additional panes were added while
the question was pending.

## Limits

- On Herdr 0.9.4, a server without an attached TUI survives the last workspace's
  shell exit, allowing restoration. With a TUI attached, Herdr also creates a
  default workspace when none remain. Recovery can therefore leave that default
  workspace alongside the restored one; the plugin does not remove it.
- Processes, agents, scrollback, environment variables and multi-pane layouts
  are **not** restored. The replacement is a fresh shell.
- The directory is the last observed `foreground_cwd`, falling back to `cwd`.
  A plain `cd` without a later structural or focus event is not captured.
- If that directory no longer exists, the shell opens in Herdr's default
  directory and a warning is logged.
- Deliberate shell `exit` and accidental EOF cannot be distinguished. Use `ask`
  when you want a choice. Preventing the exit itself requires a shell-level guard.
- The plugin cannot reconstruct metadata it never observed. Initialize it after
  installation and after enabling it in an already-running session.
- The saved focus and position reflect the last snapshot. Other user actions
  during recovery can change the surrounding layout.
- A failed or interrupted create request is not retried automatically: it may
  already have created an object, and a retry could duplicate it.
- Runtime history is retained for the current socket generation. It is local
  metadata in Herdr's plugin state directory, not a backup of terminal contents.

## Disable or remove

```sh
herdr plugin disable postrednik.last-pane-guard
# For a linked development checkout:
herdr plugin unlink postrednik.last-pane-guard
# For an installed plugin:
herdr plugin uninstall postrednik.last-pane-guard
```

Configuration and runtime state may remain after unlinking. Inspect them before
removing anything; never delete a lock belonging to a running hook.

## Troubleshooting

```sh
herdr plugin log list --plugin postrednik.last-pane-guard --limit 50
herdr plugin action invoke postrednik.last-pane-guard.refresh
```

- **Missing snapshot:** initialize the session and retry in a disposable workspace.
- **Loop guard:** fix the shell startup failure before retrying; do not disable
  protection just to repeat a failing shell indefinitely.
- **UI busy:** the replacement was kept. Close it normally if unwanted.
- **Position not restored:** verify that your `nc` supports `-U`; recovery still
  provides a shell when reordering is unavailable.
- **Lock timeout:** inspect the session's state directory and `lock/owner` PID.
  Only remove a stale lock after verifying its owning process is no longer running.
- **Invalid state or session collision:** recovery stops rather than overwriting
  unknown data. Disable the plugin, inspect and preserve the affected state, then
  initialize a clean state after resolving the cause.

## Development

```sh
/bin/bash tests/run.sh
shellcheck -x bin/*.sh tests/*.sh
```

Unit and mocked runtime tests need no running Herdr. Live acceptance tests must
use disposable workspaces and explicitly cover shell exit versus intentional
close, confirmation choices, rapid replacement exits, concurrent exits,
configuration `off`, and server shutdown. TUI close shortcuts require a manual
check in the client; injecting keys into a pane does not exercise client bindings.

See [CONTRIBUTING.md](CONTRIBUTING.md) and [CHANGELOG.md](CHANGELOG.md).

## License

[MIT](LICENSE).

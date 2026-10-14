# Contributing

Keep changes small and preserve Bash 3.2 compatibility. The runtime depends on
`jq` and standard Unix tools; there is no build step.

Run before submitting a change:

```sh
/bin/bash tests/run.sh
shellcheck -x bin/*.sh tests/*.sh
```

Add fixtures for new decision paths and mocked runtime regressions. Treat
configuration, labels, directories and event payloads as data, never shell code.
Do not infer closure from an API error other than the documented not-found code.
Preserve session isolation, atomic writes, idempotence and the loop guard.

Live tests must use only disposable workspaces you created. Linking enables a
plugin user-wide, so unlink it between unverified iterations. Never restart a
server containing someone else's work. Include exact client/server versions,
commands, expected behavior and actual results in bug reports, without secrets
or private terminal contents.

Changes are licensed under the project's MIT license.

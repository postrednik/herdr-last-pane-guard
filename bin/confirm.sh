#!/bin/bash
# lpg_state passes literal jq expressions, not interpolated shell strings.
# shellcheck disable=SC2016
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
# shellcheck source=bin/common.sh
. "$ROOT/bin/common.sh"

load_recovery() {
    [ -f "$STATE_FILE" ] || return 1
    jq -e --arg socket "$HERDR_SOCKET_PATH" --arg generation "$GENERATION" \
        --arg token "$LPG_RECOVERY_TOKEN" '
        select(.socket == $socket and .generation == $generation) |
        .handled[$token] | select(.status == "restored" and .confirmed == false)' \
        "$STATE_FILE" > "$RUN_DIR/recovery"
}

main() {
    local answer kind label id ws pane
    lpg_init || return $?
    if [ -z "${LPG_RECOVERY_TOKEN:-}" ] || [ "${LPG_SESSION_KEY:-}" != "$SESSION_KEY" ]; then
        lpg_log 'confirmation has no valid recovery context'; return 1
    fi
    lpg_lock || return $?
    if ! load_recovery; then return 0; fi
    lpg_unlock || return 1
    kind=$(jq -r '.kind' "$RUN_DIR/recovery")
    label=$(jq -r '.label | gsub("[\u0000-\u001f\u007f]"; "?")' "$RUN_DIR/recovery")
    printf '\n%s %s was closed by a shell exit and has been restored.\n' "$kind" "$label"
    while :; do
        printf 'Keep it? [Y/n] '
        answer=
        if ! IFS= read -r -n 1 answer; then answer=; fi
        printf '\n'
        case "$answer" in y|Y|'') answer=keep; break ;; n|N) answer=close; break ;; esac
    done
    lpg_lock || return $?
    # A popup may outlive its restored object or even a server handoff.
    if [ "$(lpg_generation)" != "$GENERATION" ]; then
        lpg_log 'server generation changed; stale confirmation ignored'
        return 0
    fi
    if ! load_recovery; then return 0; fi
    id=$(jq -r '.id' "$RUN_DIR/recovery")
    ws=$(jq -r '.workspace_id' "$RUN_DIR/recovery")
    pane=$(jq -r '.pane_id' "$RUN_DIR/recovery")
    if [ "$answer" = close ]; then
        # Never discard work added while the confirmation popup was pending.
        lpg_cli "$RUN_DIR/panes" pane list || return $?
        if ! jq -e --arg kind "$kind" --arg id "$id" --arg ws "$ws" --arg pane "$pane" '
            [.result.panes[] | select(if $kind == "workspace" then .workspace_id == $ws
                else .tab_id == $id end)] | length == 1 and .[0].pane_id == $pane' \
            "$RUN_DIR/panes" >/dev/null; then
            lpg_log 'restored object changed or disappeared; confirmation will not close it'
            answer=keep
        fi
    fi
    lpg_state --arg token "$LPG_RECOVERY_TOKEN" '.handled[$token].confirmed = true' || return 1
    if [ "$answer" = close ]; then
        case "$kind" in workspace|tab) ;; *) return 1 ;; esac
        lpg_cli "$RUN_DIR/closed" "$kind" close "$id" || return $?
        lpg_log 'restored object closed by confirmation'
    fi
}

if main; then exit 0; else
    result=$?
    if [ "$result" -eq 3 ]; then exit 0; fi
    exit "$result"
fi

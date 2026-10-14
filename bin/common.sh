#!/bin/bash
# Shared runtime helpers. Config files are data, never shell input.

lpg_log() {
    printf 'Last Pane Guard: %s\n' "$*" >&2
}

lpg_cleanup() {
    if [ -n "${STATE_TMP:-}" ]; then rm -f -- "$STATE_TMP"; fi
    if [ -n "${RUN_DIR:-}" ]; then
        rm -f -- "$RUN_DIR"/*
        rmdir -- "$RUN_DIR"
    fi
    if [ "${LOCKED:-0}" = 1 ]; then
        rm -f -- "$SESSION_DIR/lock/owner"
        rmdir -- "$SESSION_DIR/lock"
    fi
}

lpg_generation() {
    # Either implementation may be first in PATH, including GNU tools on macOS.
    if stat -c '%i' "$HERDR_SOCKET_PATH" 2>/dev/null; then return 0; fi
    if stat -f '%i' "$HERDR_SOCKET_PATH" 2>/dev/null; then return 0; fi
    [ -e "$HERDR_SOCKET_PATH" ] || return 3
    lpg_log 'cannot identify socket generation'
    return 1
}

lpg_init() {
    : "${HERDR_BIN_PATH:?Herdr binary path is required}"
    : "${HERDR_SOCKET_PATH:?Herdr socket path is required}"
    : "${HERDR_PLUGIN_STATE_DIR:?Herdr plugin state directory is required}"
    : "${HERDR_PLUGIN_CONFIG_DIR:?Herdr plugin config directory is required}"
    command -v jq >/dev/null || { lpg_log 'jq is required'; return 1; }
    [ -x "$HERDR_BIN_PATH" ] || { lpg_log 'Herdr binary is not executable'; return 1; }
    # A stopped server is normal, including during session shutdown.
    [ -e "$HERDR_SOCKET_PATH" ] || return 3
    umask 077
    local checksum bytes
    read -r checksum bytes < <(printf '%s' "$HERDR_SOCKET_PATH" | cksum)
    SESSION_KEY="$checksum-$bytes"
    SESSION_DIR="$HERDR_PLUGIN_STATE_DIR/$SESSION_KEY"
    mkdir -p -- "$SESSION_DIR" || return 1
    STATE_FILE="$SESSION_DIR/state.json"
    GENERATION=$(lpg_generation) || return $?
    RUN_DIR=$(mktemp -d "$SESSION_DIR/.run.XXXXXX") || return 1
    LOCKED=0
    STATE_TMP=
    trap lpg_cleanup EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM HUP
}

lpg_lock() {
    local attempt=0
    while ! mkdir -- "$SESSION_DIR/lock" 2>/dev/null; do
        attempt=$((attempt + 1))
        if [ "$attempt" -ge 400 ]; then
            lpg_log 'state lock timed out; no restoration attempted'
            return 1
        fi
        [ -e "$HERDR_SOCKET_PATH" ] || return 3
        sleep 0.05
    done
    LOCKED=1
    printf '%s\n' "$$" > "$SESSION_DIR/lock/owner"
}

lpg_unlock() {
    rm -f -- "$SESSION_DIR/lock/owner" && rmdir -- "$SESSION_DIR/lock" || return 1
    LOCKED=0
}

# All state writes run while holding the session lock, on the same filesystem.
lpg_state() {
    STATE_TMP=$(mktemp "$SESSION_DIR/.state.XXXXXX") || return 1
    if jq "$@" "$STATE_FILE" > "$STATE_TMP" && mv -f -- "$STATE_TMP" "$STATE_FILE"; then
        STATE_TMP=
    else
        return 1
    fi
}

lpg_prepare_state() {
    if [ -e "$STATE_FILE" ]; then
        jq -e 'type == "object" and .version == 1 and (.panes | type == "object")
            and (.workspaces | type == "object") and (.tabs | type == "object")
            and (.handled | type == "object") and (.restored | type == "object")
            and (.closed | type == "object")' "$STATE_FILE" >/dev/null || {
            lpg_log 'invalid state file; refusing to overwrite it'; return 1;
        }
        # cksum is portable; a collision must never mix different sessions.
        jq -e --arg socket "$HERDR_SOCKET_PATH" '.socket == $socket' "$STATE_FILE" >/dev/null || {
            lpg_log 'session key collision; refusing to use another session state'; return 1;
        }
        if jq -e --arg generation "$GENERATION" '.generation == $generation' "$STATE_FILE" >/dev/null; then
            return 0
        fi
    fi
    STATE_TMP=$(mktemp "$SESSION_DIR/.state.XXXXXX") || return 1
    jq -n --arg socket "$HERDR_SOCKET_PATH" --arg generation "$GENERATION" \
        '{version:1,socket:$socket,generation:$generation,workspaces:{},tabs:{},panes:{},
          handled:{},claims:{},restored:{},closed:{workspaces:{},tabs:{},panes:{}}}' > "$STATE_TMP" &&
        mv -f -- "$STATE_TMP" "$STATE_FILE" || return 1
    STATE_TMP=
}

# Never treat transport/permission/parse failures as proof that an object vanished.
lpg_cli() {
    local output=$1 rc
    shift
    if "$HERDR_BIN_PATH" "$@" > "$output" 2> "$RUN_DIR/cli-error"; then
        jq -e 'has("result") and (has("error") | not)' "$output" >/dev/null || {
            lpg_log 'invalid Herdr API response'; return 1;
        }
    else
        rc=$?
        if [ ! -e "$HERDR_SOCKET_PATH" ] ||
            jq -e '.error.code == "server_not_running"' "$RUN_DIR/cli-error" >/dev/null 2>&1; then
            return 3
        fi
        lpg_log "Herdr command failed (exit $rc); state not assumed missing"
        return 1
    fi
}

# nc is optional. A failed reorder must not discard a successfully restored shell.
lpg_move() {
    local kind=$1 id=$2 index=$3 request response
    command -v nc >/dev/null || { lpg_log 'nc unavailable; position not restored'; return 0; }
    request=$(jq -cn --arg kind "$kind" --arg id "$id" --argjson index "$index" \
        '{id:"last-pane-guard:move",method:($kind+".move"),
          params:{($kind+"_id"):$id,insert_index:$index}}') || return 1
    if response=$(printf '%s\n' "$request" | nc -U -w 1 "$HERDR_SOCKET_PATH" 2> "$RUN_DIR/nc-error") &&
        printf '%s\n' "$response" | jq -e 'has("result") and (has("error") | not)' >/dev/null 2>&1; then
        return 0
    fi
    lpg_log 'socket reorder unavailable or rejected; restored shell kept at its new position'
}

lpg_notify() {
    local title=$1 body=$2
    if ! "$HERDR_BIN_PATH" notification show "$title" --body "$body" > "$RUN_DIR/notification" 2>&1; then
        [ -e "$HERDR_SOCKET_PATH" ] && lpg_log 'notification could not be delivered'
    fi
    return 0
}

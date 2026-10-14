#!/bin/bash
# lpg_state passes literal jq expressions, not interpolated shell strings.
# shellcheck disable=SC2016
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
# shellcheck source=bin/common.sh
. "$ROOT/bin/common.sh"
# shellcheck source=bin/config.sh
. "$ROOT/bin/config.sh"

collect_live() {
    local ws rc
    lpg_cli "$RUN_DIR/workspaces" workspace list || return $?
    lpg_cli "$RUN_DIR/panes" pane list || return $?
    jq -e '.result.workspaces | type == "array"' "$RUN_DIR/workspaces" >/dev/null || return 1
    jq -e '.result.panes | type == "array"' "$RUN_DIR/panes" >/dev/null || return 1
    : > "$RUN_DIR/tabs-lines"
    while IFS= read -r ws; do
        if "$HERDR_BIN_PATH" tab list --workspace "$ws" > "$RUN_DIR/tabs-one" 2> "$RUN_DIR/cli-error"; then
            jq -ce '.result.tabs | select(type == "array") | to_entries[] |
                .value + {index:.key}' "$RUN_DIR/tabs-one" >> "$RUN_DIR/tabs-lines" || {
                # An empty tab array is legitimate during removal.
                jq -e '.result.tabs == []' "$RUN_DIR/tabs-one" >/dev/null || return 1
            }
        else
            rc=$?
            if jq -e '.error.code == "workspace_not_found"' "$RUN_DIR/cli-error" >/dev/null 2>&1; then
                continue
            fi
            [ -e "$HERDR_SOCKET_PATH" ] || return 3
            lpg_log "tab snapshot failed (exit $rc)"; return 1
        fi
    done < <(jq -r '.result.workspaces[].workspace_id' "$RUN_DIR/workspaces")
    jq -s '.' "$RUN_DIR/tabs-lines" > "$RUN_DIR/tabs"
    jq -n --slurpfile ws "$RUN_DIR/workspaces" --slurpfile tabs "$RUN_DIR/tabs" \
        --slurpfile panes "$RUN_DIR/panes" \
        '{workspaces:($ws[0].result.workspaces | to_entries | map(.value + {index:.key})),
          tabs:$tabs[0],panes:$panes[0].result.panes}' > "$RUN_DIR/live"
}

merge_live() {
    # Missing entities stay cached until their exit hook consumes them. A concurrent
    # focus/structure hook must not erase the only copy of a vanished object's data.
    lpg_state --slurpfile live "$RUN_DIR/live" '
        def keyed($field): map({key:.[$field],value:.}) | from_entries;
        .workspaces += ($live[0].workspaces | keyed("workspace_id")) |
        .tabs += ($live[0].tabs | keyed("tab_id")) |
        . as $state |
        .panes += ($live[0].panes | map(. + {
            workspace_snapshot:$state.workspaces[.workspace_id],
            tab_snapshot:$state.tabs[.tab_id]}) | keyed("pane_id"))'
}

lookup() {
    local kind=$1 id=$2
    if "$HERDR_BIN_PATH" "$kind" get "$id" > "$RUN_DIR/get-$kind" 2> "$RUN_DIR/cli-error"; then
        jq -e --arg kind "$kind" '.result[$kind] | type == "object"' "$RUN_DIR/get-$kind" >/dev/null || return 1
    else
        if jq -e --arg code "${kind}_not_found" '.error.code == $code' "$RUN_DIR/cli-error" >/dev/null 2>&1; then
            printf '{"result":{}}\n' > "$RUN_DIR/get-$kind"
        elif [ ! -e "$HERDR_SOCKET_PATH" ] ||
            jq -e '.error.code == "server_not_running"' "$RUN_DIR/cli-error" >/dev/null 2>&1; then
            return 3
        else
            lpg_log 'existence lookup failed; no restoration attempted'; return 1
        fi
    fi
}

mark_closed() {
    local kind=$1 id=$2
    [ -n "$id" ] || return 1
    lpg_state --arg kind "$kind" --arg id "$id" '.closed[$kind][$id] = true'
}

restore() {
    local kind label cwd index focus ws tab pane new_id now token mode tab_label number
    kind=$(jq -r '.kind' "$RUN_DIR/plan")
    label=$(jq -r '.label' "$RUN_DIR/plan")
    cwd=$(jq -r '.cwd // empty' "$RUN_DIR/plan")
    index=$(jq -r '.index' "$RUN_DIR/plan")
    focus=$(jq -r '.focused' "$RUN_DIR/plan")
    mode=$(jq -r '.mode' "$RUN_DIR/plan")
    ws=$(jq -r '.workspace_id' "$RUN_DIR/plan")
    local args=("$kind" create --label "$label" --no-focus)
    if [ "$focus" = true ]; then args=("$kind" create --label "$label" --focus); fi
    if [ -n "$cwd" ] && [ -d "$cwd" ]; then
        args+=(--cwd "$cwd")
    else
        lpg_log 'last known directory is unavailable; using the default shell directory'
    fi
    if [ "$kind" = tab ]; then args+=(--workspace "$ws"); fi
    # Claim the entity too: different exited panes can belong to the same vanished container.
    # An ambiguous command failure must not create a duplicate on later delivery.
    lpg_state --arg pane "$EXIT_PANE" --slurpfile plan "$RUN_DIR/plan" '
        .handled[$pane] = {status:"claimed"} |
        .claims[$plan[0].kind + ":" + (if $plan[0].kind == "workspace"
            then $plan[0].workspace_id else $plan[0].tab_id end)] = true' || return 1
    lpg_cli "$RUN_DIR/created" "${args[@]}" || return $?
    pane=$(jq -er '.result.root_pane.pane_id' "$RUN_DIR/created") || return 1
    tab=$(jq -er '.result.tab.tab_id' "$RUN_DIR/created") || return 1
    if [ "$kind" = workspace ]; then
        ws=$(jq -er '.result.workspace.workspace_id' "$RUN_DIR/created") || return 1
        new_id=$ws
    else
        new_id=$tab
    fi
    now=$(date +%s)
    token=$EXIT_PANE
    # Cache creation immediately: the replacement shell can exit before any refresh.
    lpg_state --slurpfile created "$RUN_DIR/created" --slurpfile plan "$RUN_DIR/plan" \
        --arg pane "$pane" --arg tab "$tab" --arg ws "$ws" --arg old "$EXIT_PANE" \
        --arg kind "$kind" --arg id "$new_id" --argjson now "$now" --argjson index "$index" '
        .panes[$pane] = $created[0].result.root_pane |
        .tabs[$tab] = ($created[0].result.tab + {index:(if $kind == "tab" then $index else 0 end)}) |
        (if $kind == "workspace" then
            .workspaces[$ws] = ($created[0].result.workspace + {index:$index}) else . end) |
        .restored[$pane] = $now |
        .handled[$old] = {status:"restored",kind:$kind,id:$id,pane_id:$pane,
            workspace_id:$ws,tab_id:$tab,label:$plan[0].label,confirmed:false}' || return 1
    lpg_move "$kind" "$new_id" "$index"
    if [ "$kind" = workspace ]; then
        tab_label=$(jq -r '.tab_label' "$RUN_DIR/plan")
        number=$(jq -r --arg id "$(jq -r '.tab_id' "$RUN_DIR/plan")" '.tabs[$id].number' "$STATE_FILE")
        if [ -n "$tab_label" ] && [ "$tab_label" != "$number" ]; then
            if ! lpg_cli "$RUN_DIR/renamed" tab rename "$tab" "$tab_label"; then
                lpg_log 'restored workspace kept, but its tab label could not be restored'
            fi
        fi
    fi
    lpg_log "$kind restored after shell exit"
    if [ "$mode" = auto ]; then
        lpg_notify "$kind $label restored" 'closed by shell exit'
    else
        # Custom env binds the popup to this restoration, never to UI focus.
        if ! "$HERDR_BIN_PATH" plugin pane open --plugin postrednik.last-pane-guard \
            --entrypoint confirm --env "LPG_RECOVERY_TOKEN=$token" \
            --env "LPG_SESSION_KEY=$SESSION_KEY" > "$RUN_DIR/popup" 2> "$RUN_DIR/cli-error"; then
            if jq -e '.error.code == "ui_busy"' "$RUN_DIR/cli-error" >/dev/null 2>&1; then
                lpg_log 'confirmation UI busy; restored shell kept'
            else
                lpg_log 'confirmation popup unavailable; restored shell kept'
            fi
            lpg_notify "$kind $label restored" 'closed by shell exit; confirmation unavailable, kept'
        fi
    fi
}

handle_exit() {
    local ws tab now attempt action reason
    EXIT_PANE=$(printf '%s' "$HERDR_PLUGIN_EVENT_JSON" | jq -er '.data.pane_id | select(type == "string" and length > 0)') || return 1
    if ! jq -e --arg pane "$EXIT_PANE" '.panes | has($pane)' "$STATE_FILE" >/dev/null; then
        lpg_log 'exited pane missing from snapshot; skipped'
        return 0
    fi
    ws=$(jq -r --arg pane "$EXIT_PANE" '.panes[$pane].workspace_id' "$STATE_FILE")
    tab=$(jq -r --arg pane "$EXIT_PANE" '.panes[$pane].tab_id' "$STATE_FILE")
    # Do not refresh the shadow snapshot here. Probe live existence separately.
    for attempt in 1 2 3 4 5; do
        lookup pane "$EXIT_PANE" || return $?
        if jq -e '.result.pane == null' "$RUN_DIR/get-pane" >/dev/null; then break; fi
        sleep 0.1
    done
    lookup workspace "$ws" || return $?
    lookup tab "$tab" || return $?
    now=$(date +%s)
    jq -n --arg pane "$EXIT_PANE" --argjson now "$now" --slurpfile state "$STATE_FILE" \
        --slurpfile ws "$RUN_DIR/get-workspace" --slurpfile tab "$RUN_DIR/get-tab" \
        --slurpfile pane_live "$RUN_DIR/get-pane" --arg workspace_mode "$RESTORE_WORKSPACE" \
        --arg tab_mode "$RESTORE_TAB" --argjson seconds "$LOOP_GUARD_SECONDS" '
        {pane_id:$pane,now:$now,state:$state[0],
         config:{restore_workspace:$workspace_mode,restore_tab:$tab_mode,loop_guard_seconds:$seconds},
         live:{workspaces:[$ws[0].result.workspace // empty],tabs:[$tab[0].result.tab // empty],
               panes:[$pane_live[0].result.pane // empty]}}' > "$RUN_DIR/input" || return 1
    jq -f "$ROOT/bin/plan.jq" "$RUN_DIR/input" > "$RUN_DIR/plan" || return 1
    action=$(jq -er '.action' "$RUN_DIR/plan")
    reason=$(jq -er '.reason' "$RUN_DIR/plan")
    case "$action" in
        restore) restore || return $? ;;
        loop_guard)
            lpg_state --arg pane "$EXIT_PANE" '.handled[$pane] = {status:"loop_guard"}' || return 1
            lpg_log 'rapid replacement shell exit; loop guard prevented another restore'
            lpg_notify 'Last Pane Guard stopped a restore loop' 'The replacement shell exited too quickly.' ;;
        skip)
            lpg_log "exit skipped: $reason"
            if [ "$reason" != already_handled ]; then
                lpg_state --arg pane "$EXIT_PANE" --arg reason "$reason" \
                    '.handled[$pane] = {status:$reason}' || return 1
            fi ;;
        *) lpg_log 'invalid restoration plan'; return 1 ;;
    esac
}

main() {
    local mode=${1:-event} event id
    lpg_init || return $?
    lpg_load_config "$HERDR_PLUGIN_CONFIG_DIR/config.env" || return $?
    lpg_lock || return $?
    lpg_prepare_state || return $?
    case "$mode" in
        refresh) collect_live && merge_live; return $? ;;
        event) : "${HERDR_PLUGIN_EVENT:?Event name is required}" ;;
        *) lpg_log 'usage: guard.sh refresh|event'; return 1 ;;
    esac
    event=$HERDR_PLUGIN_EVENT
    case "$event" in
        pane.exited) handle_exit; return $? ;;
        workspace.closed|tab.closed|pane.closed)
            id=$(printf '%s' "${HERDR_PLUGIN_EVENT_JSON:-}" | jq -er --arg field "${event%%.*}_id" \
                '.data[$field] | select(type == "string" and length > 0)') || return 1
            case "$event" in
                workspace.closed) mark_closed workspaces "$id" || return $? ;;
                tab.closed) mark_closed tabs "$id" || return $? ;;
                pane.closed) mark_closed panes "$id" || return $? ;;
            esac ;;
        workspace.created|workspace.renamed|workspace.moved|workspace.reordered|workspace.focused|\
        tab.created|tab.renamed|tab.moved|tab.focused|pane.created|pane.focused|pane.moved) ;;
        *) lpg_log 'unsupported event ignored'; return 0 ;;
    esac
    collect_live && merge_live
}

if main "$@"; then exit 0; else
    result=$?
    # Shutdown is not a plugin failure and must not create replacement workspaces.
    if [ "$result" -eq 3 ]; then exit 0; fi
    exit "$result"
fi

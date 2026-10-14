#!/bin/bash
# save passes literal jq expressions to jq.
# shellcheck disable=SC2016
set -euo pipefail
: "${MOCK_STATE:?}"
: "${MOCK_CALLS:?}"
printf '%s\n' "$*" >> "$MOCK_CALLS"
kind=${1:-}
verb=${2:-}
shift 2
if [ "${MOCK_FAIL:-}" = "$kind.$verb" ]; then
    printf '{"error":{"code":"permission_denied"}}\n' >&2
    exit 1
fi
save() {
    jq "$@" "$MOCK_STATE" > "$MOCK_STATE.tmp"
    mv "$MOCK_STATE.tmp" "$MOCK_STATE"
}
missing() {
    printf '{"error":{"code":"%s_not_found"}}\n' "$kind" >&2
    exit 1
}
case "$kind.$verb" in
    workspace.list) jq '{result:{workspaces:.workspaces}}' "$MOCK_STATE" ;;
    pane.list) jq '{result:{panes:.panes}}' "$MOCK_STATE" ;;
    tab.list)
        jq --arg ws "${2:-}" '{result:{tabs:[.tabs[] | select(.workspace_id == $ws)]}}' "$MOCK_STATE" ;;
    workspace.get|tab.get|pane.get)
        item=$(jq -c --arg kind "$kind" --arg id "$1" '.[$kind+"s"][] | select(.[$kind+"_id"] == $id)' "$MOCK_STATE")
        [ -n "$item" ] || missing
        jq -n --arg kind "$kind" --argjson item "$item" '{result:{($kind):$item}}' ;;
    workspace.create|tab.create)
        label='' cwd='' focused=false ws=''
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --label) label=$2; shift 2 ;;
                --cwd) cwd=$2; shift 2 ;;
                --workspace) ws=$2; shift 2 ;;
                --focus) focused=true; shift ;;
                --no-focus) focused=false; shift ;;
                *) exit 2 ;;
            esac
        done
        count=$(jq '.counter + 1' "$MOCK_STATE")
        if [ "$kind" = workspace ]; then ws="mock-w$count"; fi
        tab="mock-t$count" pane="mock-p$count"
        created=$(jq -n --arg kind "$kind" --arg label "$label" --arg cwd "$cwd" \
            --arg ws "$ws" --arg tab "$tab" --arg pane "$pane" --argjson focus "$focused" '
            {result:{workspace:{workspace_id:$ws,label:$label,focused:$focus,number:1,
                        active_tab_id:$tab,pane_count:1,tab_count:1},
                     tab:{tab_id:$tab,workspace_id:$ws,label:(if $kind == "tab" then $label else "1" end),
                          number:1,focused:$focus,pane_count:1},
                     root_pane:{pane_id:$pane,tab_id:$tab,workspace_id:$ws,cwd:$cwd,foreground_cwd:$cwd,focused:$focus}}}')
        save --arg kind "$kind" --argjson created "$created" --argjson count "$count" '
            .counter = $count |
            (if $kind == "workspace" then .workspaces += [$created.result.workspace] else . end) |
            .tabs += [$created.result.tab] | .panes += [$created.result.root_pane]'
        printf '%s\n' "$created" ;;
    workspace.close|tab.close)
        id=$1
        save --arg kind "$kind" --arg id "$id" '
            if $kind == "workspace" then
                .workspaces |= map(select(.workspace_id != $id)) |
                .tabs |= map(select(.workspace_id != $id)) |
                .panes |= map(select(.workspace_id != $id))
            else .tabs |= map(select(.tab_id != $id)) | .panes |= map(select(.tab_id != $id)) end'
        printf '{"result":{"type":"ok"}}\n' ;;
    tab.rename)
        save --arg id "$1" --arg label "$2" '.tabs |= map(if .tab_id == $id then .label=$label else . end)'
        printf '{"result":{"type":"ok"}}\n' ;;
    notification.show) printf '{"result":{"type":"ok"}}\n' ;;
    plugin.pane)
        if [ "${MOCK_POPUP:-busy}" = busy ]; then
            printf '{"error":{"code":"ui_busy"}}\n' >&2; exit 1
        fi
        printf '{"result":{"type":"ok"}}\n' ;;
    *) printf 'Unsupported mock command\n' >&2; exit 2 ;;
esac

#!/bin/bash
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf -- "$TMP"' EXIT
export HERDR_BIN_PATH="$ROOT/tests/mock-herdr.sh"
export HERDR_PLUGIN_CONFIG_DIR="$TMP/config"
export HERDR_PLUGIN_STATE_DIR="$TMP/state"
export HERDR_SOCKET_PATH="$TMP/socket"
export MOCK_STATE="$TMP/mock.json" MOCK_CALLS="$TMP/calls"
mkdir -p "$HERDR_PLUGIN_CONFIG_DIR" "$TMP/bin"
# A regular file supplies a stable test generation; no real socket is contacted.
touch "$HERDR_SOCKET_PATH"
printf '#!/bin/bash\nwhile IFS= read -r line; do :; done\nprintf '\''{"result":{"type":"ok"}}\\n'\''\n' > "$TMP/bin/nc"
chmod +x "$TMP/bin/nc"
export PATH="$TMP/bin:$PATH"
passed=0

reset_case() {
    rm -rf -- "$HERDR_PLUGIN_STATE_DIR"
    : > "$MOCK_CALLS"
    printf 'RESTORE_WORKSPACE=auto\nRESTORE_TAB=auto\n' > "$HERDR_PLUGIN_CONFIG_DIR/config.env"
    jq -n --arg cwd "$TMP" '{counter:0,
        workspaces:[{workspace_id:"w1",label:"Example",focused:true,active_tab_id:"t1",number:1}],
        tabs:[{tab_id:"t1",workspace_id:"w1",label:"custom",number:7,focused:true}],
        panes:[{pane_id:"p1",workspace_id:"w1",tab_id:"t1",cwd:$cwd,foreground_cwd:$cwd}]}' > "$MOCK_STATE"
    unset MOCK_FAIL MOCK_POPUP
    /bin/bash "$ROOT/bin/guard.sh" refresh > "$TMP/out" 2> "$TMP/err"
    STATE_FILE=$(find "$HERDR_PLUGIN_STATE_DIR" -name state.json -type f)
}

mutate() {
    jq "$@" "$MOCK_STATE" > "$TMP/new.json"
    mv "$TMP/new.json" "$MOCK_STATE"
}

vanish_workspace() { mutate '.workspaces=[] | .tabs=[] | .panes=[]'; }

fire() {
    local event=$1 id=$2
    HERDR_PLUGIN_EVENT="$event" HERDR_PLUGIN_EVENT_JSON="$(jq -cn --arg id "$id" --arg field "${event%%.*}_id" \
        '{data:{($field):$id}}')" /bin/bash "$ROOT/bin/guard.sh" event > "$TMP/out" 2> "$TMP/err"
}

check() {
    local description=$1 expression=$2 file=${3:-$MOCK_STATE}
    if ! jq -e "$expression" "$file" >/dev/null; then
        printf 'FAIL: %s\n' "$description" >&2
        exit 1
    fi
    passed=$((passed + 1))
}

reset_case
vanish_workspace
# Simulate an unrelated refresh winning the lock before the exit hook.
/bin/bash "$ROOT/bin/guard.sh" refresh
fire pane.exited p1
check 'workspace restored after intervening refresh' '.workspaces | length == 1'
check 'tab label preserved' '.tabs[0].label == "custom"'
check 'position captured from array, not public number' '.tabs.t1.index == 0' "$STATE_FILE"
fire pane.exited p1
check 'duplicate exit cannot create twice' '.counter == 1'

reset_case
vanish_workspace
fire pane.exited p1 & first=$!
fire pane.exited p1 & second=$!
wait "$first"
wait "$second"
check 'concurrent duplicate hooks are serialized' '.counter == 1'

reset_case
mutate '.panes += [(.panes[0] + {pane_id:"p2"})]'
/bin/bash "$ROOT/bin/guard.sh" refresh
vanish_workspace
fire pane.exited p1
fire pane.exited p2
check 'distinct exited panes restore the same workspace only once' '.counter == 1'

reset_case
mutate '.panes += [(.panes[0] + {pane_id:"p2"})]'
/bin/bash "$ROOT/bin/guard.sh" refresh
mutate '.tabs=[] | .panes=[]'
fire pane.exited p1
fire pane.exited p2
check 'distinct exited panes restore the same tab only once' '.counter == 1'

reset_case
mutate '.tabs=[] | .panes=[]'
fire pane.exited p1
check 'tab restored without another workspace' '(.workspaces | length) == 1 and .counter == 1 and .tabs[0].label == "custom"'

reset_case
mutate '.tabs=[] | .panes=[] | .workspaces[0].active_tab_id="other-tab"'
/bin/bash "$ROOT/bin/guard.sh" refresh
fire pane.exited p1
check 'intervening refresh preserves the exited tab focus' '.tabs[0].focused == true'

reset_case
vanish_workspace
fire workspace.closed w1
fire pane.closed p1
fire pane.exited p1
check 'intentional closes suppress restoration' '.counter == 0'

reset_case
printf 'RESTORE_WORKSPACE=off\nRESTORE_TAB=off\n' > "$HERDR_PLUGIN_CONFIG_DIR/config.env"
vanish_workspace
fire pane.exited p1
check 'off does not restore' '.counter == 0'

reset_case
vanish_workspace
export MOCK_FAIL=workspace.get
if fire pane.exited p1; then printf 'FAIL: API failure must propagate\n' >&2; exit 1; fi
check 'API errors are not absence' '.counter == 0'
unset MOCK_FAIL

reset_case
vanish_workspace
fire pane.exited p1
new_pane=$(jq -r '.panes[0].pane_id' "$MOCK_STATE")
vanish_workspace
fire pane.exited "$new_pane"
check 'loop guard prevents immediate second restore' '.counter == 1 and (.workspaces | length) == 0'

reset_case
printf 'RESTORE_WORKSPACE=ask\n' > "$HERDR_PLUGIN_CONFIG_DIR/config.env"
vanish_workspace
fire pane.exited p1
check 'busy popup keeps restored workspace' '.counter == 1 and (.workspaces | length) == 1'
grep -q 'confirmation UI busy' "$TMP/err"
passed=$((passed + 1))

reset_case
export MOCK_POPUP=ready
printf 'RESTORE_WORKSPACE=ask\n' > "$HERDR_PLUGIN_CONFIG_DIR/config.env"
vanish_workspace
fire pane.exited p1
export LPG_RECOVERY_TOKEN=p1 LPG_SESSION_KEY
LPG_SESSION_KEY=$(basename "$(dirname "$STATE_FILE")")
printf 'n' | /bin/bash "$ROOT/bin/confirm.sh" > "$TMP/prompt" 2> "$TMP/confirm-err"
check 'confirmation n closes the recorded workspace' '.workspaces | length == 0'
check 'confirmation is consumed once' '.handled.p1.confirmed == true' "$STATE_FILE"
printf 'n' | /bin/bash "$ROOT/bin/confirm.sh" > "$TMP/prompt" 2> "$TMP/confirm-err"
check 'replayed confirmation cannot create or close another object' '.counter == 1'

reset_case
export MOCK_POPUP=ready
printf 'RESTORE_WORKSPACE=ask\n' > "$HERDR_PLUGIN_CONFIG_DIR/config.env"
vanish_workspace
fire pane.exited p1
LPG_SESSION_KEY=$(basename "$(dirname "$STATE_FILE")")
mutate '.panes += [(.panes[0] + {pane_id:"additional"})]'
printf 'n' | /bin/bash "$ROOT/bin/confirm.sh" > "$TMP/prompt" 2> "$TMP/confirm-err"
check 'confirmation does not discard newly added work' '.workspaces | length == 1'

reset_case
export MOCK_POPUP=ready
printf 'RESTORE_WORKSPACE=ask\n' > "$HERDR_PLUGIN_CONFIG_DIR/config.env"
mutate '.workspaces[0].label = "Normal label xyz ABC 012 escape\u001b end"'
/bin/bash "$ROOT/bin/guard.sh" refresh
vanish_workspace
fire pane.exited p1
LPG_SESSION_KEY=$(basename "$(dirname "$STATE_FILE")")
mkfifo "$TMP/answer"
exec 3<> "$TMP/answer"
/bin/bash "$ROOT/bin/confirm.sh" < "$TMP/answer" > "$TMP/prompt" 2> "$TMP/confirm-err" & confirmation=$!
for _ in {1..100}; do
    if grep -q 'Keep it?' "$TMP/prompt"; then break; fi
    sleep 0.05
done
grep -q 'Keep it?' "$TMP/prompt"
grep -Fq 'Normal label xyz ABC 012 escape? end' "$TMP/prompt"
passed=$((passed + 1))
# Keep the old inode allocated so the new generation cannot reuse it.
mv "$HERDR_SOCKET_PATH" "$TMP/socket-old"
touch "$HERDR_SOCKET_PATH"
printf 'n' >&3
wait "$confirmation"
exec 3>&-
check 'stale popup cannot close an object on a different server generation' '.workspaces | length == 1'

reset_case
vanish_workspace
fire pane.exited p1
first_state=$STATE_FILE
first_socket=$HERDR_SOCKET_PATH
export HERDR_SOCKET_PATH="$TMP/socket-other"
touch "$HERDR_SOCKET_PATH"
/bin/bash "$ROOT/bin/guard.sh" refresh
check 'first session retains idempotence marker' '.handled | has("p1")' "$first_state"
other_state=$(find "$HERDR_PLUGIN_STATE_DIR" -name state.json -type f | grep -F -v "$first_state")
check 'other session does not inherit markers' '.handled == {}' "$other_state"
export HERDR_SOCKET_PATH=$first_socket

# No socket on shutdown: normal success, no restoration calls.
rm "$HERDR_SOCKET_PATH"
before=$(wc -l < "$MOCK_CALLS")
fire pane.exited p1
after=$(wc -l < "$MOCK_CALLS")
[ "$before" = "$after" ] || { printf 'FAIL: shutdown called API\n' >&2; exit 1; }
passed=$((passed + 1))
printf 'runtime: %s checks passed\n' "$passed"

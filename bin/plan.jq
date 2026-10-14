# Inputs are validated snapshots; this planner has no side effects.
def skip($reason):
  {action: "skip", reason: $reason};

def restore_parameters($kind; $pane_id; $pane; $workspace; $tab; $mode):
  {
    kind: $kind,
    pane_id: $pane_id,
    workspace_id: $pane.workspace_id,
    tab_id: $pane.tab_id,
    label: (if $kind == "workspace" then $workspace.label else $tab.label end),
    tab_label: $tab.label,
    cwd: (
      if ($pane.foreground_cwd // "") != "" then $pane.foreground_cwd
      elif ($pane.cwd // "") != "" then $pane.cwd
      else null
      end
    ),
    index: (if $kind == "workspace" then $workspace.index else $tab.index end),
    focused: (
      if $kind == "workspace" then $workspace.focused
      else ($workspace.focused and ($workspace.active_tab_id == $pane.tab_id))
      end
    ),
    mode: $mode
  };

. as $input
| .pane_id as $pane_id
| .state.panes[$pane_id] as $pane
| if $pane == null then
    skip("missing_snapshot")
  else
    ($pane.workspace_snapshot // .state.workspaces[$pane.workspace_id]) as $workspace
    | ($pane.tab_snapshot // .state.tabs[$pane.tab_id]) as $tab
    | if $workspace == null or $tab == null then
        skip("missing_snapshot")
      elif (.state.handled | has($pane_id)) then
        skip("already_handled")
      elif (.state.closed.workspaces | has($pane.workspace_id))
        or (.state.closed.tabs | has($pane.tab_id))
        or (.state.closed.panes | has($pane_id)) then
        skip("intentional_close")
      elif any(.live.panes[]; .pane_id == $pane_id) then
        skip("still_present")
      else
        (
          if (any(.live.workspaces[]; .workspace_id == $pane.workspace_id) | not) then
            "workspace"
          elif (any(.live.tabs[]; .tab_id == $pane.tab_id) | not) then
            "tab"
          else
            null
          end
        ) as $kind
        | if $kind == null then
            skip("still_present")
          elif ((.state.claims // {}) | has($kind + ":" +
            (if $kind == "workspace" then $pane.workspace_id else $pane.tab_id end))) then
            skip("already_handled")
          else
            (
              if $kind == "workspace" then .config.restore_workspace
              else .config.restore_tab
              end
            ) as $mode
            | if $mode == "off" then
                skip("disabled")
              else
                restore_parameters($kind; $pane_id; $pane; $workspace; $tab; $mode)
                + (
                  if $input.config.loop_guard_seconds > 0
                    and ($input.state.restored | has($pane_id))
                    and (($input.now - $input.state.restored[$pane_id])
                      < $input.config.loop_guard_seconds) then
                    {action: "loop_guard", reason: "rapid_exit"}
                  else
                    {action: "restore", reason: "shell_exit"}
                  end
                )
              end
          end
      end
  end

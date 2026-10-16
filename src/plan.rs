use serde_json::{json, Value};

fn skip(reason: &str) -> Value {
    json!({"action": "skip", "reason": reason})
}

fn truthy(value: &Value) -> bool {
    !value.is_null() && value != &Value::Bool(false)
}

fn snapshot<'a>(saved: &'a Value, current: &'a Value) -> &'a Value {
    if truthy(saved) {
        saved
    } else {
        current
    }
}

fn has(map: &Value, key: &str) -> bool {
    map.as_object().is_some_and(|map| map.contains_key(key))
}

fn present(items: &Value, key: &str, id: &str) -> bool {
    items
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item[key] == id))
}

// Inputs are snapshots. Presence markers count even when their value is false or null.
pub fn decide(input: &Value) -> Value {
    let Some(pane_id) = input["pane_id"].as_str() else {
        return skip("missing_snapshot");
    };
    let state = &input["state"];
    let pane = &state["panes"][pane_id];
    let (Some(workspace_id), Some(tab_id)) =
        (pane["workspace_id"].as_str(), pane["tab_id"].as_str())
    else {
        return skip("missing_snapshot");
    };
    let workspace = snapshot(
        &pane["workspace_snapshot"],
        &state["workspaces"][workspace_id],
    );
    let tab = snapshot(&pane["tab_snapshot"], &state["tabs"][tab_id]);
    if workspace.is_null() || tab.is_null() {
        return skip("missing_snapshot");
    }
    if has(&state["handled"], pane_id) {
        return skip("already_handled");
    }
    let closed = &state["closed"];
    if has(&closed["workspaces"], workspace_id)
        || has(&closed["tabs"], tab_id)
        || has(&closed["panes"], pane_id)
    {
        return skip("intentional_close");
    }
    let live = &input["live"];
    if present(&live["panes"], "pane_id", pane_id) {
        return skip("still_present");
    }
    let (kind, container_id, container) =
        if !present(&live["workspaces"], "workspace_id", workspace_id) {
            ("workspace", workspace_id, workspace)
        } else if !present(&live["tabs"], "tab_id", tab_id) {
            ("tab", tab_id, tab)
        } else {
            return skip("still_present");
        };
    if has(&state["claims"], &format!("{kind}:{container_id}")) {
        return skip("already_handled");
    }
    let config = &input["config"];
    let mode = if kind == "workspace" {
        &config["restore_workspace"]
    } else {
        &config["restore_tab"]
    };
    if mode == "off" {
        return skip("disabled");
    }
    let cwd = [&pane["foreground_cwd"], &pane["cwd"]]
        .into_iter()
        .find(|value| truthy(value) && *value != "")
        .unwrap_or(&Value::Null);
    let focused = if kind == "workspace" {
        workspace["focused"].clone()
    } else {
        json!(truthy(&workspace["focused"]) && workspace["active_tab_id"] == tab_id)
    };
    // Signed subtraction preserves the guard for future restoration timestamps.
    let guarded = match (
        config["loop_guard_seconds"].as_f64(),
        input["now"].as_f64(),
        state["restored"][pane_id].as_f64(),
    ) {
        (Some(seconds), Some(now), Some(restored)) => seconds > 0.0 && now - restored < seconds,
        _ => false,
    };
    json!({
        "action": if guarded { "loop_guard" } else { "restore" },
        "reason": if guarded { "rapid_exit" } else { "shell_exit" },
        "kind": kind,
        "pane_id": pane_id,
        "workspace_id": workspace_id,
        "tab_id": tab_id,
        "label": container["label"],
        "tab_label": tab["label"],
        "cwd": cwd,
        "index": container["index"],
        "focused": focused,
        "mode": mode,
    })
}

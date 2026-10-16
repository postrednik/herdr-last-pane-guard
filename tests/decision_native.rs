use std::{fs, path::Path};

use last_pane_guard::plan::decide;
use serde_json::{json, Value};

fn baseline() -> Value {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/01_workspace_mode_ask.json")).unwrap();
    fixture["input"].clone()
}

#[test]
fn all_existing_decision_fixtures_match_expected_fields() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut files: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    files.sort();
    assert_eq!(files.len(), 69, "all decision fixtures must be exercised");
    for file in files {
        let fixture: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert!(
            fixture["input"].is_object(),
            "{}: invalid input",
            file.display()
        );
        let expected = fixture["expect"].as_object().expect("expected object");
        assert!(expected.contains_key("action") && expected.contains_key("reason"));
        let actual = decide(&fixture["input"]);
        assert!(
            actual.is_object(),
            "{}: output must be an object",
            file.display()
        );
        for (key, value) in expected {
            assert_eq!(
                actual.get(key),
                Some(value),
                "{}: field {key}",
                file.display()
            );
        }
    }
}

#[test]
fn embedded_snapshots_take_precedence_over_latest_metadata() {
    let mut input = baseline();
    let mut workspace = input["state"]["workspaces"]["w1"].clone();
    workspace["label"] = json!("Saved workspace");
    workspace["index"] = json!(3);
    let mut tab = input["state"]["tabs"]["w1:t1"].clone();
    tab["label"] = json!("Saved tab");
    tab["index"] = json!(7);
    input["state"]["panes"]["w1:p1"]["workspace_snapshot"] = workspace;
    input["state"]["panes"]["w1:p1"]["tab_snapshot"] = tab;
    input["state"]["workspaces"] = json!({});
    input["state"]["tabs"] = json!({});
    let workspace_plan = decide(&input);
    assert_eq!(workspace_plan["action"], "restore");
    assert_eq!(workspace_plan["label"], "Saved workspace");
    assert_eq!(workspace_plan["tab_label"], "Saved tab");
    assert_eq!(workspace_plan["index"], 3);
    input["live"]["workspaces"] = json!([{"workspace_id": "w1"}]);
    let tab_plan = decide(&input);
    assert_eq!(tab_plan["kind"], "tab");
    assert_eq!(tab_plan["label"], "Saved tab");
    assert_eq!(tab_plan["index"], 7);
}

#[test]
fn false_and_null_embedded_snapshots_fall_back_to_state_maps() {
    for value in [Value::Null, Value::Bool(false)] {
        let mut input = baseline();
        input["state"]["panes"]["w1:p1"]["workspace_snapshot"] = value.clone();
        input["state"]["panes"]["w1:p1"]["tab_snapshot"] = value;
        assert_eq!(decide(&input), decide(&baseline()));
    }
}

#[test]
fn claims_use_key_presence_and_container_kind() {
    for value in [json!(true), json!(false), Value::Null] {
        let mut input = baseline();
        input["state"]["claims"] = json!({"workspace:w1": value});
        input["config"]["restore_workspace"] = json!("off");
        assert_eq!(
            decide(&input),
            json!({"action": "skip", "reason": "already_handled"})
        );
        input["live"]["workspaces"] = json!([{"workspace_id": "w1"}]);
        assert_eq!(decide(&input)["action"], "restore");
        input["state"]["claims"] = json!({"tab:w1:t1": value});
        input["config"]["restore_tab"] = json!("off");
        assert_eq!(
            decide(&input),
            json!({"action": "skip", "reason": "already_handled"})
        );
    }
}

#[test]
fn close_markers_use_key_presence_including_false_and_null() {
    for (collection, id) in [("workspaces", "w1"), ("tabs", "w1:t1"), ("panes", "w1:p1")] {
        for value in [json!(false), Value::Null] {
            let mut input = baseline();
            input["state"]["closed"][collection][id] = value;
            assert_eq!(
                decide(&input),
                json!({"action": "skip", "reason": "intentional_close"})
            );
        }
    }
}

#[test]
fn visibility_and_snapshot_checks_precede_claims() {
    let mut input = baseline();
    input["state"]["claims"] = json!({"workspace:w1": true});
    input["live"]["panes"] = json!([{"pane_id": "w1:p1"}]);
    assert_eq!(decide(&input)["reason"], "still_present");
    input["state"]["workspaces"]["w1"] = Value::Null;
    assert_eq!(decide(&input)["reason"], "missing_snapshot");
}

#[test]
fn signed_and_fractional_timestamps_preserve_guard_boundaries() {
    for (now, restored, action) in [
        (-2.0, -3.0, "loop_guard"),
        (-2.0, 3.0, "loop_guard"),
        (2.0, -3.0, "restore"),
        (2.5, -2.0, "loop_guard"),
        (3.0, -2.0, "restore"),
    ] {
        let mut input = baseline();
        input["now"] = json!(now);
        input["state"]["restored"]["w1:p1"] = json!(restored);
        assert_eq!(decide(&input)["action"], action);
    }
}

#[test]
fn empty_input_does_not_panic_or_restore() {
    for input in [
        Value::Null,
        json!({}),
        json!({"pane_id": "missing", "state": null}),
    ] {
        assert_eq!(
            decide(&input),
            json!({"action": "skip", "reason": "missing_snapshot"})
        );
    }
}

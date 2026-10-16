#![cfg(unix)]

use last_pane_guard::{
    state::{session_key, Lock, Store},
    Error,
};
use serde_json::json;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::process::Command;
use std::time::{Duration, Instant};

fn setup() -> (tempfile::TempDir, Store) {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("socket");
    fs::write(&socket, b"").unwrap();
    let store = Store::new(&temp.path().join("state"), socket).unwrap();
    (temp, store)
}

#[test]
fn lock_is_private_and_not_stolen() {
    let (_temp, store) = setup();
    let lock = store.lock().unwrap();
    let started = Instant::now();
    assert!(matches!(
        Lock::acquire(&store.directory, &store.socket, Duration::from_millis(80)),
        Err(Error::Timeout)
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        fs::read_to_string(store.directory.join("lock/owner")).unwrap(),
        format!("{}\n", std::process::id())
    );
    assert_eq!(
        fs::metadata(&store.directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    drop(lock);
    assert!(!store.directory.join("lock").exists());
}

#[test]
fn lock_release_does_not_remove_changed_owner() {
    let (_temp, store) = setup();
    let lock = store.lock().unwrap();
    fs::write(store.directory.join("lock/owner"), b"someone-else\n").unwrap();
    drop(lock);
    assert!(store.directory.join("lock").exists());
}

#[test]
fn corrupt_and_foreign_state_are_never_overwritten() {
    let (_temp, store) = setup();
    let _lock = store.lock().unwrap();
    let mut state = store.prepare().unwrap();
    state["socket"] = json!("different-session");
    let bytes = serde_json::to_vec(&state).unwrap();
    fs::write(store.state_path(), &bytes).unwrap();
    assert!(store.prepare().is_err());
    assert_eq!(fs::read(store.state_path()).unwrap(), bytes);
    fs::write(store.state_path(), b"not json").unwrap();
    assert!(store.prepare().is_err());
    assert_eq!(fs::read(store.state_path()).unwrap(), b"not json");
}

#[test]
fn malformed_generation_never_discards_recovery_history() {
    let (_temp, store) = setup();
    let _lock = store.lock().unwrap();
    let mut original = store.prepare().unwrap();
    original["claims"]["workspace:pending"] = json!(true);
    original["handled"]["pending-pane"] = json!({"status":"claimed"});
    for generation in [
        None,
        Some(json!(null)),
        Some(json!(123)),
        Some(json!("")),
        Some(json!("not-an-inode")),
        Some(json!("-42")),
    ] {
        let mut malformed = original.clone();
        match generation {
            Some(value) => malformed["generation"] = value,
            None => {
                malformed.as_object_mut().unwrap().remove("generation");
            }
        }
        let bytes = serde_json::to_vec(&malformed).unwrap();
        fs::write(store.state_path(), &bytes).unwrap();
        assert!(
            store.prepare().is_err(),
            "malformed generation must fail closed"
        );
        assert_eq!(fs::read(store.state_path()).unwrap(), bytes);
    }
}

#[test]
fn failed_atomic_replace_leaves_target_and_removes_temporary_file() {
    let (_temp, store) = setup();
    let _lock = store.lock().unwrap();
    fs::create_dir(store.state_path()).unwrap();
    fs::write(store.state_path().join("keep"), b"original").unwrap();
    assert!(store.save(&json!({"test":true})).is_err());
    assert_eq!(
        fs::read(store.state_path().join("keep")).unwrap(),
        b"original"
    );
    assert!(fs::read_dir(&store.directory).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".state.")));
}

#[test]
fn dangling_state_symlink_is_not_replaced() {
    let (_temp, store) = setup();
    let _lock = store.lock().unwrap();
    symlink(store.directory.join("missing"), store.state_path()).unwrap();
    assert!(store.prepare().is_err());
    assert!(fs::symlink_metadata(store.state_path())
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn generation_rollover_invalidates_pending_work() {
    let (temp, store) = setup();
    {
        let _lock = store.lock().unwrap();
        let mut state = store.prepare().unwrap();
        state["handled"]["old-pane"] = json!({"status":"claimed"});
        store.save(&state).unwrap();
    }
    fs::rename(&store.socket, temp.path().join("old-socket")).unwrap();
    fs::write(&store.socket, b"").unwrap();
    assert!(matches!(store.lock(), Err(Error::Stale)));
    assert!(!store.directory.join("lock").exists());
    let replacement = Store::new(&temp.path().join("state"), store.socket.clone()).unwrap();
    let _lock = replacement.lock().unwrap();
    assert_eq!(replacement.prepare().unwrap()["handled"], json!({}));
}

#[test]
fn state_mode_is_private_and_unknown_fields_survive() {
    let (_temp, store) = setup();
    let _lock = store.lock().unwrap();
    let mut state = store.prepare().unwrap();
    state["future"] = json!({"opaque":[1,2,3]});
    state["handled"]["null-marker"] = json!(null);
    state["claims"]["workspace:example"] = json!(false);
    store.save(&state).unwrap();
    assert_eq!(store.load().unwrap().unwrap(), state);
    assert_eq!(
        fs::metadata(store.state_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn posix_checksum_matches_legacy_utility() {
    use std::io::Write;
    use std::process::Stdio;
    for bytes in [
        b"".as_slice(),
        b"abc",
        b"path with spaces/\xc3\xa9/socket",
        &[255u8; 256],
        &[42u8; 4096],
    ] {
        let mut child = Command::new("cksum")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(bytes).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let expected = String::from_utf8(output.stdout)
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("-");
        assert_eq!(session_key(bytes), expected);
    }
}

#[test]
fn bash_and_rust_share_state_and_lock_contract() {
    let (temp, store) = setup();
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut command = Command::new("/bin/bash");
    command.args(["-c", ". \"$1/bin/common.sh\"; lpg_init && lpg_lock && lpg_prepare_state && lpg_state '.extension = {legacy:true} | .handled.old = null | .claims[\"workspace:old\"] = false'", "migration-test"])
        .arg(project)
        .env("HERDR_BIN_PATH", project.join("tests/mock-herdr.sh"))
        .env("HERDR_SOCKET_PATH", &store.socket)
        .env("HERDR_PLUGIN_STATE_DIR", temp.path().join("state"))
        .env("HERDR_PLUGIN_CONFIG_DIR", temp.path().join("config"));
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "legacy fixture setup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    {
        let _lock = store.lock().unwrap();
        let mut state = store.prepare().unwrap();
        assert_eq!(state["extension"], json!({"legacy":true}));
        assert!(state["handled"].as_object().unwrap().contains_key("old"));
        assert_eq!(state["claims"]["workspace:old"], false);
        state["native"] = json!(true);
        store.save(&state).unwrap();
    }
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(store.state_path()).unwrap()).unwrap();
    assert_eq!(state["native"], true);
    let output = Command::new("jq")
        .args([
            "-e",
            ".native == true and .extension.legacy == true and .version == 1",
        ])
        .arg(store.state_path())
        .output()
        .unwrap();
    assert!(output.status.success());
}

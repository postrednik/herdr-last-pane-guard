#![cfg(unix)]

use last_pane_guard::state::session_key;
use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const DEADLINE: Duration = Duration::from_secs(8);
static OUTPUT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug)]
enum CreateFault {
    Malformed,
    Truncated,
    Lost,
    MissingRootPane,
    InconsistentIds,
}

struct Model {
    live: Value,
    calls: Vec<Value>,
    errors: Vec<String>,
    fail_method: Option<String>,
    stall_method: Option<String>,
    create_fault: Option<CreateFault>,
    popup_busy: bool,
}

impl Model {
    fn new(cwd: &Path) -> Self {
        Self {
            live: json!({
                "counter":0,
                "workspaces":[{"workspace_id":"w1","label":"Example","focused":true,
                    "active_tab_id":"t1","number":42}],
                "tabs":[{"tab_id":"t1","workspace_id":"w1","label":"custom",
                    "number":7,"focused":true}],
                "panes":[{"pane_id":"p1","workspace_id":"w1","tab_id":"t1",
                    "cwd":cwd,"foreground_cwd":cwd}]
            }),
            calls: Vec::new(),
            errors: Vec::new(),
            fail_method: None,
            stall_method: None,
            create_fault: None,
            popup_busy: false,
        }
    }

    fn response(&mut self, request: &Value) -> Value {
        let method = request["method"].as_str().unwrap();
        let params = &request["params"];
        if self.fail_method.as_deref() == Some(method) {
            return json!({"id":request["id"],"error":{"code":"permission_denied"}});
        }
        let (kind, verb) = method.split_once('.').unwrap();
        let collection = format!("{kind}s");
        let id_field = format!("{kind}_id");
        let result = match method {
            "workspace.list" | "pane.list" => json!({collection.as_str():self.live[&collection]}),
            "tab.list" => json!({"tabs":self.live["tabs"].as_array().unwrap().iter()
                .filter(|tab|tab["workspace_id"] == params["workspace_id"]).collect::<Vec<_>>()}),
            "workspace.get" | "tab.get" | "pane.get" => {
                let found = self.live[&collection]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|object| object[&id_field] == params[&id_field]);
                match found {
                    Some(object) => json!({kind:object}),
                    None => {
                        return json!({"id":request["id"],"error":{"code":format!("{kind}_not_found")}})
                    }
                }
            }
            "workspace.create" | "tab.create" => {
                let count = self.live["counter"].as_u64().unwrap() + 1;
                self.live["counter"] = json!(count);
                let ws = if kind == "workspace" {
                    format!("restored-w{count}")
                } else {
                    params["workspace_id"].as_str().unwrap().to_owned()
                };
                let tab = format!("restored-t{count}");
                let pane = format!("restored-p{count}");
                let workspace = json!({"workspace_id":ws,"label":params["label"],"focused":params["focus"],
                    "active_tab_id":tab,"number":1,"pane_count":1,"tab_count":1});
                let tab_object = json!({"tab_id":tab,"workspace_id":ws,
                    "label":if kind == "tab" {params["label"].clone()} else {json!("1")},
                    "number":1,"focused":params["focus"],"pane_count":1});
                let root_pane = json!({"pane_id":pane,"workspace_id":ws,"tab_id":tab,
                    "cwd":params["cwd"],"foreground_cwd":params["cwd"],"focused":params["focus"]});
                if kind == "workspace" {
                    self.live["workspaces"]
                        .as_array_mut()
                        .unwrap()
                        .push(workspace.clone());
                }
                self.live["tabs"]
                    .as_array_mut()
                    .unwrap()
                    .push(tab_object.clone());
                self.live["panes"]
                    .as_array_mut()
                    .unwrap()
                    .push(root_pane.clone());
                let mut result = json!({"tab":tab_object,"root_pane":root_pane});
                if kind == "workspace" {
                    result["workspace"] = workspace;
                }
                match self.create_fault {
                    Some(CreateFault::MissingRootPane) => {
                        result.as_object_mut().unwrap().remove("root_pane");
                    }
                    Some(CreateFault::InconsistentIds) => {
                        result["root_pane"]["tab_id"] = json!("unrelated-tab");
                    }
                    _ => (),
                }
                result
            }
            "workspace.move" | "tab.move" => {
                let items = self.live[&collection].as_array_mut().unwrap();
                let from = items
                    .iter()
                    .position(|object| object[&id_field] == params[&id_field])
                    .unwrap();
                let object = items.remove(from);
                let to = usize::try_from(params["insert_index"].as_u64().unwrap())
                    .unwrap()
                    .min(items.len());
                items.insert(to, object);
                json!({"type":"ok"})
            }
            "tab.rename" => {
                let tab = self.live["tabs"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|tab| tab["tab_id"] == params["tab_id"])
                    .unwrap();
                tab["label"] = params["label"].clone();
                json!({"type":"ok"})
            }
            "workspace.close" | "tab.close" => {
                for name in if kind == "workspace" {
                    &["workspaces", "tabs", "panes"][..]
                } else {
                    &["tabs", "panes"][..]
                } {
                    self.live[*name]
                        .as_array_mut()
                        .unwrap()
                        .retain(|object| object[&id_field] != params[&id_field]);
                }
                json!({"type":"ok"})
            }
            "plugin.pane.open" if self.popup_busy => {
                return json!({"id":request["id"],"error":{"code":"ui_busy"}})
            }
            "plugin.pane.open" | "notification.show" => json!({"type":"ok"}),
            _ => panic!("validated method was not implemented: {kind}.{verb}"),
        };
        json!({"id":request["id"],"result":result})
    }
}

fn valid_request(request: &Value) -> bool {
    let Some(envelope) = request.as_object() else {
        return false;
    };
    if envelope.len() != 3 || !request["id"].is_string() {
        return false;
    }
    let Some(method) = request["method"].as_str() else {
        return false;
    };
    let Some(params) = request["params"].as_object() else {
        return false;
    };
    let (required, optional): (&[&str], &[&str]) = match method {
        "workspace.list" | "pane.list" => (&[], &[]),
        "workspace.get" | "workspace.close" | "tab.list" => (&["workspace_id"], &[]),
        "tab.get" | "tab.close" => (&["tab_id"], &[]),
        "pane.get" => (&["pane_id"], &[]),
        "workspace.create" => (&["label", "focus"], &["cwd"]),
        "tab.create" => (&["workspace_id", "label", "focus"], &["cwd"]),
        "workspace.move" => (&["workspace_id", "insert_index"], &[]),
        "tab.move" => (&["tab_id", "insert_index"], &[]),
        "tab.rename" => (&["tab_id", "label"], &[]),
        "notification.show" => (&["title", "body"], &[]),
        "plugin.pane.open" => (&["plugin_id", "entrypoint", "env", "focus"], &[]),
        _ => return false,
    };
    if required.iter().any(|key| !params.contains_key(*key))
        || params
            .keys()
            .any(|key| !required.contains(&key.as_str()) && !optional.contains(&key.as_str()))
    {
        return false;
    }
    params.iter().all(|(key, value)| match key.as_str() {
        "focus" if method == "plugin.pane.open" => value == true,
        "focus" => value.is_boolean(),
        "insert_index" => value.is_u64(),
        "env" => value.as_object().is_some_and(|env| {
            env.len() == 2
                && env.get("LPG_RECOVERY_TOKEN").is_some_and(Value::is_string)
                && env.get("LPG_SESSION_KEY").is_some_and(Value::is_string)
        }),
        "plugin_id" => value == "postrednik.last-pane-guard",
        "entrypoint" => value == "confirm",
        _ => value.is_string(),
    })
}

struct Server {
    socket: PathBuf,
    model: Arc<Mutex<Model>>,
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Server {
    fn new(socket: PathBuf, cwd: &Path) -> Self {
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let model = Arc::new(Mutex::new(Model::new(cwd)));
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_model = Arc::clone(&model);
        let worker_stop = Arc::clone(&stopping);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => serve(stream, &worker_model, &worker_stop),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => {
                        worker_model
                            .lock()
                            .unwrap()
                            .errors
                            .push(format!("accept: {error}"));
                        break;
                    }
                }
            }
        });
        Self {
            socket,
            model,
            stopping,
            worker: Some(worker),
        }
    }

    fn stop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
        assert!(
            self.model.lock().unwrap().errors.is_empty(),
            "mock server rejected a request"
        );
    }

    fn calls(&self, method: &str) -> Vec<Value> {
        self.model
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|call| call["method"] == method)
            .cloned()
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                eprintln!("mock server thread failed");
            }
        }
    }
}

fn serve(mut stream: UnixStream, model: &Arc<Mutex<Model>>, stopping: &AtomicBool) {
    // BSD-derived systems let an accepted socket inherit the listener's nonblocking
    // mode, which would make an early read fail instead of honoring the timeout.
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut line = String::new();
    if let Err(error) = BufReader::new((&stream).take(64 * 1024)).read_line(&mut line) {
        model
            .lock()
            .unwrap()
            .errors
            .push(format!("request read: {error}"));
        return;
    }
    let request = serde_json::from_str::<Value>(&line).unwrap_or(Value::Null);
    if !line.ends_with('\n') || !valid_request(&request) {
        model
            .lock()
            .unwrap()
            .errors
            .push("invalid request shape".into());
        writeln!(
            stream,
            "{}",
            json!({"id":request["id"],"error":{"code":"invalid_params"}})
        )
        .unwrap();
        return;
    }
    let (response, stall, fault) = {
        let mut model = model.lock().unwrap();
        model.calls.push(request.clone());
        let stall = model.stall_method.as_deref() == request["method"].as_str();
        let fault = if matches!(
            request["method"].as_str(),
            Some("workspace.create" | "tab.create")
        ) {
            model.create_fault
        } else {
            None
        };
        (model.response(&request), stall, fault)
    };
    if stall {
        let deadline = Instant::now() + DEADLINE;
        while !stopping.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        return;
    }
    let bytes = match fault {
        Some(CreateFault::Malformed) => b"not-json\n".to_vec(),
        Some(CreateFault::Truncated) => response.to_string().into_bytes(),
        Some(CreateFault::Lost) => return,
        _ => format!("{response}\n").into_bytes(),
    };
    if let Err(error) = stream.write_all(&bytes) {
        model
            .lock()
            .unwrap()
            .errors
            .push(format!("response write: {error}"));
    }
}

struct Process {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

struct Output {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

impl Output {
    fn success(&self) {
        assert!(
            self.status.success(),
            "child failed: {}\n{}",
            self.status,
            self.stderr
        );
    }
}

impl Process {
    fn spawn(command: Command, directory: &Path) -> Self {
        Self::spawn_with_input(command, directory, Stdio::piped())
    }

    fn spawn_with_input(mut command: Command, directory: &Path, input: Stdio) -> Self {
        let id = OUTPUT_ID.fetch_add(1, Ordering::Relaxed);
        let stdout = directory.join(format!("stdout-{id}"));
        let stderr = directory.join(format!("stderr-{id}"));
        let child = command
            .stdin(input)
            .stdout(File::create(&stdout).unwrap())
            .stderr(File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        Self {
            child,
            stdout,
            stderr,
        }
    }

    fn answer(&mut self, input: &[u8]) {
        let mut stdin = self.child.stdin.take().unwrap();
        stdin.write_all(input).unwrap();
    }

    fn wait(mut self) -> Output {
        self.child.stdin.take();
        let deadline = Instant::now() + DEADLINE;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "child timed out; stderr: {}",
                fs::read_to_string(&self.stderr).unwrap()
            );
            thread::sleep(Duration::from_millis(5));
        };
        Output {
            status,
            stdout: fs::read_to_string(&self.stdout).unwrap(),
            stderr: fs::read_to_string(&self.stderr).unwrap(),
        }
    }

    fn wait_prompt(&mut self) -> String {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let text = fs::read_to_string(&self.stdout).unwrap();
            if text.contains("Keep it? [Y/n]") {
                return text;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "confirmation exited before prompting"
            );
            assert!(Instant::now() < deadline, "confirmation prompt timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct Terminal {
    master: File,
    slave: File,
    saved: libc::termios,
}

impl Terminal {
    fn new() -> Self {
        let mut master = -1;
        let mut slave = -1;
        assert_eq!(
            // SAFETY: both output pointers are writable; null optional arguments request defaults.
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        // SAFETY: openpty succeeded and returned two distinct, newly owned descriptors.
        let (master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
        for file in [&master, &slave] {
            assert_eq!(
                // SAFETY: each descriptor is live; F_SETFD takes an integer flag argument.
                unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) },
                0
            );
        }
        let saved = Self::attributes(&slave);
        assert_ne!(saved.c_lflag & libc::ICANON, 0);
        assert_ne!(saved.c_lflag & libc::ECHO, 0);
        Self {
            master,
            slave,
            saved,
        }
    }

    fn attributes(slave: &File) -> libc::termios {
        let mut attributes = std::mem::MaybeUninit::uninit();
        assert_eq!(
            // SAFETY: slave is live; attributes provides writable termios-sized storage.
            unsafe { libc::tcgetattr(slave.as_raw_fd(), attributes.as_mut_ptr()) },
            0
        );
        // SAFETY: successful tcgetattr initialized the complete termios value.
        unsafe { attributes.assume_init() }
    }

    fn confirmation(&self, fixture: &Fixture) -> Process {
        let mut command = fixture.command("confirm");
        command
            .env("LPG_RECOVERY_TOKEN", "p1")
            .env("LPG_SESSION_KEY", fixture.key());
        Process::spawn_with_input(
            command,
            fixture.directory.path(),
            Stdio::from(self.slave.try_clone().unwrap()),
        )
    }

    fn wait_raw(&self, process: &mut Process) {
        process.wait_prompt();
        let deadline = Instant::now() + DEADLINE;
        loop {
            if Self::attributes(&self.slave).c_lflag & (libc::ICANON | libc::ECHO) == 0 {
                return;
            }
            assert!(
                process.child.try_wait().unwrap().is_none(),
                "confirmation exited before entering raw input"
            );
            assert!(
                Instant::now() < deadline,
                "confirmation never changed terminal input mode"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn assert_restored(&self) {
        let actual = Self::attributes(&self.slave);
        assert_eq!(actual.c_iflag, self.saved.c_iflag);
        assert_eq!(actual.c_oflag, self.saved.c_oflag);
        assert_eq!(actual.c_cflag, self.saved.c_cflag);
        // Darwin adds PENDIN itself when ICANON is restored; it is kernel-managed state.
        #[cfg(target_vendor = "apple")]
        assert_eq!(
            actual.c_lflag & !libc::PENDIN,
            self.saved.c_lflag & !libc::PENDIN
        );
        #[cfg(not(target_vendor = "apple"))]
        assert_eq!(actual.c_lflag, self.saved.c_lflag);
        assert_eq!(actual.c_cc, self.saved.c_cc);
        // SAFETY: all pointers refer to live initialized termios values.
        unsafe {
            assert_eq!(libc::cfgetispeed(&actual), libc::cfgetispeed(&self.saved));
            assert_eq!(libc::cfgetospeed(&actual), libc::cfgetospeed(&self.saved));
        }
    }
}

struct Fixture {
    server: Server,
    directory: TempDir,
    config: PathBuf,
    state_root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config");
        fs::create_dir(&config).unwrap();
        fs::write(
            config.join("config.env"),
            "RESTORE_WORKSPACE=auto\nRESTORE_TAB=auto\n",
        )
        .unwrap();
        let server = Server::new(directory.path().join("api.sock"), directory.path());
        let state_root = directory.path().join("state");
        Self {
            server,
            directory,
            config,
            state_root,
        }
    }

    fn command(&self, mode: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_last-pane-guard"));
        command
            .env_clear()
            .current_dir(self.directory.path())
            .arg(mode)
            .env("HERDR_SOCKET_PATH", &self.server.socket)
            .env("HERDR_PLUGIN_STATE_DIR", &self.state_root)
            .env("HERDR_PLUGIN_CONFIG_DIR", &self.config);
        command
    }

    fn spawn(&self, command: Command) -> Process {
        Process::spawn(command, self.directory.path())
    }

    fn refresh(&self) {
        self.spawn(self.command("refresh")).wait().success();
    }

    fn event(&self, event: &str, id: &str) -> Process {
        let kind = event.split('.').next().unwrap();
        let mut command = self.command("event");
        command.env("HERDR_PLUGIN_EVENT", event).env(
            "HERDR_PLUGIN_EVENT_JSON",
            json!({"data":{format!("{kind}_id"):id}}).to_string(),
        );
        self.spawn(command)
    }

    fn fire(&self, event: &str, id: &str) -> Output {
        let output = self.event(event, id).wait();
        output.success();
        assert!(self.server.model.lock().unwrap().errors.is_empty());
        output
    }

    fn key(&self) -> String {
        session_key(self.server.socket.as_os_str().as_bytes())
    }

    fn state_path(&self) -> PathBuf {
        self.state_root.join(self.key()).join("state.json")
    }

    fn state(&self) -> Value {
        serde_json::from_slice(&fs::read(self.state_path()).unwrap()).unwrap()
    }

    fn live(&self) -> Value {
        self.server.model.lock().unwrap().live.clone()
    }

    fn mutate(&self, change: impl FnOnce(&mut Value)) {
        change(&mut self.server.model.lock().unwrap().live);
    }

    fn vanish(&self, kind: &str) {
        self.mutate(|live| {
            if kind == "workspace" {
                live["workspaces"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|ws| ws["workspace_id"] != "w1");
            }
            live["tabs"]
                .as_array_mut()
                .unwrap()
                .retain(|tab| tab["tab_id"] != "t1");
            live["panes"]
                .as_array_mut()
                .unwrap()
                .retain(|pane| pane["tab_id"] != "t1");
        });
    }

    fn ask(&self) {
        fs::write(
            self.config.join("config.env"),
            "RESTORE_WORKSPACE=ask\nRESTORE_TAB=ask\n",
        )
        .unwrap();
    }

    fn restore(&self, kind: &str) {
        self.refresh();
        self.vanish(kind);
        self.fire("pane.exited", "p1");
    }

    fn confirmation(&self) -> Process {
        let mut command = self.command("confirm");
        command
            .env("LPG_RECOVERY_TOKEN", "p1")
            .env("LPG_SESSION_KEY", self.key());
        self.spawn(command)
    }
}

#[test]
fn workspace_restores_retained_metadata_focus_cwd_and_array_position() {
    for focused in [true, false] {
        let fixture = Fixture::new();
        let foreground = fixture.directory.path().join("foreground directory");
        fs::create_dir(&foreground).unwrap();
        fixture.mutate(|live| {
            live["workspaces"][0]["focused"] = json!(focused);
            let mut other = live["workspaces"][0].clone();
            other["workspace_id"] = json!("other-workspace");
            live["workspaces"].as_array_mut().unwrap().insert(0, other);
            live["panes"][0]["foreground_cwd"] = json!(foreground);
        });
        fixture.refresh();
        fixture.vanish("workspace");
        fixture.refresh();
        fixture.fire("pane.exited", "p1");
        let live = fixture.live();
        assert_eq!(live["workspaces"].as_array().unwrap().len(), 2);
        assert_eq!(live["workspaces"][1]["label"], "Example");
        assert_eq!(live["workspaces"][1]["focused"], focused);
        assert_eq!(live["tabs"][0]["label"], "custom");
        assert_eq!(live["panes"][0]["cwd"], json!(foreground));
        assert_eq!(fixture.state()["tabs"]["t1"]["index"], 0);
        assert_eq!(
            fixture.server.calls("workspace.move")[0]["params"]["insert_index"],
            1
        );
        fixture.fire("pane.exited", "p1");
        assert_eq!(fixture.live()["counter"], 1);
        assert_eq!(fixture.server.calls("workspace.create").len(), 1);
    }
}

#[test]
fn tab_restores_snapshot_focus_label_and_position_without_another_workspace() {
    let fixture = Fixture::new();
    fixture.mutate(|live| {
        let mut other = live["tabs"][0].clone();
        other["tab_id"] = json!("other-tab");
        live["tabs"].as_array_mut().unwrap().insert(0, other);
    });
    fixture.refresh();
    fixture.vanish("tab");
    fixture.mutate(|live| live["workspaces"][0]["active_tab_id"] = json!("other-tab"));
    fixture.refresh();
    fixture.fire("pane.exited", "p1");
    let live = fixture.live();
    assert_eq!(live["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(live["counter"], 1);
    assert_eq!(live["tabs"][1]["label"], "custom");
    assert_eq!(live["tabs"][1]["focused"], true);
    assert_eq!(
        fixture.server.calls("tab.move")[0]["params"]["insert_index"],
        1
    );
    assert!(fixture.server.calls("workspace.create").is_empty());
}

#[test]
fn concurrent_duplicate_and_distinct_panes_claim_each_entity_once() {
    for kind in ["workspace", "tab"] {
        for second in ["p1", "p2"] {
            let fixture = Fixture::new();
            fixture.mutate(|live| {
                let mut pane = live["panes"][0].clone();
                pane["pane_id"] = json!("p2");
                live["panes"].as_array_mut().unwrap().push(pane);
            });
            fixture.refresh();
            fixture.vanish(kind);
            let first = fixture.event("pane.exited", "p1");
            let second = fixture.event("pane.exited", second);
            first.wait().success();
            second.wait().success();
            assert_eq!(fixture.live()["counter"], 1, "{kind}");
            assert_eq!(fixture.server.calls(&format!("{kind}.create")).len(), 1);
            let id = if kind == "workspace" { "w1" } else { "t1" };
            assert_eq!(fixture.state()["claims"][format!("{kind}:{id}")], true);
            assert!(!fixture.state_path().parent().unwrap().join("lock").exists());
        }
    }
}

#[test]
fn intentional_close_events_suppress_restore_in_either_order() {
    for kind in ["workspace", "tab"] {
        for reverse in [false, true] {
            let fixture = Fixture::new();
            fixture.refresh();
            fixture.vanish(kind);
            let container = (
                format!("{kind}.closed"),
                if kind == "workspace" { "w1" } else { "t1" },
            );
            let mut events = vec![(container.0.as_str(), container.1), ("pane.closed", "p1")];
            if reverse {
                events.reverse();
            }
            for (event, id) in events {
                fixture.fire(event, id);
            }
            fixture.fire("pane.exited", "p1");
            assert_eq!(fixture.live()["counter"], 0);
            assert_eq!(
                fixture.state()["handled"]["p1"]["status"],
                "intentional_close"
            );
        }
    }
}

#[test]
fn off_never_restores_either_kind() {
    for kind in ["workspace", "tab"] {
        let fixture = Fixture::new();
        fs::write(
            fixture.config.join("config.env"),
            "RESTORE_WORKSPACE=off\nRESTORE_TAB=off\n",
        )
        .unwrap();
        fixture.restore(kind);
        assert_eq!(fixture.live()["counter"], 0);
        assert_eq!(fixture.state()["handled"]["p1"]["status"], "disabled");
    }
}

#[test]
fn lookup_api_errors_are_not_absence_and_leave_recovery_retryable() {
    for method in ["pane.get", "workspace.get", "tab.get"] {
        let fixture = Fixture::new();
        fixture.refresh();
        fixture.vanish("workspace");
        fixture.server.model.lock().unwrap().fail_method = Some(method.into());
        let output = fixture.event("pane.exited", "p1").wait();
        assert!(!output.status.success(), "{method}");
        assert!(output.stderr.contains("API rejected"));
        assert_eq!(fixture.live()["counter"], 0);
        assert!(fixture.state()["handled"]["p1"].is_null());
        fixture.server.model.lock().unwrap().fail_method = None;
        fixture.fire("pane.exited", "p1");
        assert_eq!(fixture.live()["counter"], 1);
    }
}

#[test]
fn rapid_replacement_shell_exit_is_guarded() {
    for kind in ["workspace", "tab"] {
        let fixture = Fixture::new();
        fs::write(
            fixture.config.join("config.env"),
            "RESTORE_WORKSPACE=auto\nRESTORE_TAB=auto\nLOOP_GUARD_SECONDS=3600\n",
        )
        .unwrap();
        fixture.restore(kind);
        let pane = fixture.live()["panes"][0]["pane_id"]
            .as_str()
            .unwrap()
            .to_owned();
        fixture.mutate(|live| {
            if kind == "workspace" {
                live["workspaces"] = json!([]);
            }
            live["tabs"] = json!([]);
            live["panes"] = json!([]);
        });
        fixture.fire("pane.exited", &pane);
        assert_eq!(fixture.live()["counter"], 1);
        assert!(fixture.live()["panes"].as_array().unwrap().is_empty());
        assert_eq!(fixture.state()["handled"][&pane]["status"], "loop_guard");
    }
}

#[test]
fn busy_confirmation_popup_keeps_restore_and_notifies() {
    for kind in ["workspace", "tab"] {
        let fixture = Fixture::new();
        fixture.ask();
        fixture.server.model.lock().unwrap().popup_busy = true;
        fixture.refresh();
        fixture.vanish(kind);
        let output = fixture.fire("pane.exited", "p1");
        assert!(output.stderr.contains("confirmation UI busy"));
        assert_eq!(fixture.live()["counter"], 1);
        assert_eq!(fixture.live()["panes"].as_array().unwrap().len(), 1);
        let popup = fixture.server.calls("plugin.pane.open");
        assert_eq!(popup.len(), 1);
        assert_eq!(popup[0]["params"]["focus"], true);
        assert_eq!(popup[0]["params"]["env"]["LPG_SESSION_KEY"], fixture.key());
        assert_eq!(popup[0]["params"]["env"]["LPG_RECOVERY_TOKEN"], "p1");
        assert!(
            fixture.server.calls("notification.show")[0]["params"]["body"]
                .as_str()
                .unwrap()
                .contains("kept")
        );
    }
}

#[test]
fn confirmation_no_closes_only_recorded_object_and_cannot_replay() {
    for kind in ["workspace", "tab"] {
        let fixture = Fixture::new();
        fixture.ask();
        fixture.restore(kind);
        let recovery = fixture.state()["handled"]["p1"].clone();
        let mut confirmation = fixture.confirmation();
        confirmation.answer(b"n");
        confirmation.wait().success();
        assert!(fixture.live()["panes"].as_array().unwrap().is_empty());
        assert_eq!(fixture.state()["handled"]["p1"]["confirmed"], true);
        let calls = fixture.server.calls(&format!("{kind}.close"));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["params"][format!("{kind}_id")], recovery["id"]);
        // Consumed tokens must return without displaying another prompt.
        let replay = fixture.confirmation().wait();
        replay.success();
        assert!(replay.stdout.is_empty());
        assert_eq!(fixture.server.calls(&format!("{kind}.close")).len(), 1);
        assert_eq!(fixture.live()["counter"], 1);
    }
}

fn assert_inconsistent_confirmation_rejected(kind: &str, field: &str, value: &str) {
    for prompt_pending in [false, true] {
        let fixture = Fixture::new();
        fixture.ask();
        fixture.restore(kind);
        fixture.mutate(|live| {
            live["workspaces"].as_array_mut().unwrap().push(json!({
                "workspace_id":"other-workspace","label":"Unrelated","focused":false,
                "active_tab_id":"other-tab","number":2
            }));
            live["tabs"].as_array_mut().unwrap().push(json!({
                "tab_id":"other-tab","workspace_id":"other-workspace","label":"Other",
                "focused":false,"number":2
            }));
            live["panes"].as_array_mut().unwrap().push(json!({
                "pane_id":"other-pane","workspace_id":"other-workspace","tab_id":"other-tab"
            }));
        });
        let mut pending = prompt_pending.then(|| fixture.confirmation());
        if let Some(process) = &mut pending {
            process.wait_prompt();
        }
        let mut state = fixture.state();
        state["handled"]["p1"][field] = json!(value);
        fs::write(fixture.state_path(), serde_json::to_vec(&state).unwrap()).unwrap();
        let original = fs::read(fixture.state_path()).unwrap();
        let live = fixture.live();
        let output = if let Some(mut process) = pending {
            process.answer(b"n");
            process.wait()
        } else {
            // Preload input so rejection before the prompt cannot race a pipe write.
            let answer = fixture.directory.path().join("answer");
            fs::write(&answer, b"n").unwrap();
            let mut command = fixture.command("confirm");
            command
                .env("LPG_RECOVERY_TOKEN", "p1")
                .env("LPG_SESSION_KEY", fixture.key());
            Process::spawn_with_input(
                command,
                fixture.directory.path(),
                Stdio::from(File::open(answer).unwrap()),
            )
            .wait()
        };
        let close_calls = fixture.server.calls(&format!("{kind}.close"));
        let state_unchanged = fs::read(fixture.state_path()).unwrap() == original;
        assert!(
            !output.status.success() && close_calls.is_empty() && state_unchanged && fixture.live() == live,
            "{kind} {field} mismatch, prompt_pending={prompt_pending}: status={}, closes={}, state_unchanged={state_unchanged}",
            output.status, close_calls.len()
        );
    }
}

#[test]
fn confirmation_rejects_inconsistent_workspace_identity() {
    assert_inconsistent_confirmation_rejected("workspace", "id", "other-workspace");
}

#[test]
fn confirmation_rejects_inconsistent_tab_identity() {
    assert_inconsistent_confirmation_rejected("tab", "id", "other-tab");
}

#[test]
fn confirmation_rejects_inconsistent_tab_workspace_identity() {
    assert_inconsistent_confirmation_rejected("tab", "workspace_id", "other-workspace");
}

#[test]
fn confirmation_does_not_discard_added_panes() {
    for kind in ["workspace", "tab"] {
        let fixture = Fixture::new();
        fixture.ask();
        fixture.restore(kind);
        fixture.mutate(|live| {
            let mut extra = live["panes"][0].clone();
            extra["pane_id"] = json!("additional");
            live["panes"].as_array_mut().unwrap().push(extra);
        });
        let mut confirmation = fixture.confirmation();
        confirmation.answer(b"n");
        confirmation.wait().success();
        assert_eq!(fixture.live()["panes"].as_array().unwrap().len(), 2);
        assert!(fixture.server.calls(&format!("{kind}.close")).is_empty());
        assert_eq!(fixture.state()["handled"]["p1"]["confirmed"], true);
    }
}

#[test]
fn confirmation_yes_enter_eof_and_invalid_key_keep_restoration() {
    for answer in [b"y".as_slice(), b"Y", b"\n", b"\r", b"", b"?y"] {
        let fixture = Fixture::new();
        fixture.ask();
        fixture.restore("workspace");
        let mut confirmation = fixture.confirmation();
        confirmation.answer(answer);
        let output = confirmation.wait();
        output.success();
        assert_eq!(fixture.live()["panes"].as_array().unwrap().len(), 1);
        assert_eq!(fixture.state()["handled"]["p1"]["confirmed"], true);
        assert!(fixture.server.calls("workspace.close").is_empty());
        if answer == b"?y" {
            assert_eq!(output.stdout.matches("Keep it? [Y/n]").count(), 2);
        }
    }
}

#[test]
fn prompt_sanitizes_controls_and_pending_confirmation_rejects_new_generation() {
    let fixture = Fixture::new();
    fixture.ask();
    fixture.mutate(|live| {
        live["workspaces"][0]["label"] = json!("Normal xyz ABC 012\u{1b}\n\r\t\u{7f} end")
    });
    fixture.restore("workspace");
    let original = fs::read(fixture.state_path()).unwrap();
    let mut confirmation = fixture.confirmation();
    let prompt = confirmation.wait_prompt();
    assert!(prompt.contains("Normal xyz ABC 012????? end"));
    assert!(!prompt.contains('\u{1b}'));
    assert!(!prompt.contains('\u{7f}'));
    assert!(!fixture.state_path().parent().unwrap().join("lock").exists());
    // Keep the old socket inode allocated so the replacement cannot reuse it.
    fs::rename(
        &fixture.server.socket,
        fixture.directory.path().join("old.sock"),
    )
    .unwrap();
    let mut replacement = Server::new(fixture.server.socket.clone(), fixture.directory.path());
    confirmation.answer(b"n");
    confirmation.wait().success();
    assert_eq!(fixture.live()["panes"].as_array().unwrap().len(), 1);
    assert!(fixture.server.calls("workspace.close").is_empty());
    assert!(replacement.model.lock().unwrap().calls.is_empty());
    assert_eq!(fs::read(fixture.state_path()).unwrap(), original);
    replacement.stop();
}

#[test]
fn socket_sessions_isolate_state_and_confirmation_context() {
    let first = Fixture::new();
    first.restore("workspace");
    let original = fs::read(first.state_path()).unwrap();
    let mut second = Fixture::new();
    second.state_root = first.state_root.clone();
    second.refresh();
    assert_ne!(first.key(), second.key());
    assert_eq!(second.state()["handled"], json!({}));
    assert_eq!(second.state()["claims"], json!({}));
    assert_eq!(fs::read(first.state_path()).unwrap(), original);
    let mut command = second.command("confirm");
    command
        .env("LPG_RECOVERY_TOKEN", "p1")
        .env("LPG_SESSION_KEY", first.key());
    assert!(!second.spawn(command).wait().status.success());
    assert!(second.server.calls("workspace.close").is_empty());
    assert_eq!(fs::read(first.state_path()).unwrap(), original);
}

#[test]
fn shutdown_without_socket_is_successful_silent_and_does_not_call_api() {
    let mut fixture = Fixture::new();
    fixture.refresh();
    let original = fs::read(fixture.state_path()).unwrap();
    fixture.server.stop();
    fs::remove_file(&fixture.server.socket).unwrap();
    let before = fixture.server.model.lock().unwrap().calls.len();
    let output = fixture.fire("pane.exited", "p1");
    assert!(output.stderr.is_empty());
    assert_eq!(fixture.server.model.lock().unwrap().calls.len(), before);
    assert_eq!(fs::read(fixture.state_path()).unwrap(), original);
}

#[test]
fn ambiguous_create_responses_retain_claim_for_duplicate_and_distinct_panes() {
    for kind in ["workspace", "tab"] {
        for fault in [
            CreateFault::Malformed,
            CreateFault::Truncated,
            CreateFault::Lost,
            CreateFault::MissingRootPane,
            CreateFault::InconsistentIds,
        ] {
            let fixture = Fixture::new();
            fixture.mutate(|live| {
                let mut pane = live["panes"][0].clone();
                pane["pane_id"] = json!("p2");
                live["panes"].as_array_mut().unwrap().push(pane);
            });
            fixture.refresh();
            fixture.vanish(kind);
            fixture.server.model.lock().unwrap().create_fault = Some(fault);
            let output = fixture.event("pane.exited", "p1").wait();
            assert!(!output.status.success(), "{kind}: {fault:?}");
            assert_eq!(fixture.state()["handled"]["p1"]["status"], "claimed");
            let id = if kind == "workspace" { "w1" } else { "t1" };
            assert_eq!(fixture.state()["claims"][format!("{kind}:{id}")], true);
            fixture.server.model.lock().unwrap().create_fault = None;
            fixture.fire("pane.exited", "p1");
            fixture.fire("pane.exited", "p2");
            assert_eq!(fixture.live()["counter"], 1);
            assert_eq!(fixture.server.calls(&format!("{kind}.create")).len(), 1);
            assert!(!fixture.state_path().parent().unwrap().join("lock").exists());
        }
    }
}

#[test]
fn terminal_confirmation_accepts_single_keys_and_restores_terminal_attributes() {
    for answer in *b"y\rn" {
        let fixture = Fixture::new();
        fixture.ask();
        fixture.restore("workspace");
        let mut terminal = Terminal::new();
        let mut process = terminal.confirmation(&fixture);
        terminal.wait_raw(&mut process);
        // A single key without an appended newline must complete raw-mode input.
        terminal.master.write_all(&[answer]).unwrap();
        process.wait().success();
        terminal.assert_restored();
        assert_eq!(fixture.state()["handled"]["p1"]["confirmed"], true);
        assert_eq!(
            fixture.server.calls("workspace.close").len(),
            usize::from(answer == b'n')
        );
        assert_eq!(
            fixture.live()["panes"].as_array().unwrap().len(),
            usize::from(answer != b'n')
        );
    }
}

#[test]
fn sigterm_restores_terminal_attributes_and_preserves_pending_confirmation() {
    let fixture = Fixture::new();
    fixture.ask();
    fixture.restore("workspace");
    let original = fs::read(fixture.state_path()).unwrap();
    let terminal = Terminal::new();
    let mut process = terminal.confirmation(&fixture);
    terminal.wait_raw(&mut process);
    let pid = libc::pid_t::try_from(process.child.id()).unwrap();
    let started = Instant::now();
    // SAFETY: pid identifies this test's live owned child, not a process group.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let output = process.wait();
    assert_eq!(output.status.code(), Some(128 + libc::SIGTERM));
    assert!(started.elapsed() < Duration::from_secs(2));
    terminal.assert_restored();
    assert_eq!(fs::read(fixture.state_path()).unwrap(), original);
    assert!(fixture.server.calls("workspace.close").is_empty());
    assert!(!fixture.state_path().parent().unwrap().join("lock").exists());
}

#[test]
fn sigterm_releases_owned_state_lock_during_pending_socket_response() {
    let mut fixture = Fixture::new();
    fixture.refresh();
    let original = fs::read(fixture.state_path()).unwrap();
    let before = fixture.server.calls("workspace.list").len();
    fixture.server.model.lock().unwrap().stall_method = Some("workspace.list".into());
    let mut process = fixture.spawn(fixture.command("refresh"));
    let deadline = Instant::now() + DEADLINE;
    while fixture.server.calls("workspace.list").len() == before {
        assert!(process.child.try_wait().unwrap().is_none());
        assert!(
            Instant::now() < deadline,
            "refresh did not reach pending socket request"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let lock = fixture.state_path().parent().unwrap().join("lock");
    assert!(lock.is_dir());
    let pid = libc::pid_t::try_from(process.child.id()).unwrap();
    let started = Instant::now();
    // SAFETY: pid identifies this test's live owned child, not a process group.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let output = process.wait();
    assert_eq!(output.status.code(), Some(128 + libc::SIGTERM));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!lock.exists());
    assert_eq!(fs::read(fixture.state_path()).unwrap(), original);
    assert!(fs::read_dir(fixture.state_path().parent().unwrap())
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".state.")));
    fixture.server.stop();
}

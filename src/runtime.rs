use crate::{config::Config, log, plan, state::Store, transport::Client, Error, Result};
use serde_json::{json, Value};
use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PLUGIN_ID: &str = "postrednik.last-pane-guard";

fn required_env(name: &str) -> Result<String> {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(Error::Invalid(
            "required hook environment is missing or invalid",
        ))
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or(Error::Invalid("missing or invalid object identity"))
}

fn now() -> Result<i64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Invalid("system clock precedes Unix epoch"))?;
    i64::try_from(elapsed.as_secs()).map_err(|_| Error::Invalid("system clock is out of range"))
}

fn objects(result: &Value, key: &str, id: &str) -> Result<Vec<Value>> {
    let values = result[key]
        .as_array()
        .ok_or(Error::Invalid("invalid Herdr list response"))?;
    for object in values {
        text(object, id)?;
    }
    Ok(values.clone())
}

struct Runtime {
    store: Store,
    client: Client,
    config: Config,
}

impl Runtime {
    fn from_env() -> Result<Self> {
        let socket = PathBuf::from(required_env("HERDR_SOCKET_PATH")?);
        let root = PathBuf::from(required_env("HERDR_PLUGIN_STATE_DIR")?);
        let config_dir = PathBuf::from(required_env("HERDR_PLUGIN_CONFIG_DIR")?);
        let store = Store::new(&root, socket.clone())?;
        let config = Config::load(&config_dir.join("config.env"))?;
        let client = Client::new(socket, store.generation.clone());
        Ok(Self {
            store,
            client,
            config,
        })
    }

    fn refresh(&self, state: &mut Value) -> Result<()> {
        let mut workspaces = objects(
            &self.client.call("workspace.list", json!({}))?,
            "workspaces",
            "workspace_id",
        )?;
        let panes = objects(
            &self.client.call("pane.list", json!({}))?,
            "panes",
            "pane_id",
        )?;
        let mut tabs = Vec::new();
        for (index, workspace) in workspaces.iter_mut().enumerate() {
            let id = text(workspace, "workspace_id")?.to_owned();
            if !workspace["label"].is_string() || !workspace["focused"].is_boolean() {
                return Err(Error::Invalid("invalid workspace snapshot"));
            }
            workspace["index"] = json!(index);
            let response = match self.client.call("tab.list", json!({"workspace_id":id})) {
                Ok(response) => response,
                Err(Error::Api(code)) if code == "workspace_not_found" => continue,
                Err(error) => return Err(error),
            };
            for (tab_index, mut tab) in objects(&response, "tabs", "tab_id")?
                .into_iter()
                .enumerate()
            {
                if text(&tab, "workspace_id")? != id || !tab["label"].is_string() {
                    return Err(Error::Invalid("invalid tab snapshot"));
                }
                tab["index"] = json!(tab_index);
                tabs.push(tab);
            }
        }
        for pane in &panes {
            text(pane, "workspace_id")?;
            text(pane, "tab_id")?;
        }
        // Do not remove absent metadata: an exit hook may still be waiting on the lock.
        for workspace in workspaces {
            let id = text(&workspace, "workspace_id")?.to_owned();
            state["workspaces"][id] = workspace;
        }
        for tab in tabs {
            let id = text(&tab, "tab_id")?.to_owned();
            state["tabs"][id] = tab;
        }
        for mut pane in panes {
            let id = text(&pane, "pane_id")?.to_owned();
            let ws = text(&pane, "workspace_id")?.to_owned();
            let tab = text(&pane, "tab_id")?.to_owned();
            pane["workspace_snapshot"] = state["workspaces"][ws].clone();
            pane["tab_snapshot"] = state["tabs"][tab].clone();
            state["panes"][id] = pane;
        }
        self.store.save(state)
    }

    fn lookup(&self, kind: &str, id: &str) -> Result<Option<Value>> {
        let field = format!("{kind}_id");
        match self
            .client
            .call(&format!("{kind}.get"), json!({field.clone():id}))
        {
            Ok(result) => {
                let object = &result[kind];
                if !object.is_object() || text(object, &field)? != id {
                    return Err(Error::Invalid("invalid Herdr lookup response"));
                }
                Ok(Some(object.clone()))
            }
            Err(Error::Api(code)) if code == format!("{kind}_not_found") => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn notify(&self, title: &str, body: &str) {
        if self
            .client
            .call("notification.show", json!({"title":title,"body":body}))
            .is_err()
        {
            log("notification could not be delivered");
        }
    }

    fn exit(&self, state: &mut Value, pane_id: &str) -> Result<()> {
        let cached = &state["panes"][pane_id];
        if cached.is_null() {
            log("exited pane missing from snapshot; skipped");
            return Ok(());
        }
        let ws = text(cached, "workspace_id")?.to_owned();
        let tab = text(cached, "tab_id")?.to_owned();
        let mut pane_live = None;
        for _ in 0..5 {
            crate::check_interrupted()?;
            pane_live = self.lookup("pane", pane_id)?;
            if pane_live.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let workspace_live = self.lookup("workspace", &ws)?;
        let tab_live = self.lookup("tab", &tab)?;
        let decision = plan::decide(
            &json!({"pane_id":pane_id,"now":now()?,"state":state,"config":self.config,
            "live":{"workspaces":workspace_live.into_iter().collect::<Vec<_>>(),
                "tabs":tab_live.into_iter().collect::<Vec<_>>(),"panes":pane_live.into_iter().collect::<Vec<_>>()}}),
        );
        match decision["action"].as_str() {
            Some("restore") => self.restore(state, pane_id, &decision),
            Some("loop_guard") => {
                state["handled"][pane_id] = json!({"status":"loop_guard"});
                self.store.save(state)?;
                log("rapid replacement shell exit; loop guard prevented another restore");
                self.notify(
                    "Last Pane Guard stopped a restore loop",
                    "The replacement shell exited too quickly.",
                );
                Ok(())
            }
            Some("skip") => {
                let reason = text(&decision, "reason")?;
                // Reasons originate in the pure planner, not in untrusted configuration.
                log(&format!("exit skipped: {reason}"));
                if reason != "already_handled" {
                    state["handled"][pane_id] = json!({"status":reason});
                    self.store.save(state)?;
                }
                Ok(())
            }
            _ => Err(Error::Invalid("invalid restoration plan")),
        }
    }

    fn restore(&self, state: &mut Value, old_pane: &str, decision: &Value) -> Result<()> {
        let kind = text(decision, "kind")?;
        if !matches!(kind, "workspace" | "tab") {
            return Err(Error::Invalid("invalid recovery kind"));
        }
        let label = decision["label"]
            .as_str()
            .ok_or(Error::Invalid("invalid saved label"))?;
        let focused = decision["focused"]
            .as_bool()
            .ok_or(Error::Invalid("invalid saved focus"))?;
        let index = decision["index"]
            .as_u64()
            .ok_or(Error::Invalid("invalid saved position"))?;
        let old_ws = text(decision, "workspace_id")?;
        let old_tab = text(decision, "tab_id")?;
        let mut params = json!({"label":label,"focus":focused});
        if kind == "tab" {
            params["workspace_id"] = json!(old_ws);
        }
        if let Some(cwd) = decision["cwd"]
            .as_str()
            .filter(|cwd| !cwd.is_empty() && Path::new(cwd).is_dir())
        {
            params["cwd"] = json!(cwd);
        } else {
            log("last known directory is unavailable; using the default shell directory");
        }
        let claimed_id = if kind == "workspace" { old_ws } else { old_tab };
        state["handled"][old_pane] = json!({"status":"claimed"});
        state["claims"][format!("{kind}:{claimed_id}")] = json!(true);
        self.store.save(state)?;
        let created = self.client.call(&format!("{kind}.create"), params)?;
        let pane = text(&created["root_pane"], "pane_id")?;
        let tab = text(&created["tab"], "tab_id")?;
        let ws = if kind == "workspace" {
            text(&created["workspace"], "workspace_id")?
        } else {
            old_ws
        };
        if text(&created["root_pane"], "workspace_id")? != ws
            || text(&created["root_pane"], "tab_id")? != tab
            || text(&created["tab"], "workspace_id")? != ws
        {
            return Err(Error::Invalid(
                "inconsistent restoration response; claim retained",
            ));
        }
        let id = if kind == "workspace" { ws } else { tab };
        state["panes"][pane] = created["root_pane"].clone();
        state["tabs"][tab] = created["tab"].clone();
        state["tabs"][tab]["index"] = json!(if kind == "tab" { index } else { 0 });
        if kind == "workspace" {
            state["workspaces"][ws] = created["workspace"].clone();
            state["workspaces"][ws]["index"] = json!(index);
        }
        state["restored"][pane] = json!(now()?);
        state["handled"][old_pane] = json!({"status":"restored","kind":kind,"id":id,
            "pane_id":pane,"workspace_id":ws,"tab_id":tab,"label":label,"confirmed":false});
        self.store.save(state)?;
        if self
            .client
            .call(
                &format!("{kind}.move"),
                json!({format!("{kind}_id"):id,"insert_index":index}),
            )
            .is_err()
        {
            log("position could not be restored; restored shell kept");
        }
        if kind == "workspace" {
            let tab_label = decision["tab_label"].as_str().unwrap_or("");
            let number = state["tabs"][old_tab]["number"].to_string();
            if !tab_label.is_empty()
                && tab_label != number
                && self
                    .client
                    .call("tab.rename", json!({"tab_id":tab,"label":tab_label}))
                    .is_err()
            {
                log("restored workspace kept, but its tab label could not be restored");
            }
        }
        log(&format!("{kind} restored after shell exit"));
        if decision["mode"] == "auto" {
            self.notify(&format!("{kind} {label} restored"), "closed by shell exit");
        } else {
            let popup = self.client.call(
                "plugin.pane.open",
                json!({"plugin_id":PLUGIN_ID,"entrypoint":"confirm","focus":true,
                "env":{"LPG_RECOVERY_TOKEN":old_pane,"LPG_SESSION_KEY":self.store.key}}),
            );
            if let Err(error) = popup {
                if matches!(error, Error::Api(ref code) if code == "ui_busy") {
                    log("confirmation UI busy; restored shell kept");
                } else {
                    log("confirmation popup unavailable; restored shell kept");
                }
                self.notify(
                    &format!("{kind} {label} restored"),
                    "closed by shell exit; confirmation unavailable, kept",
                );
            }
        }
        Ok(())
    }

    fn recovery(&self, state: &Value, token: &str) -> Option<Value> {
        let recovery = &state["handled"][token];
        (state["generation"] == self.store.generation
            && recovery["status"] == "restored"
            && recovery["confirmed"] == false)
            .then(|| recovery.clone())
    }

    fn validate_recovery(state: &Value, recovery: &Value) -> Result<()> {
        let kind = text(recovery, "kind")?;
        let id = text(recovery, "id")?;
        let ws = text(recovery, "workspace_id")?;
        let tab = text(recovery, "tab_id")?;
        let pane = text(recovery, "pane_id")?;
        let target = match kind {
            "workspace" => ws,
            "tab" => tab,
            _ => return Err(Error::Invalid("invalid recovery kind")),
        };
        let saved_pane = &state["panes"][pane];
        if id != target
            || text(saved_pane, "pane_id")? != pane
            || text(saved_pane, "workspace_id")? != ws
            || text(saved_pane, "tab_id")? != tab
        {
            return Err(Error::Invalid(
                "inconsistent recovery identity; confirmation refused",
            ));
        }
        Ok(())
    }

    fn confirm(&self) -> Result<()> {
        let token = required_env("LPG_RECOVERY_TOKEN")?;
        if required_env("LPG_SESSION_KEY")? != self.store.key {
            return Err(Error::Invalid("confirmation has no valid recovery context"));
        }
        let initial = {
            let _lock = self.store.lock()?;
            let Some(state) = self.store.load()? else {
                return Ok(());
            };
            let Some(recovery) = self.recovery(&state, &token) else {
                return Ok(());
            };
            Self::validate_recovery(&state, &recovery)?;
            recovery
        };
        let kind = text(&initial, "kind")?;
        if !matches!(kind, "workspace" | "tab") {
            return Err(Error::Invalid("invalid recovery kind"));
        }
        let label: String = initial["label"]
            .as_str()
            .unwrap_or("")
            .chars()
            .map(|c| {
                if c <= '\u{1f}' || c == '\u{7f}' {
                    '?'
                } else {
                    c
                }
            })
            .collect();
        let mut output = std::io::stdout().lock();
        writeln!(
            output,
            "\n{kind} {label} was closed by a shell exit and has been restored."
        )?;
        let close = loop {
            write!(output, "Keep it? [Y/n] ")?;
            output.flush()?;
            let answer = read_key()?;
            writeln!(output)?;
            match answer {
                None | Some(b'\n' | b'\r' | b'y' | b'Y') => break false,
                Some(b'n' | b'N') => break true,
                _ => (),
            }
        };
        drop(output);
        let _lock = self.store.lock()?;
        let Some(mut state) = self.store.load()? else {
            return Ok(());
        };
        let Some(recovery) = self.recovery(&state, &token) else {
            return Ok(());
        };
        Self::validate_recovery(&state, &recovery)?;
        if recovery != initial {
            return Err(Error::Invalid(
                "recovery changed while confirmation was pending",
            ));
        }
        let kind = text(&recovery, "kind")?;
        if !matches!(kind, "workspace" | "tab") {
            return Err(Error::Invalid("invalid recovery kind"));
        }
        let id = text(&recovery, "id")?;
        let ws = text(&recovery, "workspace_id")?;
        let pane = text(&recovery, "pane_id")?;
        let mut should_close = close;
        if close {
            let panes = objects(
                &self.client.call("pane.list", json!({}))?,
                "panes",
                "pane_id",
            )?;
            for live in &panes {
                text(live, "workspace_id")?;
                text(live, "tab_id")?;
            }
            let matching: Vec<_> = panes
                .iter()
                .filter(|live| {
                    if kind == "workspace" {
                        live["workspace_id"] == ws
                    } else {
                        live["tab_id"] == id
                    }
                })
                .collect();
            should_close = matching.len() == 1
                && matching[0]["pane_id"] == pane
                && matching[0]["workspace_id"] == ws
                && matching[0]["tab_id"] == recovery["tab_id"];
            if !should_close {
                log("restored object changed or disappeared; confirmation will not close it");
            }
        }
        state["handled"][&token]["confirmed"] = json!(true);
        self.store.save(&state)?;
        if should_close {
            self.client
                .call(&format!("{kind}.close"), json!({format!("{kind}_id"):id}))?;
            log("restored object closed by confirmation");
        }
        Ok(())
    }
}

pub fn run(mode: &str) -> Result<()> {
    let runtime = Runtime::from_env()?;
    if mode == "confirm" {
        return runtime.confirm();
    }
    let _lock = runtime.store.lock()?;
    let mut state = runtime.store.prepare()?;
    if mode == "refresh" {
        return runtime.refresh(&mut state);
    }
    if mode != "event" {
        return Err(Error::Invalid(
            "usage: last-pane-guard refresh|event|confirm",
        ));
    }
    let event = required_env("HERDR_PLUGIN_EVENT")?;
    if event == "pane.exited"
        || matches!(
            event.as_str(),
            "workspace.closed" | "tab.closed" | "pane.closed"
        )
    {
        let payload: Value = serde_json::from_str(&required_env("HERDR_PLUGIN_EVENT_JSON")?)
            .map_err(|_| Error::Invalid("invalid hook event JSON"))?;
        let kind = event
            .split('.')
            .next()
            .ok_or(Error::Invalid("invalid event name"))?;
        let id = text(&payload["data"], &format!("{kind}_id"))?;
        if event == "pane.exited" {
            return runtime.exit(&mut state, id);
        }
        state["closed"][format!("{kind}s")][id] = json!(true);
        runtime.store.save(&state)?;
    } else if !matches!(
        event.as_str(),
        "workspace.created"
            | "workspace.renamed"
            | "workspace.moved"
            | "workspace.reordered"
            | "workspace.focused"
            | "tab.created"
            | "tab.renamed"
            | "tab.moved"
            | "tab.focused"
            | "pane.created"
            | "pane.focused"
            | "pane.moved"
    ) {
        log("unsupported event ignored");
        return Ok(());
    }
    runtime.refresh(&mut state)
}

#[cfg(unix)]
fn read_key() -> Result<Option<u8>> {
    struct Terminal(Option<libc::termios>);
    impl Drop for Terminal {
        fn drop(&mut self) {
            if let Some(saved) = &self.0 {
                // SAFETY: saved termios came from this descriptor and remains live for the call.
                if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, saved) } != 0 {
                    log("could not restore terminal mode");
                }
            }
        }
    }
    let mut terminal = Terminal(None);
    // SAFETY: isatty only inspects the standard input descriptor.
    if unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
        // SAFETY: zero-initialized storage is populated by tcgetattr before being used.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: saved points to writable termios-sized storage.
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut saved) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut mode = saved;
        mode.c_lflag &= !(libc::ICANON | libc::ECHO);
        mode.c_cc[libc::VMIN] = 1;
        mode.c_cc[libc::VTIME] = 0;
        // SAFETY: mode is a valid termios copied from this terminal, with local flags adjusted.
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &mode) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        terminal.0 = Some(saved);
    }
    loop {
        crate::check_interrupted()?;
        let mut descriptor = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: descriptor is valid writable storage for one pollfd, with finite timeout.
        let ready = unsafe { libc::poll(&mut descriptor, 1, 100) };
        if ready == 0 {
            continue;
        }
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        crate::check_interrupted()?;
        let mut byte = 0u8;
        // SAFETY: byte is writable storage of exactly the requested one byte.
        let count = unsafe { libc::read(libc::STDIN_FILENO, (&mut byte as *mut u8).cast(), 1) };
        match count {
            0 => return Ok(None),
            1 => return Ok(Some(byte)),
            _ => {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error.into());
                }
            }
        }
    }
}

/// Read one key from a console without line buffering or echo, restoring the mode.
/// Noninteractive input is read unchanged. Forced termination cannot restore it.
#[cfg(windows)]
fn read_key() -> Result<Option<u8>> {
    use std::io::Read;
    use std::os::windows::io::{AsRawHandle, RawHandle};

    const ENABLE_LINE_INPUT: u32 = 0x0002;
    const ENABLE_ECHO_INPUT: u32 = 0x0004;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetConsoleMode(console: RawHandle, mode: *mut u32) -> i32;
        fn SetConsoleMode(console: RawHandle, mode: u32) -> i32;
    }

    struct Console(RawHandle, Option<u32>);
    impl Drop for Console {
        fn drop(&mut self) {
            if let Some(saved) = self.1 {
                // SAFETY: the handle is standard input, still open for this process.
                if unsafe { SetConsoleMode(self.0, saved) } == 0 {
                    log("could not restore terminal mode");
                }
            }
        }
    }

    let stdin = std::io::stdin();
    let mut console = Console(stdin.as_raw_handle(), None);
    let mut saved = 0_u32;
    // SAFETY: saved is writable storage for one u32; failure means not a console.
    if unsafe { GetConsoleMode(console.0, &mut saved) } != 0 {
        // SAFETY: the mode is the saved console mode with line input and echo removed.
        if unsafe { SetConsoleMode(console.0, saved & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT)) }
            == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        console.1 = Some(saved);
    }
    let mut byte = [0_u8; 1];
    loop {
        crate::check_interrupted()?;
        match stdin.lock().read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => return Ok(Some(byte[0])),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn read_key() -> Result<Option<u8>> {
    Err(Error::Invalid(
        "native terminal input is not implemented on this platform",
    ))
}

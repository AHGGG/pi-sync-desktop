//! Blocking I/O lives on one worker. The native view receives presentation only.
use crate::ui_model::{Change, Choice, Command, ConfigItem, Device, Event, Snapshot};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use sync_core::{
    connection::ConnectionConfig,
    profiles::{Profile, ProfileStore},
    transport::{AuthAnswer, AuthPrompt, SshTransport},
};
use zeroize::Zeroizing;

const WORKER: &str = include_str!("../../../backend/worker.mjs");
const PUBLIC_ID: &str = "public-harness";
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    source: String,
    targets: Vec<String>,
    items: Vec<String>,
}
struct Engine {
    child: Child,
    input: ChildStdin,
    output: Receiver<Result<Value, ()>>,
    _runtime: tempfile::TempDir,
}
impl Engine {
    fn spawn() -> Result<Self> {
        let runtime = tempfile::Builder::new()
            .prefix("pi-sync-runtime-")
            .tempdir()?;
        for (name, source) in [
            ("bridge.mjs", include_str!("../../../backend/bridge.mjs")),
            ("engine.mjs", include_str!("../../../backend/engine.mjs")),
            ("worker.mjs", WORKER),
            (
                "documents.mjs",
                include_str!("../../../backend/documents.mjs"),
            ),
            ("merge.mjs", include_str!("../../../backend/merge.mjs")),
            (
                "public-source.mjs",
                include_str!("../../../backend/public-source.mjs"),
            ),
        ] {
            fs::write(runtime.path().join(name), source)?;
        }
        let mut command = std::process::Command::new("node");
        command
            .arg(runtime.path().join("bridge.mjs"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW: worker is not a terminal UI.
        }
        let mut child = command
            .spawn()
            .context("Node.js 20 or later is required. Install it, then reopen Pi Sync")?;
        let input = child.stdin.take().context("Cannot start sync input")?;
        let stdout = child.stdout.take().context("Cannot start sync output")?;
        let (tx, output) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut bytes = Zeroizing::new(Vec::new());
                // Bounded reads prevent a broken subprocess exhausting the host.
                let result = (&mut reader)
                    .take(16 * 1024 * 1024 + 1)
                    .read_until(b'\n', &mut bytes);
                match result {
                    Ok(0) | Err(_) => break,
                    Ok(n) if n > 16 * 1024 * 1024 => {
                        let _ = tx.send(Err(()));
                        break;
                    }
                    Ok(_) => {
                        let value = serde_json::from_slice(&bytes).map_err(|_| ());
                        if tx.send(value).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let mut engine = Self {
            child,
            input,
            output,
            _runtime: runtime,
        };
        engine.send(&json!({"id":"startup","op":"ping"}))?;
        let version = engine
            .output
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| {
                anyhow::anyhow!("The sync worker could not start. Node.js 20 or later is required")
            })?
            .map_err(|_| anyhow::anyhow!("Invalid sync worker startup response"))?;
        let major = version["node"]
            .as_str()
            .and_then(|v| v.split('.').next())
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0);
        ensure!(
            version["ok"] == true && major >= 20,
            "Node.js 20 or later is required"
        );
        Ok(engine)
    }
    fn send(&mut self, value: &Value) -> Result<()> {
        let mut bytes = Zeroizing::new(serde_json::to_vec(value)?);
        bytes.push(b'\n');
        self.input.write_all(&bytes)?;
        self.input.flush()?;
        Ok(())
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Backend {
    commands: Receiver<Command>,
    events: Sender<Event>,
    pending: VecDeque<Command>,
    engine: Option<Engine>,
    profiles: Vec<Profile>,
    store: ProfileStore,
    config_dir: PathBuf,
    sessions: HashMap<String, SshTransport>,
    connection_errors: HashMap<String, String>,
    snapshot: Snapshot,
    last: Option<Selection>,
    selection: Option<Selection>,
    plan: Option<Value>,
    changes: HashMap<String, (String, String, String)>,
    request: u64,
}

pub(crate) fn start() -> (Sender<Command>, Receiver<Event>) {
    let (commands_tx, commands) = mpsc::channel();
    let (events, events_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let path = match ProfileStore::default_path() {
            Ok(v) => v,
            Err(_) => {
                let _ = events.send(Event::Error(
                    "Cannot locate your application data directory".into(),
                ));
                return;
            }
        };
        let config_dir = path.parent().unwrap().to_path_buf();
        let store = ProfileStore::new(path);
        let profiles = match store.load() {
            Ok(v) => v,
            Err(_) => {
                let _ = events.send(Event::Error(
                    "Saved devices could not be read. The file has not been changed".into(),
                ));
                vec![]
            }
        };
        let last = read_selection(&config_dir.join("last-selection.json"))
            .ok()
            .flatten();
        let mut state = Backend {
            commands,
            events,
            pending: VecDeque::new(),
            engine: None,
            profiles,
            store,
            config_dir,
            sessions: HashMap::new(),
            connection_errors: HashMap::new(),
            snapshot: Snapshot::default(),
            last,
            selection: None,
            plan: None,
            changes: HashMap::new(),
            request: 0,
        };
        state.snapshot.source_id = Some(PUBLIC_ID.into());
        state.publish();
        loop {
            let command = match state
                .pending
                .pop_front()
                .or_else(|| state.commands.recv().ok())
            {
                Some(v) => v,
                None => break,
            };
            if let Err(error) = state.handle(command) {
                // Error text here is controlled adapter context, never raw remote stderr or file content.
                state.invalidate();
                let _ = state.events.send(Event::Error(error.to_string()));
                state.publish();
            }
        }
    });
    (commands_tx, events_rx)
}
impl Backend {
    fn publish(&mut self) {
        self.snapshot.devices = vec![
            Device {
                id: PUBLIC_ID.into(),
                name: "Public harness".into(),
                address: "AHGGG/my-pi-harness · latest public preset".into(),
                status: "Read-only source".into(),
                is_local: false,
            },
            Device {
                id: "local".into(),
                name: "This computer".into(),
                address: local_root(),
                status: "Local".into(),
                is_local: true,
            },
        ];
        self.snapshot.devices.extend(self.profiles.iter().map(|p| {
            Device {
                id: p.id.clone(),
                name: p.name.clone(),
                address: format!(
                    "{}@{}:{}",
                    p.connection.username, p.connection.host, p.connection.port
                ),
                status: if self.sessions.contains_key(&p.id) {
                    "Connected"
                } else {
                    "Not checked"
                }
                .into(),
                is_local: false,
            }
        }));
        self.snapshot.items = vec![
            ConfigItem { id: "settings".into(), name: "Settings & packages".into(), category: "Pi configuration".into(), detail: "Reviewed portable settings. Device identity, shell, defaults and unrelated values stay local.".into() },
            ConfigItem { id: "subagents".into(), name: "Subagents".into(), category: "Pi configuration".into(), detail: "Concurrency, scheduling and presentation settings.".into() },
            ConfigItem { id: "mcp".into(), name: "MCP configuration".into(), category: "Optional".into(), detail: "Reviewed server fields. Never enabled by selecting settings alone.".into() },
        ];
        if self.snapshot.source_id.as_deref() != Some(PUBLIC_ID) {
            self.snapshot.items.push(ConfigItem { id: "models".into(), name: "Private model definitions".into(), category: "Private · device to device".into(), detail: "Replace after review. Credentials stay private; unresolved default compatibility blocks apply.".into() });
        }
        self.snapshot.has_last_selection = self.last.is_some();
        let _ = self.events.send(Event::Snapshot(self.snapshot.clone()));
    }
    fn invalidate(&mut self) {
        self.plan = None;
        self.snapshot.plan_id = None;
        self.snapshot.command_details_reviewed = false;
        self.snapshot.changes.clear();
        self.changes.clear();
    }
    fn busy(&self, text: &str) {
        let _ = self.events.send(Event::Busy(text.into()));
    }
    fn handle(&mut self, command: Command) -> Result<()> {
        match command {
            Command::Refresh => {
                self.invalidate();
                self.publish();
            }
            Command::AddDevice {
                name,
                ssh_line,
                root,
            } => {
                let connection = ConnectionConfig::parse(&ssh_line)?;
                ensure!(
                    !self
                        .profiles
                        .iter()
                        .any(|p| p.connection.host == connection.host
                            && p.connection.username == connection.username
                            && p.connection.port == connection.port
                            && p.agent_dir == root),
                    "This device and Pi directory are already saved"
                );
                let id = format!(
                    "device-{}",
                    SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
                );
                let profile = Profile {
                    id,
                    name: name.trim().to_owned(),
                    connection,
                    agent_dir: if root.trim().is_empty() {
                        "~/.pi/agent".into()
                    } else {
                        root.trim().into()
                    },
                };
                let mut profiles = self.profiles.clone();
                profiles.push(profile);
                self.store.save(&profiles)?;
                self.profiles = profiles;
                self.invalidate();
                self.publish();
            }
            Command::RemoveDevice(id) => {
                ensure!(
                    id != "local" && id != PUBLIC_ID,
                    "This source cannot be removed"
                );
                let profiles = self
                    .profiles
                    .iter()
                    .filter(|p| p.id != id)
                    .cloned()
                    .collect::<Vec<_>>();
                self.store.save(&profiles)?;
                self.profiles = profiles;
                self.sessions.remove(&id);
                if self.snapshot.source_id.as_ref() == Some(&id) {
                    self.snapshot.source_id = Some(PUBLIC_ID.into());
                }
                self.invalidate();
                self.publish();
            }
            Command::SelectSource(id) => {
                self.source(&id)?;
                self.snapshot.source_id = Some(id);
                self.invalidate();
                self.publish();
            }
            Command::Prepare {
                source,
                targets,
                items,
            } => self.prepare(Selection {
                source,
                targets,
                items,
            })?,
            Command::RepeatLast => {
                let selection = self
                    .last
                    .clone()
                    .context("There is no previous selection yet")?;
                self.prepare(selection)?;
            }
            Command::Apply { plan_id, choices } => self.apply(plan_id, choices)?,
            Command::RevealDetails { plan_id } => {
                ensure!(
                    self.snapshot.plan_id.as_ref() == Some(&plan_id),
                    "The review is no longer current"
                );
                self.busy("Revealing command details locally…");
                let result = self.call(json!({"op":"reveal","planId":plan_id,"approved":true}))?;
                self.set_plan(result)?;
                self.publish();
            }
            Command::Authenticate { secret, .. } => {
                drop(secret);
            }
            Command::TrustHost { .. } | Command::CancelAuthentication { .. } => {}
        }
        Ok(())
    }
    fn target(&self, id: &str) -> Result<Value> {
        if id == "local" {
            return Ok(json!({"id":"local","type":"local","agentDir":local_root()}));
        }
        let profile = self
            .profiles
            .iter()
            .find(|p| p.id == id)
            .context("A selected device is no longer saved")?;
        Ok(json!({"id":profile.id,"type":"ssh","agentDir":profile.agent_dir}))
    }
    fn source(&self, id: &str) -> Result<Value> {
        if id == PUBLIC_ID {
            Ok(json!({"type":"public","latest":true}))
        } else {
            self.target(id)
        }
    }
    fn prepare(&mut self, selection: Selection) -> Result<()> {
        self.invalidate();
        self.snapshot.results.clear();
        self.connection_errors.clear();
        self.snapshot.source_id = Some(selection.source.clone());
        self.busy("Reading source and preparing changes…");
        let source = self.source(&selection.source)?;
        let targets = selection
            .targets
            .iter()
            .map(|id| self.target(id))
            .collect::<Result<Vec<_>>>()?;
        let result = self.call(
            json!({"op":"preview","source":source,"targets":targets,"items":selection.items}),
        )?;
        self.snapshot.selected_item_ids = selection.items.clone();
        self.snapshot.target_ids = selection.targets.clone();
        self.selection = Some(selection.clone());
        self.last = Some(selection);
        write_selection(
            &self.config_dir.join("last-selection.json"),
            self.last.as_ref().unwrap(),
        )?;
        self.set_plan(result)?;
        self.publish();
        Ok(())
    }
    fn set_plan(&mut self, result: Value) -> Result<()> {
        self.snapshot.plan_id = result
            .get("planId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.snapshot.changes.clear();
        self.snapshot.results.clear();
        self.snapshot.command_details_reviewed = result["commandDetailsRevealed"] == true;
        self.changes.clear();
        for target in result
            .get("targets")
            .and_then(Value::as_array)
            .context("Invalid preview response")?
        {
            let id = target["id"]
                .as_str()
                .context("Invalid target response")?
                .to_owned();
            let name = self.device_name(&id);
            let status = target["status"].as_str().unwrap_or("blocked");
            if status == "blocked" {
                self.snapshot.results.push((
                    name.clone(),
                    self.connection_errors.get(&id).cloned().unwrap_or_else(|| {
                        explain(target["error"].as_str().unwrap_or("TARGET_FAILED"))
                    }),
                ));
            }
            if let Some(documents) = target.get("documents").and_then(Value::as_object) {
                for (document, review) in documents {
                    for (kind, is_conflict) in [("changes", false), ("conflicts", true)] {
                        if let Some(changes) = review[kind].as_array() {
                            for change in changes {
                                let path =
                                    change["path"].as_str().context("Invalid change path")?;
                                let change_id = format!("{id}:{document}:{path}");
                                let before = if is_conflict {
                                    &change["target"]
                                } else {
                                    &change["before"]
                                };
                                let after = if is_conflict {
                                    &change["source"]
                                } else {
                                    &change["after"]
                                };
                                self.snapshot.changes.push(Change {
                                    id: change_id.clone(),
                                    device: name.clone(),
                                    path: format!("{document}.json{path}"),
                                    before: display_value(before),
                                    after: display_value(after),
                                    conflict: is_conflict,
                                });
                                self.changes
                                    .insert(change_id, (id.clone(), document.clone(), path.into()));
                            }
                        }
                    }
                }
            }
            if target["models"]["changed"] == true {
                let change_id = format!("{id}:models:/");
                self.snapshot.changes.push(Change {
                    id: change_id.clone(),
                    device: name,
                    path: "models.json · entire private file".into(),
                    before: model_preview(
                        &target["models"]["targetSummary"],
                        target["models"]["targetHash"].as_str().unwrap_or("missing"),
                    ),
                    after: model_preview(
                        &target["models"]["sourceSummary"],
                        target["models"]["sourceHash"].as_str().unwrap_or("unknown"),
                    ),
                    conflict: target["models"]["error"].is_string(),
                });
                if let Some(error) = target["models"]["error"].as_str() {
                    self.snapshot.results.push((
                        self.device_name(&id),
                        format!(
                            "{} · choose Target or Skip for models to continue with other settings",
                            explain(error)
                        ),
                    ));
                }
                self.changes
                    .insert(change_id, (id, "models".into(), "/".into()));
            }
        }
        if let Some(revision) = result.get("sourceRevision").and_then(Value::as_str) {
            self.snapshot
                .results
                .push(("Source revision".into(), revision.into()));
        }
        self.plan = Some(result);
        Ok(())
    }
    fn apply(&mut self, plan_id: String, choices: BTreeMap<String, Choice>) -> Result<()> {
        ensure!(
            self.snapshot.plan_id.as_ref() == Some(&plan_id),
            "The preview is no longer current. Prepare it again"
        );
        ensure!(
            self.snapshot
                .changes
                .iter()
                .filter(|c| c.conflict)
                .all(|c| choices.contains_key(&c.id)),
            "Choose source, target or skip for every conflict"
        );
        ensure!(
            choices.keys().all(|id| self.changes.contains_key(id)),
            "A review choice is stale. Prepare again"
        );
        let mut resolutions = json!({});
        let mut model_choices = json!({});
        for (id, choice) in choices {
            let (target, document, path) =
                self.changes.get(&id).context("Unknown review choice")?;
            let choice = match choice {
                Choice::Source => "source",
                Choice::Target => "target",
                Choice::Skip => "skip",
            };
            if document == "models" {
                model_choices[target] = json!(choice);
            } else {
                resolutions[target][document][path] = json!(choice);
            }
        }
        self.busy("Applying the changes you reviewed…");
        // Resolve against captured snapshots, never silently obtain a new preview.
        let resolved = self.call(json!({"op":"resolve","planId":plan_id,"resolutions":resolutions,"modelChoices":model_choices}))?;
        let ready = resolved["targets"]
            .as_array()
            .context("Invalid resolved plan")?
            .iter()
            .filter(|t| t["status"] == "ready")
            .filter_map(|t| t["id"].as_str())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        ensure!(
            !ready.is_empty(),
            "No target is ready. Review the blocked devices and prepare again"
        );
        let blocked = resolved["targets"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["status"] != "ready")
            .map(|t| {
                let id = t["id"].as_str().unwrap_or("unknown");
                let error =
                    self.connection_errors.get(id).cloned().unwrap_or_else(|| {
                        explain(t["error"].as_str().unwrap_or("TARGET_NOT_READY"))
                    });
                (self.device_name(id), error)
            })
            .collect::<Vec<_>>();
        let new_plan = resolved["planId"]
            .as_str()
            .context("Invalid resolved plan identifier")?;
        let result =
            self.call(json!({"op":"apply","planId":new_plan,"approved":true,"targetIds":ready,"commandReviewApproved":self.snapshot.command_details_reviewed}))?;
        self.invalidate();
        self.snapshot.results = blocked;
        for target in result["targets"]
            .as_array()
            .context("Invalid apply response")?
        {
            let id = target["id"].as_str().unwrap_or("unknown");
            let mut status = match target["status"].as_str() {
                Some("applied") => "Applied · previous versions of changed files backed up".into(),
                Some("unchanged") => "Already up to date".into(),
                _ => format!(
                    "{} · inspect and prepare again before retrying",
                    explain(target["error"].as_str().unwrap_or("TARGET_FAILED"))
                ),
            };
            if let Some(files) = target["files"]
                .as_array()
                .or_else(|| target["applied"].as_array())
            {
                let names = files
                    .iter()
                    .filter_map(|f| f.as_str().or_else(|| f["file"].as_str()))
                    .collect::<Vec<_>>();
                if !names.is_empty() {
                    status.push_str(&format!(" · Files: {}", names.join(", ")));
                }
            }
            if let Some(path) = target["backupPath"].as_str() {
                status.push_str(&format!(" · Backup: {path}"));
            }
            self.snapshot.results.push((self.device_name(id), status));
        }
        self.publish();
        Ok(())
    }
    fn device_name(&self, id: &str) -> String {
        if id == "local" {
            "This computer".into()
        } else {
            self.profiles
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| id.into())
        }
    }
    fn call(&mut self, mut request: Value) -> Result<Value> {
        if self.engine.is_none() {
            self.engine = Some(Engine::spawn()?);
        }
        self.request += 1;
        let id = self.request;
        request["id"] = json!(id);
        self.engine.as_mut().unwrap().send(&request)?;
        loop {
            let value = self
                .engine
                .as_ref()
                .unwrap()
                .output
                .recv_timeout(Duration::from_secs(120));
            let value = match value {
                Ok(Ok(v)) => v,
                _ => {
                    self.engine = None;
                    bail!(
                        "The sync worker stopped or timed out. If applying, inspect target results before preparing again"
                    );
                }
            };
            if value["type"] == "transport" {
                let target_id = value["target"]["id"]
                    .as_str()
                    .context("Invalid transport target")?
                    .to_owned();
                let response = self.remote(&target_id, &value["request"]);
                let message = match response {
                    Ok(response) => {
                        json!({"op":"transport-result","transportId":value["transportId"],"response":response})
                    }
                    Err(error) => {
                        self.connection_errors
                            .insert(target_id.clone(), error.to_string());
                        json!({"op":"transport-result","transportId":value["transportId"],"error":"SSH_FAILED"})
                    }
                };
                self.engine.as_mut().unwrap().send(&message)?;
            } else if value["id"] == id {
                if value["ok"] != true {
                    bail!(
                        "{}",
                        explain(
                            value["error"]
                                .as_str()
                                .or_else(|| value["error"]["code"].as_str())
                                .unwrap_or("SYNC_FAILED")
                        )
                    );
                }
                return Ok(value.get("result").cloned().unwrap_or(value));
            }
        }
    }
    fn remote(&mut self, id: &str, request: &Value) -> Result<Value> {
        if !self.sessions.contains_key(id) {
            let profile = self
                .profiles
                .iter()
                .find(|p| p.id == id)
                .context("Unknown SSH device")?
                .clone();
            let known = self.config_dir.join("known_hosts");
            let session = SshTransport::connect(&profile.connection, &known, |prompt| {
                self.prompt(prompt, &profile.name, &profile.connection)
            })?;
            self.sessions.insert(id.into(), session);
        }
        let result = self
            .sessions
            .get_mut(id)
            .unwrap()
            .run_worker(WORKER, request);
        if result.is_err() {
            self.sessions.remove(id);
        }
        result
    }
    fn prompt(
        &mut self,
        prompt: AuthPrompt,
        device: &str,
        connection: &ConnectionConfig,
    ) -> AuthAnswer {
        self.request += 1;
        let id = format!("auth-{}", self.request);
        let event = match prompt {
            AuthPrompt::TrustHost {
                host,
                port,
                fingerprint,
            } => Event::TrustHost {
                request_id: id.clone(),
                host: format!("{host}:{port}"),
                fingerprint,
            },
            AuthPrompt::Password { username } => Event::Authentication {
                request_id: id.clone(),
                prompt: format!(
                    "SSH password for {username}@{}:{} ({device})",
                    connection.host, connection.port
                ),
            },
            AuthPrompt::KeyPassphrase { key_path } => Event::Authentication {
                request_id: id.clone(),
                prompt: format!(
                    "Key passphrase for {}. Connecting to {}@{}:{} ({device})",
                    key_path.display(),
                    connection.username,
                    connection.host,
                    connection.port
                ),
            },
        };
        if self.events.send(event).is_err() {
            return AuthAnswer::Cancel;
        }
        while let Ok(command) = self.commands.recv() {
            match command {
                Command::TrustHost {
                    request_id,
                    accepted,
                } if request_id == id => return AuthAnswer::Trust(accepted),
                Command::Authenticate { request_id, secret } if request_id == id => {
                    return AuthAnswer::Secret(secret);
                }
                Command::CancelAuthentication { request_id } if request_id == id => {
                    return AuthAnswer::Cancel;
                }
                Command::Authenticate { secret, .. } => drop(secret),
                command => self.pending.push_back(command),
            }
        }
        AuthAnswer::Cancel
    }
}
fn local_root() -> String {
    std::env::var("PI_CODING_AGENT_DIR").unwrap_or_else(|_| "~/.pi/agent".into())
}
fn model_preview(summary: &Value, hash: &str) -> String {
    let mut rows = vec![format!("SHA256 {hash}")];
    if let Some(providers) = summary["providers"].as_object() {
        if providers.is_empty() {
            rows.push("No custom providers".into());
        }
        for (name, provider) in providers {
            let models = provider["models"]
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|v| v["id"].as_str())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            rows.push(format!(
                "{name}: {}",
                if models.is_empty() {
                    "provider configuration; no explicit models".into()
                } else {
                    models.join(", ")
                }
            ));
        }
    } else {
        rows.push("No previous model file".into());
    }
    rows.push("Credentials, endpoints and other private details are hidden. The entire file is replaced if you choose Source.".into());
    rows.join("\n")
}
fn display_value(value: &Value) -> String {
    if value["present"] == false {
        return "Not set".into();
    }
    let value = value.get("value").unwrap_or(value);
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| "Unavailable".into());
    // The engine projects only audited nonsecret leaves before this seam.
    text
}
fn explain(code: &str) -> String {
    match code {
        "DEFAULT_MODEL_UNVERIFIED" => "Blocked: the target default model cannot be verified in the selected models. Defaults were preserved",
        "SOURCE_CHANGED" => "The source changed after review. Prepare a fresh preview",
        "STALE_PLAN" | "STALE_TARGET" | "TARGET_CHANGED" | "STALE_SNAPSHOT" => "The target changed after review. Prepare a fresh preview",
        "PUBLIC_MODELS_FORBIDDEN" => "Private models require a device source",
        "SSH_FAILED" => "Connection or authentication failed; this device was not confirmed updated",
        "PLAN_NOT_FOUND" | "PLAN_EXPIRED" => "This preview expired. Prepare it again",
        "SYMLINK_REJECTED" => "A configuration path is a symbolic link. Sync is blocked for safety",
        "REPOSITORY_TARGET" => "Private device configuration cannot be written inside a Git repository",
        "PUBLIC_FETCH_FAILED" | "PUBLIC_SOURCE_UNAVAILABLE" => "Cannot fetch the public harness. Check the connection and try again",
        _ => return format!("Sync blocked: {code}"),
    }.into()
}
fn read_selection(path: &Path) -> Result<Option<Selection>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() < 64 * 1024,
        "Invalid saved selection"
    );
    Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
}
fn write_selection(path: &Path, selection: &Selection) -> Result<()> {
    let parent = path.parent().context("Missing config directory")?;
    fs::create_dir_all(parent)?;
    ensure!(
        !fs::symlink_metadata(parent)?.file_type().is_symlink(),
        "Refusing symbolic-link configuration directory"
    );
    if let Ok(meta) = fs::symlink_metadata(path) {
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "Refusing symbolic-link selection file"
        );
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&serde_json::to_vec(selection)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remembers_selection_not_secrets_or_plan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selection.json");
        let selection = Selection {
            source: "local".into(),
            targets: vec!["test".into()],
            items: vec!["settings".into()],
        };
        write_selection(&path, &selection).unwrap();
        let restored = read_selection(&path).unwrap().unwrap();
        assert_eq!(restored.source, "local");
        assert_eq!(restored.items, vec!["settings"]);
        assert!(!fs::read_to_string(path).unwrap().contains("plan"));
    }
    fn exchange(engine: &mut Engine, value: Value) -> Value {
        engine.send(&value).unwrap();
        let response = engine
            .output
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
            .unwrap();
        assert_eq!(response["id"], value["id"]);
        response
    }
    #[test]
    fn embedded_bridge_previews_resolves_and_applies_without_touching_other_values() {
        let root = tempfile::tempdir().unwrap();
        // macOS temporary paths can traverse /var -> /private/var. Resolve that
        // fixture alias without weakening the worker's symlink rejection.
        #[cfg(target_os = "macos")]
        let root_path = fs::canonicalize(root.path()).unwrap();
        #[cfg(not(target_os = "macos"))]
        let root_path = root.path().to_path_buf();
        let source = root_path.join("source");
        let target = root_path.join("target");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(source.join("settings.json"), r#"{"theme":"light"}"#).unwrap();
        fs::write(target.join("settings.json"),r#"{"theme":"dark","deviceId":"keep-id","defaultProvider":"builtin","defaultModel":"keep-model","shellPath":"keep-shell","privateValue":"keep"}"#).unwrap();
        let mut engine = Engine::spawn().unwrap();
        let preview = exchange(
            &mut engine,
            json!({"id":1,"op":"preview","source":{"id":"source","type":"local","agentDir":source},"targets":[{"id":"target","type":"local","agentDir":target}],"items":["settings"]}),
        );
        assert_eq!(preview["ok"], true, "fixture preview failed: {preview}");
        assert_eq!(preview["targets"][0]["status"], "conflicts");
        let unchanged: Value =
            serde_json::from_slice(&fs::read(target.join("settings.json")).unwrap()).unwrap();
        assert_eq!(unchanged["theme"], "dark");
        let resolved = exchange(
            &mut engine,
            json!({"id":2,"op":"resolve","planId":preview["planId"],"resolutions":{"target":{"settings":{"/theme":"source"}}}}),
        );
        assert_eq!(resolved["targets"][0]["status"], "ready");
        let applied = exchange(
            &mut engine,
            json!({"id":3,"op":"apply","planId":resolved["planId"],"approved":true,"targetIds":["target"]}),
        );
        assert_eq!(applied["targets"][0]["status"], "applied");
        let saved: Value =
            serde_json::from_slice(&fs::read(target.join("settings.json")).unwrap()).unwrap();
        assert_eq!(saved["theme"], "light");
        assert_eq!(saved["deviceId"], "keep-id");
        assert_eq!(saved["defaultModel"], "keep-model");
        assert_eq!(saved["shellPath"], "keep-shell");
        assert_eq!(saved["privateValue"], "keep");
        let replay = exchange(
            &mut engine,
            json!({"id":4,"op":"apply","planId":resolved["planId"],"approved":true,"targetIds":["target"]}),
        );
        assert_eq!(replay["error"]["code"], "PLAN_NOT_FOUND");
    }
    #[test]
    fn model_review_displays_only_provider_names_and_ids() {
        let summary = json!({"providers":{"custom":{"apiKey":"DO_NOT_DISPLAY","models":[{"id":"model-a","name":"PRIVATE_LABEL"}]}}});
        let review = model_preview(&summary, "hash");
        assert!(review.contains("custom: model-a"));
        assert!(!review.contains("DO_NOT_DISPLAY"));
        assert!(!review.contains("PRIVATE_LABEL"));
    }
}

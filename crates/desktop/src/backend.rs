//! Blocking I/O lives on one worker. The native view receives presentation only.
use crate::ui_model::{Change, Choice, Command, ConfigItem, Device, Event, Snapshot};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
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
// Combined audited documents, models, and custom files share this private protocol envelope.
const MAX_PROTOCOL_BYTES: usize = 48 * 1024 * 1024;
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
                    .take((MAX_PROTOCOL_BYTES + 1) as u64)
                    .read_until(b'\n', &mut bytes);
                match result {
                    Ok(0) | Err(_) => break,
                    Ok(n) if n > MAX_PROTOCOL_BYTES => {
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
        ensure!(
            bytes.len() <= MAX_PROTOCOL_BYTES,
            "Sync request exceeds the safe protocol size limit"
        );
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
        state.restore_startup_selection();
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
                platform: "public".into(),
            },
            Device {
                id: "local".into(),
                name: "This computer".into(),
                address: local_root(),
                status: "Local".into(),
                is_local: true,
                platform: std::env::consts::OS.into(),
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
                status: if self.connection_errors.contains_key(&p.id) {
                    "Connection failed"
                } else if self.sessions.contains_key(&p.id) {
                    "Connected"
                } else {
                    "Not checked"
                }
                .into(),
                is_local: false,
                platform: "ssh".into(),
            }
        }));
        self.snapshot.items = vec![
            ConfigItem { id: "settings".into(), name: "Settings & packages".into(), category: "Pi configuration".into(), detail: "Reviewed portable settings. Device identity, shell, defaults and unrelated values stay local.".into() },
            ConfigItem { id: "subagents".into(), name: "Subagents".into(), category: "Pi configuration".into(), detail: "Concurrency, scheduling and presentation settings.".into() },
            ConfigItem { id: "mcp".into(), name: "MCP configuration".into(), category: "Optional".into(), detail: "Reviewed server fields. Never enabled by selecting settings alone.".into() },
        ];
        if self.snapshot.source_id.as_deref() != Some(PUBLIC_ID) {
            self.snapshot.items.push(ConfigItem { id: "models".into(), name: "Private model definitions".into(), category: "Private · device to device".into(), detail: "Copy the entire models.json file. Private values stay hidden; the previous file is backed up.".into() });
        }
        for id in &self.snapshot.selected_item_ids {
            if let Some(path) = id.strip_prefix("path:") {
                self.snapshot.items.push(ConfigItem {
                    id: id.clone(), name: path.into(), category: "Files & folders".into(),
                    detail: "Relative to the selected Pi directory. Previous files are backed up before replacement.".into(),
                });
            }
        }
        self.snapshot.has_last_selection = self
            .last
            .as_ref()
            .is_some_and(|selection| !selection.items.is_empty() && !selection.targets.is_empty());
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
                self.snapshot.blocked_targets.clear();
                self.publish();
            }
            Command::AddDevice {
                name,
                ssh_line,
                root,
            } => {
                let connection = ConnectionConfig::parse(&ssh_line)?;
                let root = if root.trim().is_empty() {
                    "~/.pi/agent".to_owned()
                } else {
                    root.trim().to_owned()
                };
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
                    agent_dir: root,
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
                self.connection_errors.remove(&id);
                self.snapshot.target_ids.retain(|target| target != &id);
                if self.snapshot.source_id.as_ref() == Some(&id) {
                    self.snapshot.source_id = Some("local".into());
                    self.snapshot.selected_item_ids.clear();
                    self.snapshot.target_ids.retain(|target| target != "local");
                }
                self.invalidate();
                self.snapshot.blocked_targets.clear();
                self.save_selection(Selection {
                    source: self.snapshot.source_id.clone().unwrap_or_else(|| "local".into()),
                    targets: self.snapshot.target_ids.clone(),
                    items: self.snapshot.selected_item_ids.clone(),
                }).context("The device was removed, but the updated selection could not be saved. Save your selection again")?;
                self.publish();
            }
            Command::SelectSource(id) => {
                self.source(&id)?;
                self.save_selection(Selection {
                    source: id.clone(),
                    targets: self
                        .snapshot
                        .target_ids
                        .iter()
                        .filter(|target| *target != &id)
                        .cloned()
                        .collect(),
                    items: vec![],
                })?;
                self.invalidate();
                self.snapshot.blocked_targets.clear();
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
            Command::SaveSelection {
                source,
                targets,
                items,
            } => {
                self.save_selection(Selection {
                    source,
                    targets,
                    items,
                })?;
                self.invalidate();
                let has_last_selection = self.last.as_ref().is_some_and(|selection| {
                    !selection.items.is_empty() && !selection.targets.is_empty()
                });
                self.snapshot.has_last_selection = has_last_selection;
                let _ = self
                    .events
                    .send(Event::SelectionSaved { has_last_selection });
            }
            Command::CancelReview => {
                let plan_id = self.snapshot.plan_id.clone();
                self.invalidate();
                self.snapshot.results.clear();
                self.snapshot.blocked_targets.clear();
                if let Some(plan_id) = plan_id {
                    self.call(json!({"op":"discard","planId":plan_id}))?;
                }
                self.publish();
            }
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
    fn restore_startup_selection(&mut self) {
        self.snapshot.source_id = Some("local".into());
        if let Some(mut saved) = self.last.clone() {
            if self.source(&saved.source).is_err() {
                saved.source = "local".into();
                saved.items.clear();
            }
            saved
                .targets
                .retain(|id| id != &saved.source && self.target(id).is_ok());
            if self.validate_selection(&saved).is_ok() {
                self.snapshot.source_id = Some(saved.source.clone());
                self.snapshot.selected_item_ids = saved.items.clone();
                self.snapshot.target_ids = saved.targets.clone();
                self.last = Some(saved);
            } else {
                self.last = None;
            }
        }
        self.snapshot.selection_loaded = true;
    }
    fn validate_selection(&self, selection: &Selection) -> Result<()> {
        validate_selection_shape(selection)?;
        self.source(&selection.source)?;
        for id in &selection.targets {
            self.target(id)?;
        }
        Ok(())
    }
    fn save_selection(&mut self, selection: Selection) -> Result<()> {
        self.validate_selection(&selection)?;
        write_selection(&self.config_dir.join("last-selection.json"), &selection).context(
            "The selection could not be saved. Your previous saved selection is unchanged",
        )?;
        self.snapshot.source_id = Some(selection.source.clone());
        self.snapshot.selected_item_ids = selection.items.clone();
        self.snapshot.target_ids = selection.targets.clone();
        self.last = Some(selection);
        Ok(())
    }
    fn prepare(&mut self, selection: Selection) -> Result<()> {
        self.validate_selection(&selection)?;
        ensure!(
            !selection.items.is_empty(),
            "Choose at least one item to review"
        );
        ensure!(
            !selection.targets.is_empty(),
            "Choose at least one target device"
        );
        self.invalidate();
        self.snapshot.results.clear();
        self.snapshot.blocked_targets.clear();
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
        self.save_selection(selection.clone())?;
        self.selection = Some(selection);
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
        self.snapshot.blocked_targets.clear();
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
                self.snapshot.blocked_targets.push((
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
                                    source_available: true,
                                });
                                self.changes
                                    .insert(change_id, (id.clone(), document.clone(), path.into()));
                            }
                        }
                    }
                }
            }
            if let Some(files) = target["files"].as_array() {
                for file in files {
                    if file["changed"] != true && !file["error"].is_string() {
                        continue;
                    }
                    let path = file["path"].as_str().context("Invalid file review path")?;
                    let change_id = format!("{id}:files:{path}");
                    self.snapshot.changes.push(Change {
                        id: change_id.clone(), device: name.clone(), path: path.into(),
                        before: custom_file_preview(&file["before"], file["targetHash"].as_str()),
                        after: file["error"].as_str().map(explain).unwrap_or_else(|| format!("{}Whole-file copy. Extension code may execute; permission policies may change tool access. Inspect content before using source.\n\n{}", if file["warning"] == "PERMISSION_POLICY_CHANGE" { "PERMISSION POLICY CHANGE: This standalone policy may grant or remove tool access.\n\n" } else { "" }, custom_file_preview(&file["after"], file["sourceHash"].as_str()))),
                        conflict: true,
                        source_available: file["sourceAvailable"] != false && !file["error"].is_string(),
                    });
                    self.changes
                        .insert(change_id, (id.clone(), "files".into(), path.into()));
                }
            }
            if target["models"]["changed"] == true {
                let change_id = format!("{id}:models:/");
                self.snapshot.changes.push(Change {
                    id: change_id.clone(),
                    device: name,
                    path: "models.json · entire private file".into(),
                    before: model_target_preview(
                        target["models"]["targetHash"].as_str(),
                        target["models"]["targetInspected"] == true,
                    ),
                    after: model_source_preview(
                        target["models"]["sourceHash"].as_str(),
                        target["models"]["error"].as_str(),
                    ),
                    conflict: target["models"]["error"].is_string(),
                    source_available: !target["models"]["error"].is_string(),
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
            self.snapshot.unresolved_count(&choices) == 0,
            "Choose Target or Skip for unavailable source files and resolve the remaining conflicts"
        );
        ensure!(
            choices.keys().all(|id| self.changes.contains_key(id)),
            "A review choice is stale. Prepare again"
        );
        let mut resolutions = json!({});
        let mut model_choices = json!({});
        let mut file_choices = json!({});
        for (id, choice) in choices {
            let (target, document, path) =
                self.changes.get(&id).context("Unknown review choice")?;
            let choice = match choice {
                Choice::Source => "source",
                Choice::Target => "target",
                Choice::Skip => "skip",
            };
            if document == "files" {
                file_choices[target][path] = json!(choice);
            } else if document == "models" {
                model_choices[target] = json!(choice);
            } else {
                resolutions[target][document][path] = json!(choice);
            }
        }
        self.busy("Applying the changes you reviewed…");
        // Resolve against captured snapshots, never silently obtain a new preview.
        let resolved = self.call(json!({"op":"resolve","planId":plan_id,"resolutions":resolutions,"modelChoices":model_choices,"fileChoices":file_choices}))?;
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
        self.snapshot.blocked_targets = blocked.clone();
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
            if !matches!(target["status"].as_str(), Some("applied" | "unchanged")) {
                self.snapshot
                    .blocked_targets
                    .push((self.device_name(id), status.clone()));
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
                            .insert(target_id.clone(), safe_connection_error(&error));
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
fn safe_connection_error(error: &anyhow::Error) -> String {
    let text = error.to_string();
    match text.as_str() {
        "Host key changed. Connection blocked; verify the server independently"
        | "Host identity was not accepted"
        | "Cannot verify host identity"
        | "Authentication cancelled"
        | "SSH key authentication failed"
        | "SSH password authentication failed"
        | "SSH authentication failed"
        | "Cannot resolve SSH host"
        | "Cannot connect to SSH host"
        | "SSH handshake failed"
        | "Cannot start remote Node worker"
        | "Cannot open SSH worker channel"
        | "Cannot send remote request"
        | "Cannot read remote worker response"
        | "Remote request exceeds size limit" => text,
        _ => explain("SSH_FAILED"),
    }
}
fn local_root() -> String {
    std::env::var("PI_CODING_AGENT_DIR").unwrap_or_else(|_| "~/.pi/agent".into())
}
fn custom_file_preview(content: &Value, hash: Option<&str>) -> String {
    let content = content.as_str().unwrap_or("Not present");
    match hash {
        Some(hash) => format!("SHA256 {hash}\n\n{content}"),
        None => content.into(),
    }
}
fn model_target_preview(hash: Option<&str>, inspected: bool) -> String {
    if !inspected {
        return "Destination models.json is unchanged. It was not inspected because the source file is unavailable.".into();
    }
    match hash {
        Some(hash) => format!(
            "Private values hidden.\nSHA256 {hash}\nThe previous file will be backed up before replacement."
        ),
        None => "No previous models.json file. Choosing Source creates it.".into(),
    }
}
fn model_source_preview(hash: Option<&str>, error: Option<&str>) -> String {
    if let Some(error) = error {
        return format!(
            "{}\nChoose Target or Skip to continue with other settings. Choosing Source remains blocked. Private values hidden.",
            explain(error)
        );
    }
    let fingerprint = hash
        .map(|hash| format!("\nSHA256 {hash}"))
        .unwrap_or_default();
    format!(
        "Copy the entire source models.json file. Private values hidden.{fingerprint}\nChoosing Source replaces the destination file, not individual models. Default provider/model settings stay unchanged; you may need to select a model in Pi after replacement. Provider availability is not tested."
    )
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
        "SOURCE_CUSTOM_PATH_MISSING" => "The selected source file or folder was not found. Check its path and review again",
        "PROTECTED_CUSTOM_PATH" => "Authentication and credential files cannot be synced",
        "AUDITED_DOCUMENT_REQUIRED" => "Use the built-in configuration item to review this file safely",
        "INVALID_CUSTOM_PATH" => "Choose a relative file or folder inside the Pi directory",
        "PUBLIC_CUSTOM_PATHS_FORBIDDEN" => "Custom files require a device source",
        "CUSTOM_PATH_COLLISION" => "Selected paths collide on another platform (case or Unicode spelling). Rename them before syncing",
        "CUSTOM_SELECTION_TOO_LARGE" => "Choose a smaller folder or fewer files: custom sync allows 256 files, 4 MiB per file, 8 MiB total and 32 folder levels",
        "CUSTOM_PATH_CHANGED" | "CONCURRENT_UPDATE" => "A selected file changed during sync. Inspect the results and prepare a fresh review",
        "INVALID_CUSTOM_SELECTION" => "The selected file paths are invalid. Choose paths relative to the Pi directory",
        "SOURCE_MODELS_INVALID_JSON" => {
            "The source models.json is not valid JSON and cannot be copied"
        }
        "SOURCE_MODELS_UNREADABLE" => {
            "The source models.json cannot be read. Check its file permissions or skip it"
        }
        "SOURCE_MODELS_MISSING" => "The source has no models.json file to copy",
        "SOURCE_MODELS_SYMLINK_REJECTED" => {
            "The source models.json is a symbolic link and cannot be copied safely"
        }
        "SOURCE_MODELS_NOT_REGULAR_FILE" => {
            "The source models.json is not a regular file and cannot be copied safely"
        }
        "SOURCE_MODELS_TOO_LARGE" => "The source models.json exceeds the safe file size limit",
        "TARGET_MODELS_UNREADABLE" => {
            "The destination models.json cannot be read for a backup. Check its file permissions"
        }
        "PERMISSION_DENIED" => "A file could not be read or written. Check permissions for the configuration and backup folders",
        "DISK_FULL" => "The destination ran out of space. Free space, inspect any applied files, then preview again",
        "READ_ONLY_FILESYSTEM" => "The destination is read-only. Choose a writable configuration folder",
        "IO_ERROR" => "A file operation failed. Check the device connection, available disk space, and configuration folder access",
        "SOURCE_CHANGED" => "The source changed after review. Prepare a fresh preview",
        "STALE_PLAN" | "STALE_TARGET" | "TARGET_CHANGED" | "STALE_SNAPSHOT" => {
            "The target changed after review. Prepare a fresh preview"
        }
        "PUBLIC_MODELS_FORBIDDEN" => "Private models require a device source",
        "SSH_FAILED" => {
            "Connection or authentication failed; this device was not confirmed updated"
        }
        "PLAN_NOT_FOUND" | "PLAN_EXPIRED" => "This preview expired. Prepare it again",
        "SYMLINK_REJECTED" => "A configuration path is a symbolic link. Sync is blocked for safety",
        "NOT_REGULAR_FILE" => {
            "A configuration path is not a regular file. Sync is blocked for safety"
        }
        "FILE_TOO_LARGE" => {
            "A configuration file exceeds the safe file size limit. Sync is blocked"
        }
        "REPOSITORY_TARGET" => {
            "Private device configuration cannot be written inside a Git repository"
        }
        "PUBLIC_FETCH_FAILED" | "PUBLIC_SOURCE_UNAVAILABLE" => {
            "Cannot fetch the public harness. Check the connection and try again"
        }
        _ => "Sync could not continue safely. Check the selected paths and device connection, then review again",
    }
    .into()
}
fn validate_selection_shape(selection: &Selection) -> Result<()> {
    let valid_id = |id: &str| {
        !id.is_empty()
            && id.len() <= 60
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
    };
    ensure!(valid_id(&selection.source), "Invalid source device");
    ensure!(
        selection.targets.len() <= 100 && selection.items.len() <= 256,
        "Selection exceeds the safe size limit"
    );
    let mut targets = HashSet::new();
    for id in &selection.targets {
        ensure!(
            valid_id(id) && id != PUBLIC_ID && id != &selection.source && targets.insert(id),
            "Invalid or duplicate target device"
        );
    }
    let mut items = HashSet::new();
    for item in &selection.items {
        ensure!(items.insert(item), "Duplicate sync item");
        if matches!(item.as_str(), "settings" | "subagents" | "mcp" | "models") {
            ensure!(
                selection.source != PUBLIC_ID || item != "models",
                "Private models require a device source"
            );
            continue;
        }
        let path = item.strip_prefix("path:").context("Unknown sync item")?;
        ensure!(
            selection.source != PUBLIC_ID,
            "Custom files require a device source"
        );
        ensure!(
            !path.is_empty()
                && path.len() <= 1024
                && !path.chars().any(char::is_control)
                && !path.contains(['\\', ':', '?', '*', '<', '>', '|', '"'])
                && path
                    .split('/')
                    .all(|part| !part.is_empty() && !part.starts_with('.')),
            "Choose a relative file or folder inside the Pi directory, without hidden folders or parent traversal"
        );
        ensure!(
            !matches!(
                path.split('/')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .as_str(),
                "settings.json" | "mcp.json" | "subagents.json" | "models.json"
            ),
            "Use the built-in configuration item to review this file safely"
        );
        for part in path.split('/') {
            let lower = part.to_ascii_lowercase();
            let stem = lower.split('.').next().unwrap_or("");
            ensure!(
                !matches!(lower.as_str(), "__proto__" | "prototype" | "constructor")
                    && !lower.ends_with(['.', ' '])
                    && !matches!(stem, "con" | "prn" | "aux" | "nul")
                    && !(stem.len() == 4
                        && (stem.starts_with("com") || stem.starts_with("lpt"))
                        && matches!(stem.as_bytes()[3], b'1'..=b'9')),
                "This file name is not portable between devices"
            );
            let words = lower.split(['.', '_', '-']).collect::<Vec<_>>();
            let protected = ["credential", "password", "secret"]
                .iter()
                .any(|term| lower.contains(*term))
                || words.iter().any(|word| {
                    matches!(
                        *word,
                        "credential"
                            | "credentials"
                            | "secret"
                            | "secrets"
                            | "token"
                            | "tokens"
                            | "oauth"
                            | "authentication"
                            | "authorization"
                    )
                })
                || matches!(words[0], "auth" | "password" | "passwords")
                || matches!(
                    lower.as_str(),
                    "id_rsa" | "id_ed25519" | "id_ecdsa" | "known_hosts" | "authorized_keys"
                )
                || [".pem", ".key", ".p12", ".pfx", ".env"]
                    .iter()
                    .any(|ext| lower.ends_with(*ext));
            ensure!(
                !protected,
                "Authentication and credential files cannot be selected"
            );
        }
    }
    ensure!(
        serde_json::to_vec(selection)?.len() < 64 * 1024,
        "Selection exceeds the safe size limit"
    );
    Ok(())
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
    let selection = serde_json::from_slice(&fs::read(path)?)?;
    validate_selection_shape(&selection)?;
    Ok(Some(selection))
}
fn write_selection(path: &Path, selection: &Selection) -> Result<()> {
    validate_selection_shape(selection)?;
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
    fn test_backend(dir: &Path) -> Backend {
        let (_, commands) = mpsc::channel();
        let (events, _) = mpsc::channel();
        Backend {
            commands,
            events,
            pending: VecDeque::new(),
            engine: None,
            profiles: vec![],
            store: ProfileStore::new(dir.join("profiles.json")),
            config_dir: dir.to_owned(),
            sessions: HashMap::new(),
            connection_errors: HashMap::new(),
            snapshot: Snapshot::default(),
            last: None,
            selection: None,
            plan: None,
            changes: HashMap::new(),
            request: 0,
        }
    }
    #[test]
    fn startup_defaults_local_and_restores_saved_custom_scope_without_a_plan() {
        let dir = tempfile::tempdir().unwrap();
        let mut backend = test_backend(dir.path());
        backend.restore_startup_selection();
        assert_eq!(backend.snapshot.source_id.as_deref(), Some("local"));
        assert!(backend.snapshot.selection_loaded);
        backend.last = Some(Selection {
            source: "local".into(),
            targets: vec!["removed-device".into()],
            items: vec!["path:extensions".into(), "path:permissions.json".into()],
        });
        backend.restore_startup_selection();
        backend.publish();
        assert!(backend.snapshot.target_ids.is_empty());
        assert_eq!(backend.snapshot.selected_item_ids.len(), 2);
        assert!(
            backend
                .snapshot
                .items
                .iter()
                .any(|item| item.id == "path:extensions")
        );
        assert!(backend.snapshot.plan_id.is_none());
        backend.snapshot.selected_item_ids = vec!["subagents".into()];
        backend.handle(Command::Refresh).unwrap();
        assert_eq!(backend.snapshot.selected_item_ids, vec!["subagents"]);
    }
    #[test]
    fn save_selection_does_not_prepare_or_apply_and_invalid_save_preserves_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut backend = test_backend(dir.path());
        backend
            .handle(Command::SaveSelection {
                source: "local".into(),
                targets: vec![],
                items: vec!["path:extensions".into()],
            })
            .unwrap();
        assert!(backend.engine.is_none());
        assert!(backend.snapshot.plan_id.is_none());
        let path = dir.path().join("last-selection.json");
        let before = fs::read(&path).unwrap();
        assert!(
            backend
                .handle(Command::SaveSelection {
                    source: "local".into(),
                    targets: vec![],
                    items: vec!["path:../escape".into()]
                })
                .is_err()
        );
        assert_eq!(fs::read(path).unwrap(), before);
    }
    #[test]
    fn save_acknowledgements_cannot_replace_newer_ui_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut backend = test_backend(dir.path());
        let (tx, events) = mpsc::channel();
        backend.events = tx;
        backend
            .handle(Command::SaveSelection {
                source: "local".into(),
                targets: vec![],
                items: vec![],
            })
            .unwrap();
        assert!(matches!(
            events.try_recv().unwrap(),
            Event::SelectionSaved {
                has_last_selection: false
            }
        ));
        assert!(events.try_recv().is_err());
    }
    #[test]
    fn source_change_is_saved_and_default_device_roots_are_deduplicated() {
        let dir = tempfile::tempdir().unwrap();
        let mut backend = test_backend(dir.path());
        backend
            .handle(Command::AddDevice {
                name: "Test".into(),
                ssh_line: "ssh user@host".into(),
                root: "".into(),
            })
            .unwrap();
        assert!(
            backend
                .handle(Command::AddDevice {
                    name: "Duplicate".into(),
                    ssh_line: "ssh user@host".into(),
                    root: "  ~/.pi/agent  ".into()
                })
                .is_err()
        );
        let source = backend.profiles[0].id.clone();
        backend.snapshot.target_ids = vec![source.clone(), "local".into()];
        backend.snapshot.selected_item_ids = vec!["settings".into()];
        backend
            .handle(Command::SelectSource(source.clone()))
            .unwrap();
        let saved = read_selection(&dir.path().join("last-selection.json"))
            .unwrap()
            .unwrap();
        assert_eq!(saved.source, source);
        assert_eq!(saved.targets, vec!["local"]);
        assert!(saved.items.is_empty());
        assert!(!backend.snapshot.has_last_selection);
    }
    #[test]
    fn removing_devices_persists_remaining_scope_and_resets_removed_source() {
        let dir = tempfile::tempdir().unwrap();
        let mut backend = test_backend(dir.path());
        for (name, ssh) in [("First", "ssh user@first"), ("Second", "ssh user@second")] {
            backend
                .handle(Command::AddDevice {
                    name: name.into(),
                    ssh_line: ssh.into(),
                    root: "".into(),
                })
                .unwrap();
        }
        let first = backend.profiles[0].id.clone();
        let second = backend.profiles[1].id.clone();
        backend
            .save_selection(Selection {
                source: "local".into(),
                targets: vec![first.clone(), second.clone()],
                items: vec!["path:extensions".into()],
            })
            .unwrap();
        backend.handle(Command::RemoveDevice(first)).unwrap();
        let saved = read_selection(&dir.path().join("last-selection.json"))
            .unwrap()
            .unwrap();
        assert_eq!(saved.targets, vec![second.clone()]);
        assert_eq!(saved.items, vec!["path:extensions"]);
        backend
            .save_selection(Selection {
                source: second.clone(),
                targets: vec!["local".into()],
                items: vec!["path:remote-only".into()],
            })
            .unwrap();
        backend.handle(Command::RemoveDevice(second)).unwrap();
        let saved = read_selection(&dir.path().join("last-selection.json"))
            .unwrap()
            .unwrap();
        assert_eq!(saved.source, "local");
        assert!(saved.targets.is_empty());
        assert!(saved.items.is_empty());
        assert!(!backend.snapshot.has_last_selection);
    }
    #[test]
    fn rapid_saves_are_fifo_and_empty_selection_is_persistent() {
        let dir = tempfile::tempdir().unwrap();
        let mut backend = test_backend(dir.path());
        for items in [
            vec!["settings".into()],
            vec!["path:extensions".into()],
            vec![],
        ] {
            backend
                .handle(Command::SaveSelection {
                    source: "local".into(),
                    targets: vec![],
                    items,
                })
                .unwrap();
        }
        let saved = read_selection(&dir.path().join("last-selection.json"))
            .unwrap()
            .unwrap();
        assert!(saved.items.is_empty());
        assert!(saved.targets.is_empty());
        backend.last = Some(saved);
        backend.restore_startup_selection();
        assert!(backend.snapshot.selected_item_ids.is_empty());
        assert!(backend.snapshot.target_ids.is_empty());
    }
    #[test]
    fn invalid_custom_selections_are_rejected_but_permission_files_are_explicitly_allowed() {
        let mut selection = Selection {
            source: "local".into(),
            targets: vec![],
            items: vec![],
        };
        for path in [
            "",
            "../escape",
            "/absolute",
            "x/./a",
            "x/../a",
            ".ssh",
            "x//a",
            "x/",
            "C:/x",
            "auth.json",
            "settings.json",
            "mcp.json",
            "private.pem",
        ] {
            selection.items = vec![format!("path:{path}")];
            assert!(
                validate_selection_shape(&selection).is_err(),
                "accepted {path}"
            );
        }
        selection.items = vec!["path:permissions.json".into(), "path:extensions".into()];
        assert!(validate_selection_shape(&selection).is_ok());
        selection.source = PUBLIC_ID.into();
        assert!(validate_selection_shape(&selection).is_err());
    }
    #[test]
    fn file_reviews_require_explicit_decision_and_failed_targets_are_not_zero_diffs() {
        let dir = tempfile::tempdir().unwrap();
        let mut backend = test_backend(dir.path());
        backend.set_plan(json!({"planId":"files-plan","targets":[
            {"id":"local","status":"conflicts","files":[{"path":"extensions/example.ts","changed":true,"sourceAvailable":true,"before":"old","after":"new","warning":"PERMISSION_POLICY_CHANGE"}]},
            {"id":"offline","status":"blocked","error":"SSH_FAILED"}
        ]})).unwrap();
        assert_eq!(backend.snapshot.blocked_targets.len(), 1);
        assert_eq!(backend.snapshot.unresolved_count(&BTreeMap::new()), 1);
        assert!(
            backend.snapshot.changes[0]
                .after
                .contains("permission policies")
        );
        assert_eq!(
            backend.changes["local:files:extensions/example.ts"].1,
            "files"
        );
        backend.invalidate();
        assert!(backend.apply("files-plan".into(), BTreeMap::new()).is_err());
        assert!(backend.engine.is_none());
    }
    #[test]
    fn hidden_file_reviews_still_identify_exact_bytes() {
        let preview =
            custom_file_preview(&json!("[Sensitive content hidden]"), Some("reviewed-hash"));
        assert!(preview.contains("SHA256 reviewed-hash"));
        assert!(preview.contains("[Sensitive content hidden]"));
        assert_eq!(custom_file_preview(&Value::Null, None), "Not present");
    }
    #[test]
    fn unknown_error_never_displays_remote_content() {
        assert!(!explain("secret remote stderr").contains("secret remote stderr"));
    }
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
    fn embedded_bridge_skips_invalid_models_and_applies_settings() {
        let root = tempfile::tempdir().unwrap();
        #[cfg(target_os = "macos")]
        let root_path = fs::canonicalize(root.path()).unwrap();
        #[cfg(not(target_os = "macos"))]
        let root_path = root.path().to_path_buf();
        let source = root_path.join("source");
        let target = root_path.join("target");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(source.join("settings.json"), r#"{"theme":"light"}"#).unwrap();
        fs::write(source.join("models.json"), "not json").unwrap();
        fs::write(target.join("settings.json"), r#"{"theme":"dark"}"#).unwrap();
        fs::write(target.join("models.json"), "keep these exact bytes").unwrap();
        let mut engine = Engine::spawn().unwrap();
        let preview = exchange(
            &mut engine,
            json!({"id":1,"op":"preview","source":{"id":"source","type":"local","agentDir":source},"targets":[{"id":"target","type":"local","agentDir":target}],"items":["settings","models"]}),
        );
        assert_eq!(preview["ok"], true, "fixture preview failed: {preview}");
        assert_eq!(preview["targets"][0]["status"], "conflicts");
        assert_eq!(
            preview["targets"][0]["models"]["error"],
            "SOURCE_MODELS_INVALID_JSON"
        );
        let resolved = exchange(
            &mut engine,
            json!({"id":2,"op":"resolve","planId":preview["planId"],"resolutions":{"target":{"settings":{"/theme":"source"}}},"modelChoices":{"target":"skip"}}),
        );
        assert_eq!(resolved["targets"][0]["status"], "ready");
        assert_eq!(resolved["targets"][0]["models"]["changed"], false);
        let applied = exchange(
            &mut engine,
            json!({"id":3,"op":"apply","planId":resolved["planId"],"approved":true,"targetIds":["target"]}),
        );
        assert_eq!(applied["targets"][0]["status"], "applied");
        let saved: Value =
            serde_json::from_slice(&fs::read(target.join("settings.json")).unwrap()).unwrap();
        assert_eq!(saved["theme"], "light");
        assert_eq!(
            fs::read_to_string(target.join("models.json")).unwrap(),
            "keep these exact bytes"
        );
    }
    #[test]
    fn model_review_describes_private_whole_file_copy_and_backup() {
        let before = model_target_preview(Some("target-hash"), true);
        let after = model_source_preview(Some("source-hash"), None);
        assert!(before.contains("previous file will be backed up"));
        assert!(before.contains("Private values hidden"));
        assert!(after.contains("entire source models.json file"));
        assert!(after.contains("replaces the destination file"));
        assert!(after.contains("Provider availability is not tested"));
        assert!(after.contains("SHA256 source-hash"));
        assert!(model_target_preview(None, true).contains("No previous models.json"));
        assert!(model_target_preview(None, false).contains("not inspected"));
    }
    #[test]
    fn model_source_failures_offer_skipping_without_pretending_to_copy() {
        for error in [
            "SOURCE_MODELS_INVALID_JSON",
            "SOURCE_MODELS_UNREADABLE",
            "SOURCE_MODELS_MISSING",
        ] {
            let review = model_source_preview(None, Some(error));
            assert!(review.contains("Choose Target or Skip"));
            assert!(review.contains("Choosing Source remains blocked"));
            assert!(!review.contains("Copy the entire"));
            assert!(!review.contains("SHA256"));
        }
    }
    #[test]
    fn file_errors_offer_actionable_recovery_without_raw_codes() {
        for code in [
            "PERMISSION_DENIED",
            "DISK_FULL",
            "READ_ONLY_FILESYSTEM",
            "IO_ERROR",
        ] {
            let message = explain(code);
            assert!(!message.contains(code));
            assert!(!message.starts_with("Sync blocked:"));
        }
    }
}

//! The workspace owns selection and presentation; backend owns durable profiles,
//! device I/O, reviewed plans, conflict validation, and explicit application.
use crate::ui_model::*;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Selectable, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    dialog::DialogButtonProps,
    form::{Field, Form},
    input::{Input, InputState},
    radio::{Radio, RadioGroup},
    resizable::{h_resizable, resizable_panel},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::mpsc::{Receiver, Sender, TryRecvError},
    time::Duration,
};

actions!(pi_sync, [RefreshDevices, PrepareReview, RepeatSelection]);

#[derive(Clone)]
struct DragItem {
    id: String,
    name: String,
}
impl Render for DragItem {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .p_3()
            .rounded(cx.theme().radius)
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .shadow_md()
            .child(self.name.clone())
    }
}

pub(crate) struct Workspace {
    commands: Sender<Command>,
    snapshot: Snapshot,
    targets: BTreeSet<String>,
    selected: BTreeSet<String>,
    choices: BTreeMap<String, Choice>,
    busy: Option<String>,
    error: Option<String>,
    focus: FocusHandle,
    onboarding: bool,
    initial_snapshot_received: bool,
    source_picker_open: bool,
    setup_name: Entity<InputState>,
    setup_ssh: Entity<InputState>,
    setup_root: Entity<InputState>,
    _events_task: Task<()>,
}

impl Workspace {
    pub(crate) fn new(
        commands: Sender<Command>,
        events: Receiver<Event>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.bind_keys([
            KeyBinding::new(
                if cfg!(target_os = "macos") {
                    "cmd-r"
                } else {
                    "ctrl-r"
                },
                RefreshDevices,
                Some("PiSync"),
            ),
            KeyBinding::new(
                if cfg!(target_os = "macos") {
                    "cmd-enter"
                } else {
                    "ctrl-enter"
                },
                PrepareReview,
                Some("PiSync"),
            ),
            KeyBinding::new(
                if cfg!(target_os = "macos") {
                    "cmd-shift-r"
                } else {
                    "ctrl-shift-r"
                },
                RepeatSelection,
                Some("PiSync"),
            ),
        ]);
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let _events_task = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(80))
                    .await;
                let connected = this.update_in(cx, |this, window, cx| loop {
                    match events.try_recv() {
                        Ok(event) => this.receive(event, window, cx),
                        Err(TryRecvError::Empty) => return true,
                        Err(TryRecvError::Disconnected) => {
                            this.receive(Event::Error("The sync worker stopped. Restart Pi Sync before trying again.".into()), window, cx);
                            return false;
                        }
                    }
                });
                if !matches!(connected, Ok(true)) { break; }
            }
        });
        let this = Self {
            commands,
            snapshot: Snapshot::default(),
            targets: BTreeSet::new(),
            selected: BTreeSet::new(),
            choices: BTreeMap::new(),
            busy: Some("Reading device profiles".into()),
            error: None,
            focus,
            onboarding: false,
            initial_snapshot_received: false,
            source_picker_open: false,
            setup_name: cx.new(|cx| InputState::new(window, cx).placeholder("Work laptop")),
            setup_ssh: cx.new(|cx| InputState::new(window, cx).placeholder("ssh user@host -p 22")),
            setup_root: cx.new(|cx| InputState::new(window, cx).placeholder("~/.pi/agent")),
            _events_task,
        };
        let _ = this.commands.send(Command::Refresh);
        this
    }

    fn send(&mut self, command: Command, message: &str, cx: &mut Context<Self>) {
        self.error = None;
        if self.commands.send(command).is_err() {
            self.error =
                Some("The sync worker stopped. Restart Pi Sync before trying again.".into());
            self.busy = None;
        } else {
            self.busy = Some(message.into());
        }
        cx.notify();
    }
    fn invalidate_review(&mut self) {
        self.snapshot.plan_id = None;
        self.snapshot.changes.clear();
        self.choices.clear();
        self.snapshot.results.clear();
        self.snapshot.blocked_targets.clear();
    }
    fn stage(&mut self, id: String, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.selected.insert(id);
        self.invalidate_review();
        self.save_selection(cx);
    }
    fn prepare(&mut self, cx: &mut Context<Self>) {
        if self.onboarding
            || self.snapshot.plan_id.is_some()
            || self.busy.is_some()
            || self.targets.is_empty()
            || self.selected.is_empty()
        {
            return;
        }
        if let Some(source) = self.snapshot.source_id.clone() {
            self.send(
                Command::Prepare {
                    source,
                    targets: self.targets.iter().cloned().collect(),
                    items: self.selected.iter().cloned().collect(),
                },
                "Reading target files and preparing a fresh review",
                cx,
            );
        }
    }
    pub(crate) fn receive(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            Event::Snapshot(snapshot) => {
                if !self.initial_snapshot_received {
                    self.onboarding = !snapshot
                        .devices
                        .iter()
                        .any(|device| !device.is_local && device.id != "public-harness");
                    self.initial_snapshot_received = true;
                    if snapshot.selection_loaded {
                        self.selected = snapshot.selected_item_ids.iter().cloned().collect();
                        self.targets = snapshot.target_ids.iter().cloned().collect();
                    }
                }
                if let Some((items, targets)) =
                    snapshot.restored_selection(self.snapshot.plan_id.as_deref())
                {
                    self.selected = items;
                    self.targets = targets;
                }
                if snapshot.plan_id != self.snapshot.plan_id {
                    self.choices.clear();
                }
                self.snapshot = snapshot;
                self.targets.retain(|id| {
                    self.snapshot.devices.iter().any(|d| &d.id == id)
                        && self.snapshot.source_id.as_ref() != Some(id)
                });
                self.selected.retain(|id| {
                    id.starts_with("path:") || self.snapshot.items.iter().any(|item| &item.id == id)
                });
                self.busy = None;
            }
            Event::SelectionSaved { has_last_selection } => {
                self.snapshot.has_last_selection = has_last_selection;
            }
            Event::Busy(message) => {
                self.busy = Some(message);
                self.error = None;
            }
            Event::Error(message) => {
                self.error = Some(message);
                self.busy = None;
            }
            Event::TrustHost {
                request_id,
                host,
                fingerprint,
            } => self.trust_host(request_id, host, fingerprint, window, cx),
            Event::Authentication { request_id, prompt } => {
                self.authenticate(request_id, prompt, window, cx)
            }
        }
        cx.notify();
    }

    fn add_device(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Workstation"));
        let ssh = cx.new(|cx| InputState::new(window, cx).placeholder("ssh user@host -p 22"));
        let root = cx.new(|cx| InputState::new(window, cx).placeholder("~/.pi/agent"));
        let sender = self.commands.clone();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let name = name.clone(); let ssh = ssh.clone(); let root = root.clone(); let sender = sender.clone();
            dialog.title("Add SSH device").confirm()
                .button_props(DialogButtonProps::default().ok_text("Add device"))
                .child(Form::new()
                    .child(Field::new().label("Device name").child(Input::new(&name).id("device-name")))
                    .child(Field::new().label("SSH command or host").child(Input::new(&ssh).id("ssh-command")))
                    .child(Field::new().label("Pi agent directory").child(Input::new(&root).id("agent-root"))))
                .child("Paste a normal SSH command. Passwords and passphrases are requested separately and are never saved in profiles.")
                .on_ok(move |_, _, cx| {
                    let ssh_line = ssh.read(cx).value().trim().to_string();
                    if ssh_line.is_empty() { return false; }
                    sender.send(Command::AddDevice { name: name.read(cx).value().trim().to_string(), ssh_line, root: root.read(cx).value().trim().to_string() }).is_ok()
                })
        });
    }
    fn trust_host(
        &mut self,
        request_id: String,
        host: String,
        fingerprint: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sender = self.commands.clone();
        window.open_alert_dialog(cx, move |dialog, _, cx| {
            let yes = sender.clone(); let no = sender.clone(); let yes_id = request_id.clone(); let no_id = request_id.clone();
            dialog.title("Verify SSH host identity").confirm().close_button(false)
                .button_props(DialogButtonProps::default().ok_text("Trust this host").cancel_text("Cancel connection"))
                .child(format!("First connection to {host}. Compare this fingerprint with the device's administrator or another trusted source."))
                .child(div().p_3().bg(cx.theme().muted).rounded(cx.theme().radius).child(fingerprint.clone()))
                .child("Trust is saved for this host. A changed key will block the connection.")
                .on_ok(move |_, _, _| { let _ = yes.send(Command::TrustHost { request_id: yes_id.clone(), accepted: true }); true })
                .on_cancel(move |_, _, _| { let _ = no.send(Command::TrustHost { request_id: no_id.clone(), accepted: false }); true })
        });
    }
    fn authenticate(
        &mut self,
        request_id: String,
        prompt: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let secret = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Password or key passphrase")
        });
        let sender = self.commands.clone();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let input = secret.clone();
            let clear = secret.clone();
            let yes = sender.clone();
            let no = sender.clone();
            let yes_id = request_id.clone();
            let no_id = request_id.clone();
            dialog
                .title("Authenticate SSH connection")
                .confirm()
                .close_button(false)
                .button_props(DialogButtonProps::default().ok_text("Connect"))
                .child(prompt.clone())
                .child(Input::new(&secret).id("ssh-secret"))
                .child("Used only for this connection. Not saved in your device profile.")
                .on_ok(move |_, window, cx| {
                    let value = zeroize::Zeroizing::new(input.read(cx).value().to_string());
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                    let _ = yes.send(Command::Authenticate {
                        request_id: yes_id.clone(),
                        secret: value,
                    });
                    true
                })
                .on_cancel(move |_, window, cx| {
                    clear.update(cx, |input, cx| input.set_value("", window, cx));
                    let _ = no.send(Command::CancelAuthentication {
                        request_id: no_id.clone(),
                    });
                    true
                })
        });
    }
    fn save_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(source) = self.snapshot.source_id.clone()
            && self
                .commands
                .send(Command::SaveSelection {
                    source,
                    targets: self.targets.iter().cloned().collect(),
                    items: self.selected.iter().cloned().collect(),
                })
                .is_err()
        {
            self.error =
                Some("Your selection could not be saved because the worker stopped.".into());
        }
        cx.notify();
    }

    fn add_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = cx.new(|cx| {
            InputState::new(window, cx).placeholder("extensions or skills/my-skill/SKILL.md")
        });
        let entity = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let path = path.clone(); let entity = entity.clone();
            dialog.title("Choose a file or folder").confirm()
                .button_props(DialogButtonProps::default().ok_text("Remember selection"))
                .child("Enter a path relative to the source Pi agent directory. It will be copied to the same relative path on each selected target.")
                .child(Input::new(&path).id("custom-path"))
                .child("Folders are recursive. Target-only files stay untouched. Authentication and sync-state files are blocked. Use the built-in selections for settings.json, mcp.json, subagents.json and models.json.")
                .child("Selected extension code can execute in Pi. Standalone permission policies can change tool access. Every file requires an explicit decision in review.")
                .on_ok(move |_, _, cx| {
                    let value = path.read(cx).value().trim().to_string();
                    if value.is_empty() { return false; }
                    entity.update(cx, |this, cx| {
                        let id = format!("path:{value}");
                        if !this.snapshot.items.iter().any(|item| item.id == id) {
                            this.snapshot.items.push(ConfigItem { id: id.clone(), name: value.clone(), category: "File or folder".into(), detail: "Whole-file review at the same relative path on each target. Target-only files stay untouched.".into() });
                        }
                        this.selected.insert(id);
                        this.invalidate_review();
                        this.save_selection(cx);
                    }).is_ok()
                })
        });
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let reviewing = self.snapshot.plan_id.is_some();
        div()
            .flex()
            .items_center()
            .gap_4()
            .px_5()
            .py_4()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Pi Sync"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.onboarding {
                        "1  Connect your devices"
                    } else if reviewing {
                        "3  Review & confirm"
                    } else {
                        "2  Choose what goes where"
                    }),
            )
            .child(div().flex_1())
            .when(!self.onboarding && !reviewing, |el| {
                el.child(
                    Button::new("repeat-last")
                        .ghost()
                        .label("Review saved selection")
                        .disabled(self.busy.is_some() || !self.snapshot.has_last_selection)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.send(
                                Command::RepeatLast,
                                "Preparing a fresh review of your saved selection",
                                cx,
                            )
                        })),
                )
            })
            .when(!reviewing, |el| {
                el.child(
                    Button::new("refresh")
                        .ghost()
                        .label("Refresh")
                        .disabled(self.busy.is_some())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.invalidate_review();
                            this.send(Command::Refresh, "Refreshing devices", cx);
                        })),
                )
            })
    }

    fn device_icon(device: &Device) -> Icon {
        Icon::new(match device.platform.as_str() {
            "public" => gpui_kit::assets::IconName::Globe,
            "macos" => gpui_kit::assets::IconName::Laptop,
            "windows" | "linux" => gpui_kit::assets::IconName::Monitor,
            _ if device.is_local => gpui_kit::assets::IconName::Monitor,
            _ => gpui_kit::assets::IconName::Server,
        })
        .size_6()
    }

    fn platform_label(device: &Device) -> &'static str {
        match device.platform.as_str() {
            "macos" => "macOS",
            "windows" => "Windows",
            "linux" => "Linux",
            "public" => "Public preset",
            _ if device.is_local => "This computer",
            _ => "SSH device",
        }
    }

    fn render_onboarding(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let saved = self
            .snapshot
            .devices
            .iter()
            .filter(|d| !d.is_local && d.id != "public-harness")
            .collect::<Vec<_>>();
        div().id("connection-setup").size_full().flex().flex_col().p_8().gap_5()
            .child(div().text_2xl().font_weight(FontWeight::SEMIBOLD).child("Where would you like to sync?"))
            .child(div().text_color(cx.theme().muted_foreground).child("Save your SSH connections once. This computer is your default source. You will choose files and review every change before anything is written."))
            .child(div().flex().flex_1().min_h_0().gap_8()
                .child(div().flex_1().min_w_0().flex().flex_col().gap_4()
                    .child(Form::new()
                        .child(Field::new().label("Device name").child(Input::new(&self.setup_name).id("setup-device-name")))
                        .child(Field::new().label("SSH connection").child(Input::new(&self.setup_ssh).id("setup-ssh-command")))
                        .child(Field::new().label("Pi agent directory on this device").child(Input::new(&self.setup_root).id("setup-agent-root"))))
                    .child(Button::new("save-connection").primary().label("Save connection").disabled(self.busy.is_some())
                        .on_click(cx.listener(|this, _, _, cx| {
                            let name = this.setup_name.read(cx).value().trim().to_string();
                            let ssh_line = this.setup_ssh.read(cx).value().trim().to_string();
                            if name.is_empty() || ssh_line.is_empty() { this.error = Some("Enter a device name and SSH connection.".into()); cx.notify(); return; }
                            let root = this.setup_root.read(cx).value().trim().to_string();
                            this.send(Command::AddDevice { name, ssh_line, root }, "Saving connection", cx);
                        })))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Only connection details are saved. SSH passwords and key passphrases are requested when connecting and never stored. Saving is not a connection test.")))
                .child(div().flex_1().min_w_0().id("saved-connections").overflow_y_scroll().flex().flex_col().gap_3()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Saved devices"))
                    .when(saved.is_empty(), |el| el.child(div().p_5().rounded(cx.theme().radius).bg(cx.theme().muted).child("Your devices will appear here.")))
                    .children(saved.iter().map(|device| div().flex().items_center().gap_3().p_4().border_1().border_color(cx.theme().border).rounded(cx.theme().radius)
                        .child(Self::device_icon(device))
                        .child(div().flex_1().min_w_0().child(device.name.clone()).child(div().text_xs().text_color(cx.theme().muted_foreground).child(device.address.clone())))
                        .child(div().text_xs().text_color(cx.theme().muted_foreground).child(Self::platform_label(device)))))))
            .child(div().flex().justify_end().child(Button::new("setup-next").primary().label("Next: choose what to sync")
                .disabled(self.busy.is_some() || saved.is_empty())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.onboarding = false;
                    if this.targets.is_empty()
                        && let Some(device) = this.snapshot.devices.iter().find(|d| !d.is_local && d.id != "public-harness" && Some(&d.id) != this.snapshot.source_id.as_ref()) { this.targets.insert(device.id.clone()); }
                    this.save_selection(cx);
                }))))
    }

    fn render_items(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy.is_some();
        let source = self
            .snapshot
            .devices
            .iter()
            .find(|device| Some(&device.id) == self.snapshot.source_id.as_ref());
        let items = self
            .snapshot
            .items
            .iter()
            .filter(|item| item.id.starts_with("path:"))
            .chain(
                self.snapshot
                    .items
                    .iter()
                    .filter(|item| !item.id.starts_with("path:")),
            );
        div().id("source-selection").flex().flex_col().size_full().min_w_0()
            .child(div().px_5().pt_5().pb_4()
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("What to sync"))
                .child(div().mt_1().text_sm().text_color(cx.theme().muted_foreground).child("Choose once. Your selection is remembered for the next review."))
                .when_some(source, |el, device| el.child(div().mt_4().p_3().flex().items_center().gap_3().rounded(cx.theme().radius).bg(cx.theme().muted)
                    .child(Self::device_icon(device))
                    .child(div().flex_1().min_w_0().child(div().font_weight(FontWeight::MEDIUM).child(format!("From {}", device.name))).child(div().text_xs().text_color(cx.theme().muted_foreground).child(device.address.clone())))
                    .child(Button::new("change-source").ghost().small().label("Change").disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| { this.source_picker_open = !this.source_picker_open; cx.notify(); }))))))
            .when(self.source_picker_open, |el| el.child(div().px_5().pb_3().flex().flex_col().gap_2()
                .children(self.snapshot.devices.iter().map(|device| {
                    let id = device.id.clone();
                    Button::new(SharedString::from(format!("source-{id}"))).small().label(format!("{} · {}", device.name, Self::platform_label(device)))
                        .selected(self.snapshot.source_id.as_ref() == Some(&id)).disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.source_picker_open = false;
                            this.selected.clear(); this.targets.remove(&id); this.invalidate_review();
                            this.send(Command::SelectSource(id.clone()), "Changing source", cx);
                        }))
                }))))
            .child(div().px_5().pb_3().child(Button::new("add-path").w_full().label("Add file or folder…").disabled(busy || self.snapshot.source_id.as_deref() == Some("public-harness"))
                .on_click(cx.listener(|this, _, window, cx| this.add_path(window, cx)))))
            .child(div().id("configuration-library").flex_1().min_h_0().overflow_y_scroll().px_5().pb_5().flex().flex_col().gap_3()
                .children(items.map(|item| {
                    let id = item.id.clone(); let checked = self.selected.contains(&id);
                    let drag = DragItem { id: id.clone(), name: item.name.clone() };
                    div().id(SharedString::from(format!("config-{id}"))).test_support().p_4().rounded(cx.theme().radius)
                        .border_1().border_color(if checked { cx.theme().primary } else { cx.theme().border })
                        .bg(if checked { cx.theme().accent } else { cx.theme().background })
                        .when(!busy, |el| el.on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone())))
                        .child(Checkbox::new(SharedString::from(format!("select-{id}"))).label(item.name.clone()).checked(checked).disabled(busy)
                            .on_change(cx.listener(move |this, checked, _, cx| {
                                if *checked { this.selected.insert(id.clone()); } else { this.selected.remove(&id); }
                                this.invalidate_review(); this.save_selection(cx);
                            })))
                        .child(div().mt_2().text_xs().text_color(cx.theme().muted_foreground).child(item.category.clone()))
                        .child(div().mt_1().text_sm().text_color(cx.theme().muted_foreground).child(item.detail.clone()))
                })))
            .child(div().px_5().py_3().border_t_1().border_color(cx.theme().border).text_xs().text_color(cx.theme().muted_foreground).child("Paths are relative to each device's Pi directory. Authentication files stay local. Built-in settings preserve device identity, model defaults, shell and embedded permissions."))
    }

    fn render_queue(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy.is_some();
        let eligible = self
            .snapshot
            .devices
            .iter()
            .filter(|d| d.id != "public-harness" && Some(&d.id) != self.snapshot.source_id.as_ref())
            .collect::<Vec<_>>();
        div().id("target-selection").flex().flex_col().size_full().min_w_0().bg(cx.theme().sidebar)
            .child(div().px_5().pt_5().pb_4()
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("Where to sync"))
                .child(div().mt_1().text_sm().text_color(cx.theme().muted_foreground).child("Choose your saved target devices.")))
            .child(div().id("queue-scroll").flex_1().min_h_0().overflow_y_scroll().px_5().pb_5().flex().flex_col().gap_3()
                .when(eligible.is_empty(), |el| el.child(div().py_5().child("Save an SSH connection to choose a target.")))
                .children(eligible.iter().map(|device| {
                    let id = device.id.clone(); let remove_id = id.clone(); let remove_name = device.name.clone(); let checked = self.targets.contains(&id);
                    div().p_4().rounded(cx.theme().radius).border_1().border_color(if checked { cx.theme().primary } else { cx.theme().border }).bg(if checked { cx.theme().accent } else { cx.theme().background })
                        .child(div().flex().items_center().gap_3().child(Self::device_icon(device))
                            .child(div().flex_1().min_w_0().child(Checkbox::new(SharedString::from(format!("target-{id}"))).label(device.name.clone()).checked(checked).disabled(busy)
                                .on_change(cx.listener(move |this, checked, _, cx| {
                                    if *checked { this.targets.insert(id.clone()); } else { this.targets.remove(&id); }
                                    this.invalidate_review(); this.save_selection(cx);
                                })))
                                .child(div().mt_1().text_xs().text_color(cx.theme().muted_foreground).child(format!("{} · {}", Self::platform_label(device), device.status))))
                            .when(!device.is_local, |el| el.child(Button::new(SharedString::from(format!("remove-device-{remove_id}"))).ghost().small().label("Remove").disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let sender = this.commands.clone(); let id = remove_id.clone(); let name = remove_name.clone();
                                    window.open_alert_dialog(cx, move |dialog, _, _| {
                                        let sender = sender.clone(); let id = id.clone();
                                        dialog.title(format!("Remove {name}?")).confirm().button_props(DialogButtonProps::default().ok_text("Remove profile"))
                                            .child("Only the saved connection is removed. Files on the device stay unchanged.")
                                            .on_ok(move |_, _, _| sender.send(Command::RemoveDevice(id.clone())).is_ok())
                                    });
                                })))))
                        .child(div().mt_3().text_xs().text_color(cx.theme().muted_foreground).child(device.address.clone()))
                }))
                .child(Button::new("add-device").label("Add SSH device…").disabled(busy).on_click(cx.listener(|this, _, window, cx| this.add_device(window, cx))))
                .child(div().id("queue-drop-zone").test_support().mt_3().p_4().rounded(cx.theme().radius).bg(cx.theme().muted)
                    .on_drop(cx.listener(|this, item: &DragItem, _, cx| this.stage(item.id.clone(), cx)))
                    .child(div().font_weight(FontWeight::MEDIUM).child(format!("Selected items: {} · target devices: {}", self.selected.len(), self.targets.len())))
                    .child(div().mt_1().text_xs().text_color(cx.theme().muted_foreground).child("Review reads the current source and targets. Nothing is written until you confirm.")))
                .when(!self.snapshot.results.is_empty(), |el| el.child(div().mt_3().flex().flex_col().gap_3()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Last operation"))
                    .children(self.snapshot.results.iter().map(|(device, status)| div().text_sm().child(div().font_weight(FontWeight::MEDIUM).child(device.clone())).child(status.clone()))))))
            .child(div().px_5().py_4().border_t_1().border_color(cx.theme().border)
                .child(Button::new("prepare-review").primary().w_full().label("Review changes →")
                    .disabled(busy || self.targets.is_empty() || self.selected.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.prepare(cx)))))
    }

    fn unresolved(&self) -> usize {
        self.snapshot.unresolved_count(&self.choices)
    }
    fn confirm_apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.unresolved() != 0 {
            return;
        }
        let Some(plan_id) = self.snapshot.plan_id.clone() else {
            return;
        };
        let Some(choices) = self.snapshot.reviewed_choices(&self.choices) else {
            return;
        };
        let writes = choices
            .values()
            .filter(|choice| **choice == Choice::Source)
            .count();
        let targets = self
            .snapshot
            .changes
            .iter()
            .filter(|change| choices.get(&change.id) == Some(&Choice::Source))
            .map(|change| change.device.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        let targets = if targets.is_empty() {
            "None; all changes are kept or skipped".to_string()
        } else {
            targets
        };
        let private_transfer = self.snapshot.changes.iter().any(|change| {
            choices.get(&change.id) == Some(&Choice::Source)
                && (change.path.starts_with("models.json")
                    || change.path.starts_with("mcp.json")
                    || change.id.contains(":files:"))
        });

        let entity = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone(); let plan_id = plan_id.clone(); let choices = choices.clone();
            dialog.title("Confirm sync?").confirm()
                .button_props(DialogButtonProps::default().ok_text("Apply changes"))
                .child(format!("Apply {writes} reviewed changes to the selected target devices. Target files are checked again before writing. Previous versions of changed files are backed up; offline or changed targets are reported separately."))
                .child(format!("Targets: {targets}"))
                .when(private_transfer, |dialog| dialog.child("This selection includes whole files or MCP values that may contain private data. Extension code can execute in Pi; standalone permission policies can change tool access. Hidden or truncated contents are still copied in full to the named targets."))
                .on_ok(move |_, _, cx| entity.update(cx, |this, cx| {
                    if this.busy.is_some() || this.snapshot.plan_id.as_ref() != Some(&plan_id) { return false; }
                    this.send(Command::Apply { plan_id: plan_id.clone(), choices: choices.clone() }, "Applying the reviewed changes", cx);
                    true
                }).unwrap_or(false))
        });
    }

    fn render_review(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let unresolved = self.unresolved();
        let needs_details = self.snapshot.needs_command_details(&self.choices);
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .child(
                div()
                    .px_5()
                    .pt_5()
                    .pb_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Review changes"),
                            )
                            .child(div().flex_1())
                            .child(
                                Button::new("back-to-selection")
                                    .ghost()
                                    .small()
                                    .label("Cancel review")
                                    .disabled(self.busy.is_some())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.invalidate_review();
                                        this.send(Command::CancelReview, "Returning to your saved selection", cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} changes · {} decisions needed",
                                self.snapshot.changes.len(),
                                unresolved
                            )),
                    ),
            )
            .when(self.snapshot.changes.iter().any(|change| change.id.contains(":files:")), |el| el.child(
                div().px_5().pb_4().flex().items_center().gap_3()
                    .child(Button::new("use-source-files").label("Use source for all file changes").disabled(self.busy.is_some())
                        .on_click(cx.listener(|this, _, _, cx| {
                            for change in &this.snapshot.changes {
                                if change.id.contains(":files:") && change.source_available { this.choices.insert(change.id.clone(), Choice::Source); }
                            }
                            cx.notify();
                        })))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Whole files will be copied, including hidden or truncated contents. Confirm sync is still required."))
            ))
            .when(self.snapshot.changes.iter().any(|change| change.path.starts_with("mcp.json")), |el| el.child(
                div().px_5().pb_4().flex().flex_col().gap_2()
                    .child(Button::new("reveal-command-details").label(if self.snapshot.command_details_reviewed { "Command details shown" } else { "Reveal command details" })
                        .disabled(self.busy.is_some() || self.snapshot.command_details_reviewed)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(plan_id) = this.snapshot.plan_id.clone() { this.send(Command::RevealDetails { plan_id }, "Reading command details for this review", cx); }
                        })))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child("May include secrets. Visible only on this computer. Review executable commands before applying MCP changes."))
            ))
            .child(
                div()
                    .id("review-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_5()
                    .pb_5()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .when(!self.snapshot.blocked_targets.is_empty(), |el| el.child(
                        div().id("blocked-review-notice").test_support().p_4().rounded(cx.theme().radius).border_1().border_color(cx.theme().danger).flex().flex_col().gap_2()
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(cx.theme().danger).child("Some targets could not be reviewed"))
                            .children(self.snapshot.blocked_targets.iter().map(|(device, reason)| div().child(format!("{device}: {reason}"))))))
                    .when(self.snapshot.changes.is_empty(), |el| {
                        el.child(
                            div().py_8().child(
                                if self.snapshot.blocked_targets.is_empty() { "Already in sync. The selected files were read successfully and no differences were found." } else { "Review incomplete. One or more targets could not be checked. This does not mean they are in sync." },
                            ),
                        )
                    })
                    .children(self.snapshot.changes.iter().map(|change| {
                        let id = change.id.clone();
                        let choice = self.choices.get(&id).copied().or(if change.conflict {
                            None
                        } else {
                            Some(Choice::Source)
                        });
                        let index = choice.map(|choice| match choice {
                            Choice::Source => 0,
                            Choice::Target => 1,
                            Choice::Skip => 2,
                        });
                        div()
                            .pb_5()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(change.device.clone()),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(change.path.clone()),
                            )
                            .when(change.conflict, |el| {
                                el.child(
                                    div()
                                        .mt_2()
                                        .text_sm()
                                        .text_color(cx.theme().warning)
                                        .child(if change.id.contains(":files:") { "Whole-file copy: choose which version to keep" } else { "Conflict: choose which value to keep" }),
                                )
                            })
                            .child(
                                div()
                                    .mt_3()
                                    .flex()
                                    .gap_3()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .p_3()
                                            .rounded(cx.theme().radius)
                                            .bg(cx.theme().muted)
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child("Current target"),
                                            )
                                            .child(
                                                div().mt_2().text_sm().child(change.before.clone()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .p_3()
                                            .rounded(cx.theme().radius)
                                            .bg(cx.theme().accent)
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child("Incoming source"),
                                            )
                                            .child(
                                                div().mt_2().text_sm().child(change.after.clone()),
                                            ),
                                    ),
                            )
                            .child(
                                div().mt_3().child(
                                    RadioGroup::horizontal(SharedString::from(format!("resolve-{id}")))
                                        .children([
                                            Radio::from("Use source").disabled(!change.source_available),
                                            Radio::from("Keep target"),
                                            Radio::from("Skip"),
                                        ])
                                        .selected_index(index)
                                        .disabled(self.busy.is_some())
                                        .on_change(cx.listener(move |this, index, _, cx| {
                                            this.choices.insert(
                                                id.clone(),
                                                match index {
                                                    0 => Choice::Source,
                                                    1 => Choice::Target,
                                                    _ => Choice::Skip,
                                                },
                                            );
                                            cx.notify();
                                        })),
                                ),
                            )
                    })),
            )
            .child(
                div()
                    .px_5()
                    .py_4()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if unresolved > 0 {
                                "Make each remaining decision before applying."
                            } else if needs_details {
                                "Reveal and review command details, or keep/skip MCP changes."
                            } else {
                                "Only the reviewed selection will be applied."
                            }),
                    )
                    .child(
                        Button::new("apply-reviewed")
                            .primary()
                            .label("Confirm sync…")
                            .disabled(
                                self.busy.is_some()
                                    || unresolved > 0
                                    || needs_details
                                    || self.snapshot.changes.is_empty(),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.confirm_apply(window, cx)),
                            ),
                    ),
            )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem = window.rem_size();
        div()
            .key_context("PiSync")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .text_sm()
            .on_action(cx.listener(|this, _: &RefreshDevices, _, cx| {
                if this.busy.is_none() {
                    this.invalidate_review();
                    this.send(Command::Refresh, "Refreshing devices", cx);
                }
            }))
            .on_action(cx.listener(|this, _: &PrepareReview, _, cx| this.prepare(cx)))
            .on_action(cx.listener(|this, _: &RepeatSelection, _, cx| {
                if !this.onboarding
                    && this.snapshot.plan_id.is_none()
                    && this.busy.is_none()
                    && this.snapshot.has_last_selection
                {
                    this.send(Command::RepeatLast, "Preparing a fresh review", cx);
                }
            }))
            .child(self.render_toolbar(cx))
            .when_some(self.error.clone(), |el, error| {
                el.child(
                    div()
                        .px_5()
                        .py_3()
                        .bg(cx.theme().muted)
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(if self.snapshot.plan_id.is_some() {
                        self.render_review(cx).into_any_element()
                    } else if self.onboarding {
                        self.render_onboarding(cx).into_any_element()
                    } else {
                        h_resizable("workspace-panes")
                            .child(
                                resizable_panel()
                                    .size(rem * 40.)
                                    .size_range(rem * 25. ..rem * 100.)
                                    .child(self.render_items(cx)),
                            )
                            .child(
                                resizable_panel()
                                    .size_range(rem * 25. ..rem * 100.)
                                    .child(self.render_queue(cx)),
                            )
                            .into_any_element()
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_5()
                    .py_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.busy.clone().unwrap_or_else(|| {
                        if self.onboarding {
                            "Save a connection, then choose Next"
                        } else if self.snapshot.plan_id.is_some() {
                            "Review only · no files have been written"
                        } else {
                            "Ready · your selection is remembered"
                        }
                        .into()
                    }))
                    .child(div().flex_1())
                    .child("SSH passwords are not saved"),
            )
    }
}

#[cfg(test)]
#[path = "workspace_test.rs"]
mod tests;

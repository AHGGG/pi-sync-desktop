//! The workspace owns selection and presentation; backend owns durable profiles,
//! device I/O, reviewed plans, conflict validation, and explicit application.
use crate::ui_model::*;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    dialog::DialogButtonProps,
    form::{Field, Form},
    input::{Input, InputState},
    radio::RadioGroup,
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
    }
    fn stage(&mut self, id: String, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.selected.insert(id);
        self.invalidate_review();
        cx.notify();
    }
    fn prepare(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.targets.is_empty() || self.selected.is_empty() {
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
                self.selected
                    .retain(|id| self.snapshot.items.iter().any(|item| &item.id == id));
                self.busy = None;
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
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
                    .child("Select  →  Review  →  Apply"),
            )
            .child(div().flex_1())
            .child(
                Button::new("repeat-last")
                    .ghost()
                    .label("Repeat last selection")
                    .disabled(self.busy.is_some() || !self.snapshot.has_last_selection)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.send(
                            Command::RepeatLast,
                            "Preparing a fresh review of your last selection",
                            cx,
                        )
                    })),
            )
            .child(
                Button::new("refresh")
                    .label("Refresh")
                    .disabled(self.busy.is_some())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.invalidate_review();
                        this.send(Command::Refresh, "Refreshing devices", cx);
                    })),
            )
    }

    fn render_devices(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy.is_some();
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .bg(cx.theme().sidebar)
            .child(
                div()
                    .px_4()
                    .pt_5()
                    .pb_3()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Source device"),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Choose where to copy from"),
                    ),
            )
            .child(
                div()
                    .id("device-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_4()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(self.snapshot.devices.iter().map(|device| {
                        let id = device.id.clone();
                        let active = self.snapshot.source_id.as_ref() == Some(&id);
                        let remove_id = id.clone();
                        let remove_name = device.name.clone();
                        div()
                            .id(SharedString::from(format!("device-{id}")))
                            .p_3()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(if active {
                                cx.theme().primary
                            } else {
                                cx.theme().border
                            })
                            .bg(if active {
                                cx.theme().accent
                            } else {
                                cx.theme().background
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(device.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(if device.id == "public-harness" { "GitHub" } else if device.is_local { "Local" } else { "SSH" }),
                                    ),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(device.address.clone()),
                            )
                            .child(div().mt_2().text_xs().child(device.status.clone()))
                            .child(
                                Button::new(SharedString::from(format!("source-{id}")))
                                    .small()
                                    .mt_3()
                                    .w_full()
                                    .label(if active {
                                        "Selected source"
                                    } else {
                                        "Use as source"
                                    })
                                    .selected(active)
                                    .disabled(busy || active)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.selected.clear();
                                        this.targets.remove(&id);
                                        this.invalidate_review();
                                        this.send(
                                            Command::SelectSource(id.clone()),
                                            "Reading source configuration",
                                            cx,
                                        );
                                    })),
                            )
                            .when(!device.is_local && device.id != "public-harness", |el| el.child(
                                Button::new(SharedString::from(format!("remove-device-{remove_id}")))
                                    .ghost().small().mt_2().label("Remove profile…").disabled(busy)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let sender = this.commands.clone();
                                        let id = remove_id.clone();
                                        let name = remove_name.clone();
                                        window.open_alert_dialog(cx, move |dialog, _, _| {
                                            let sender = sender.clone(); let id = id.clone();
                                            dialog.title(format!("Remove {name}?")).confirm()
                                                .button_props(DialogButtonProps::default().ok_text("Remove profile"))
                                                .child("Only the saved connection profile is removed. Files on this device are not changed.")
                                                .on_ok(move |_, _, _| sender.send(Command::RemoveDevice(id.clone())).is_ok())
                                        });
                                    }))
                            ))
                    })),
            )
            .child(
                div()
                    .p_4()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("add-device")
                            .w_full()
                            .label("Add SSH device…")
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add_device(window, cx)),
                            ),
                    ),
            )
    }

    fn render_items(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy.is_some();
        div().flex().flex_col().size_full().min_w_0()
            .child(div().px_5().pt_5().pb_4()
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("Configuration library"))
                .child(div().mt_1().text_sm().text_color(cx.theme().muted_foreground).child("Drag items to the queue, or select them below.")))
            .child(div().id("configuration-library").flex_1().min_h_0().overflow_y_scroll().px_5().pb_5().flex().flex_col().gap_3()
                .when(self.snapshot.items.is_empty(), |el| el.child(
                    div().py_8().flex().flex_col().gap_2()
                        .child(div().font_weight(FontWeight::MEDIUM).child(if busy { "Reading configuration" } else { "No configuration items found" }))
                        .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Choose a device with Pi configuration, or check its agent directory and refresh."))))
                .children(self.snapshot.items.iter().map(|item| {
                    let id = item.id.clone(); let checked = self.selected.contains(&id);
                    let unavailable = id == "permissions" || (id == "models" && self.snapshot.source_id.as_deref() == Some("public-harness"));
                    let drag = DragItem { id: id.clone(), name: item.name.clone() };
                    div().id(SharedString::from(format!("config-{id}"))).test_support().p_4().rounded(cx.theme().radius)
                        .border_1().border_color(if checked { cx.theme().primary } else { cx.theme().border })
                        .bg(if checked { cx.theme().accent } else { cx.theme().background })
                        .when(!busy && !unavailable, |el| el.on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone())))
                        .child(div().flex().items_center().gap_3()
                            .child(Checkbox::new(SharedString::from(format!("select-{id}"))).label(item.name.clone()).checked(checked).disabled(busy || unavailable)
                                .on_change(cx.listener(move |this, checked, _, cx| {
                                    if *checked { this.selected.insert(id.clone()); } else { this.selected.remove(&id); }
                                    this.invalidate_review(); cx.notify();
                                })))
                            .child(div().flex_1())
                            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(item.category.clone())))
                        .child(div().mt_2().text_sm().text_color(cx.theme().muted_foreground).child(item.detail.clone()))
                        .when(unavailable, |el| el.child(div().mt_2().text_xs().text_color(cx.theme().muted_foreground).child("Not available from this source")))
                })))
            .child(div().px_5().py_3().border_t_1().border_color(cx.theme().border).text_xs().text_color(cx.theme().muted_foreground).child("Authentication files and permission policies are excluded. Private models and MCP command arguments may contain credentials."))
    }

    fn render_queue(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy.is_some();
        let eligible = self
            .snapshot
            .devices
            .iter()
            .filter(|d| d.id != "public-harness" && Some(&d.id) != self.snapshot.source_id.as_ref())
            .collect::<Vec<_>>();
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .bg(cx.theme().sidebar)
            .child(
                div()
                    .px_4()
                    .pt_5()
                    .pb_3()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Sync queue"))
                    .child(
                        div()
                            .mt_1()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Nothing changes until you confirm."),
                    ),
            )
            .child(
                div()
                    .id("queue-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_4()
                    .pb_4()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .id("queue-drop-zone")
                            .test_support()
                            .p_4()
                            .min_h_24()
                            .border_1()
                            .border_color(cx.theme().border)
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().background)
                            .on_drop(cx.listener(|this, item: &DragItem, _, cx| {
                                this.stage(item.id.clone(), cx)
                            }))
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(format!("{} selected items", self.selected.len())),
                            )
                            .when(self.selected.is_empty(), |el| {
                                el.child(
                                    div()
                                        .mt_2()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Drop configuration items here"),
                                )
                            })
                            .children(
                                self.snapshot
                                    .items
                                    .iter()
                                    .filter(|item| self.selected.contains(&item.id))
                                    .map(|item| {
                                        let id = item.id.clone();
                                        div()
                                            .mt_2()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div().flex_1().text_sm().child(item.name.clone()),
                                            )
                                            .child(
                                                Button::new(SharedString::from(format!(
                                                    "remove-{id}"
                                                )))
                                                .ghost()
                                                .small()
                                                .label("Remove")
                                                .disabled(busy)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.selected.remove(&id);
                                                    this.invalidate_review();
                                                    cx.notify();
                                                })),
                                            )
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Target devices"),
                            )
                            .when(eligible.is_empty(), |el| {
                                el.child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Add another device to start a sync."),
                                )
                            })
                            .children(eligible.into_iter().map(|device| {
                                let id = device.id.clone();
                                div()
                                    .pb_3()
                                    .border_b_1()
                                    .border_color(cx.theme().border)
                                    .child(
                                        Checkbox::new(SharedString::from(format!("target-{id}")))
                                            .label(device.name.clone())
                                            .checked(self.targets.contains(&id))
                                            .disabled(busy)
                                            .on_change(cx.listener(move |this, checked, _, cx| {
                                                if *checked {
                                                    this.targets.insert(id.clone());
                                                } else {
                                                    this.targets.remove(&id);
                                                }
                                                this.invalidate_review();
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        div()
                                            .mt_1()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(device.status.clone()),
                                    )
                            })),
                    )
                    .when(!self.snapshot.results.is_empty(), |el| {
                        el.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("Device results"),
                                )
                                .children(self.snapshot.results.iter().map(|(device, status)| {
                                    div()
                                        .text_sm()
                                        .child(
                                            div()
                                                .font_weight(FontWeight::MEDIUM)
                                                .child(device.clone()),
                                        )
                                        .child(
                                            div()
                                                .mt_1()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(status.clone()),
                                        )
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .p_4()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("prepare-review")
                            .w_full()
                            .label("Review changes")
                            .disabled(busy || self.targets.is_empty() || self.selected.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.prepare(cx))),
                    ),
            )
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
                && (change.path.starts_with("models.json") || change.path.starts_with("mcp.json"))
        });

        let sender = self.commands.clone();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let sender = sender.clone(); let plan_id = plan_id.clone(); let choices = choices.clone();
            dialog.title("Apply reviewed changes?").confirm()
                .button_props(DialogButtonProps::default().ok_text("Apply changes"))
                .child(format!("Apply {writes} reviewed changes to the selected target devices. Target files are checked again before writing. Previous versions of changed files are backed up; offline or changed targets are reported separately."))
                .child(format!("Targets: {targets}"))
                .when(private_transfer, |dialog| dialog.child("This selection includes private models or MCP command arguments that may contain credentials. Applying copies these values to the named targets."))
                .on_ok(move |_, _, _| sender.send(Command::Apply { plan_id: plan_id.clone(), choices: choices.clone() }).is_ok())
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
                                    .label("Back to selection")
                                    .disabled(self.busy.is_some())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.invalidate_review();
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} changes · {} unresolved conflicts",
                                self.snapshot.changes.len(),
                                unresolved
                            )),
                    ),
            )
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
                    .when(self.snapshot.changes.is_empty(), |el| {
                        el.child(
                            div().py_8().child(
                                if self.snapshot.results.is_empty() { "These selections are already in sync. No changes to apply." } else { "No applicable changes. Review the device results in the queue." },
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
                                        .child("Conflict: choose which value to keep"),
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
                                    RadioGroup::new(SharedString::from(format!("resolve-{id}")))
                                        .children(["Use source", "Keep target", "Skip"])
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
                                "Resolve each conflict before applying."
                            } else if needs_details {
                                "Reveal and review command details, or keep/skip MCP changes."
                            } else {
                                "Only the reviewed selection will be applied."
                            }),
                    )
                    .child(
                        Button::new("apply-reviewed")
                            .primary()
                            .label("Apply reviewed changes…")
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
                if this.busy.is_none() && this.snapshot.has_last_selection {
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
                div().flex_1().min_h_0().child(
                    h_resizable("workspace-panes")
                        .child(
                            resizable_panel()
                                .size(rem * 17.)
                                .size_range(rem * 14. ..rem * 24.)
                                .child(self.render_devices(cx)),
                        )
                        .child(resizable_panel().size_range(rem * 24. ..rem * 100.).child(
                            if self.snapshot.plan_id.is_some() {
                                self.render_review(cx).into_any_element()
                            } else {
                                self.render_items(cx).into_any_element()
                            },
                        ))
                        .child(
                            resizable_panel()
                                .size(rem * 21.)
                                .size_range(rem * 18. ..rem * 30.)
                                .child(self.render_queue(cx)),
                        ),
                ),
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
                        "Ready · Select items and targets to prepare a review".into()
                    }))
                    .child(div().flex_1())
                    .child("SSH passwords are not saved"),
            )
    }
}

#[cfg(test)]
#[path = "workspace_test.rs"]
mod tests;

//! The workspace owns selection and presentation; backend owns durable profiles,
//! device I/O, reviewed plans, conflict validation, and explicit application.
use crate::ui_model::*;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, WindowExt,
    button::{Button, ButtonCustomVariant, ButtonVariants},
    checkbox::Checkbox,
    dialog::DialogButtonProps,
    diff::{
        Diff, DiffDecoration, DiffFile, DiffHunkSeparator, DiffLinePosition, DiffLineRange,
        DiffMode, DiffSide, DiffState,
    },
    form::{Field, Form},
    input::{Input, InputState},
    resizable::{h_resizable, resizable_panel},
    tooltip::Tooltip,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::mpsc::{Receiver, Sender, TryRecvError},
    time::Duration,
};

actions!(
    pi_sync,
    [
        RefreshDevices,
        PrepareReview,
        RepeatSelection,
        SelectAllSourceChanges,
        ChooseReviewSource,
        ChooseReviewTarget,
        PreviousReviewHunk,
        NextReviewHunk
    ]
);

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

/// Navigation is retained independently of scroll position: multiple hunks can
/// fit in the viewport, and scrolling to a near-bottom hunk can be clamped.
struct ReviewDiff {
    state: Entity<DiffState>,
    hunks: Vec<Vec<DiffLineRange>>,
    active: usize,
}

struct ReviewDestination {
    change_id: String,
    hunk: Option<usize>,
}

impl ReviewDiff {
    fn can_move(&self, forward: bool) -> bool {
        if forward {
            self.active + 1 < self.hunks.len()
        } else {
            self.active > 0
        }
    }
}

/// A version choice must not consume text-selection or modifier gestures.
fn review_content_click(event: &ClickEvent) -> bool {
    match event {
        ClickEvent::Mouse(mouse) => {
            let moved = mouse.up.position - mouse.down.position;
            mouse.down.button == MouseButton::Left
                && mouse.down.click_count == 1
                && mouse.down.modifiers == Modifiers::default()
                && mouse.up.modifiers == Modifiers::default()
                && moved.x.abs() <= px(3.)
                && moved.y.abs() <= px(3.)
        }
        ClickEvent::Touch(_) => true,
        ClickEvent::Keyboard(_) => false,
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
    authentication: Option<(String, Option<Entity<InputState>>)>,
    focus: FocusHandle,
    onboarding: bool,
    initial_snapshot_received: bool,
    source_picker_open: bool,
    review_change_id: Option<String>,
    review_pending_only: bool,
    review_hovered_side: Option<Choice>,
    review_hover_owner: Option<&'static str>,
    review_diffs: BTreeMap<String, ReviewDiff>,
    review_pointer: Option<(String, Point<Pixels>, bool)>,
    review_pointer_blocked: bool,
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
            KeyBinding::new("alt-left", ChooseReviewSource, Some("PiSync")),
            KeyBinding::new("alt-right", ChooseReviewTarget, Some("PiSync")),
            KeyBinding::new("alt-up", PreviousReviewHunk, Some("PiSync")),
            KeyBinding::new("alt-down", NextReviewHunk, Some("PiSync")),
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
            authentication: None,
            focus,
            onboarding: false,
            initial_snapshot_received: false,
            source_picker_open: false,
            review_change_id: None,
            review_pending_only: false,
            review_hovered_side: None,
            review_hover_owner: None,
            review_diffs: BTreeMap::new(),
            review_pointer: None,
            review_pointer_blocked: true,
            setup_name: cx.new(|cx| InputState::new(window, cx).placeholder("Work laptop")),
            setup_ssh: cx
                .new(|cx| InputState::new(window, cx).placeholder("ssh [user@]host-or-alias")),
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
        self.review_diffs.clear();
        self.review_pointer = None;
        self.review_pointer_blocked = true;
        self.review_change_id = None;
        self.review_pending_only = false;
        self.review_hovered_side = None;
        self.review_hover_owner = None;
        self.snapshot.plan_id = None;
        self.snapshot.changes.clear();
        self.snapshot.text_previews.clear();
        self.snapshot.file_review_details.clear();
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
                // Do not complete a press against preview content replaced mid-gesture.
                self.review_pointer = None;
                self.review_pointer_blocked = true;
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
                    self.review_change_id = None;
                    self.review_pending_only = false;
                    self.review_hovered_side = None;
                    self.review_hover_owner = None;
                    self.choices.clear();
                }
                let same_plan = snapshot.plan_id == self.snapshot.plan_id;
                self.review_diffs = snapshot
                    .changes
                    .iter()
                    .filter_map(|change| {
                        if !change.source_available || change.path.starts_with("models.json") {
                            return None;
                        }
                        let (before, after) = if change.id.contains(":files:") {
                            {
                                let (before, after) = snapshot.text_previews.get(&change.id)?;
                                (before, after)
                            }
                        } else {
                            (&change.before, &change.after)
                        };
                        // Preserve navigation, text selection and scroll for an unchanged
                        // preview (for example when command details are revealed elsewhere).
                        if same_plan
                            && self.snapshot.changes.iter().any(|old| {
                                old.id == change.id
                                    && old.path == change.path
                                    && old.before == change.before
                                    && old.after == change.after
                                    && old.source_available == change.source_available
                            })
                            && self.snapshot.text_previews.get(&change.id)
                                == snapshot.text_previews.get(&change.id)
                            && let Some(diff) = self.review_diffs.remove(&change.id)
                        {
                            return Some((change.id.clone(), diff));
                        }
                        // Constant patch filenames keep path/control characters out of the parser.
                        // Full domain paths remain visible in the application's own headers.
                        let extension = if change.id.contains(":files:") {
                            change.path.rsplit('.').next().unwrap_or("txt")
                        } else {
                            "json"
                        };
                        let extension = if extension.chars().all(|c| c.is_ascii_alphanumeric()) {
                            extension
                        } else {
                            "txt"
                        };
                        let name = format!("preview.{extension}");
                        let text_diff = similar::TextDiff::configure()
                            .timeout(Duration::from_millis(20))
                            .diff_lines(after.as_str(), before.as_str());
                        let patch = text_diff
                            .unified_diff()
                            .context_radius(3)
                            .header(&name, &name)
                            .to_string();
                        let files = DiffFile::parse(&patch).ok()?;
                        if files.is_empty() {
                            return None;
                        }
                        let path = files[0].path().clone();
                        let hunks = text_diff
                            .grouped_ops(3)
                            .into_iter()
                            .filter_map(|ops| {
                                let ranges: Vec<_> = ops
                                    .iter()
                                    .filter(|op| op.tag() != similar::DiffTag::Equal)
                                    .flat_map(|op| {
                                        [
                                            (DiffSide::Original, op.old_range()),
                                            (DiffSide::Modified, op.new_range()),
                                        ]
                                    })
                                    .filter(|(_, lines)| !lines.is_empty())
                                    .map(|(side, lines)| {
                                        DiffLineRange::new(
                                            path.clone(),
                                            side,
                                            lines.start + 1,
                                            lines.end,
                                        )
                                    })
                                    .collect();
                                (!ranges.is_empty()).then_some(ranges)
                            })
                            .collect();
                        Some((
                            change.id.clone(),
                            ReviewDiff {
                                state: cx
                                    .new(|cx| DiffState::new(files, cx).with_mode(DiffMode::Split)),
                                hunks,
                                active: 0,
                            },
                        ))
                    })
                    .collect();
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
                self.close_authentication(window, cx);
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
            Event::AuthenticationFinished { request_id } => {
                if self
                    .authentication
                    .as_ref()
                    .is_some_and(|(id, _)| id == &request_id)
                {
                    self.close_authentication(window, cx);
                }
            }
        }
        cx.notify();
    }

    fn close_authentication(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, secret)) = self.authentication.take() {
            if let Some(secret) = secret {
                secret.update(cx, |input, cx| input.set_value("", window, cx));
            }
            window.close_dialog(cx);
        }
    }

    fn add_device(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Workstation"));
        let ssh = cx.new(|cx| InputState::new(window, cx).placeholder("ssh [user@]host-or-alias"));
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
                .child("Use ssh [user@]host-or-alias. OpenSSH configuration supplies omitted user and port. Passwords and passphrases are requested separately and are never saved in profiles.")
                .child(format!("SSH client: {}", sync_core::transport::ssh_client_description()))
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
        self.close_authentication(window, cx);
        self.authentication = Some((request_id.clone(), None));
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
        self.close_authentication(window, cx);
        self.authentication = Some((request_id.clone(), Some(secret.clone())));
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
            .when(reviewing, |el| el.py_2())
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
                        "Review changes"
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
                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Use ssh [user@]host-or-alias. OpenSSH configuration supplies omitted user and port."))
                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child(format!("SSH client: {}", sync_core::transport::ssh_client_description())))
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
            "None; all target versions are kept".to_string()
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
                .when(private_transfer, |dialog| dialog.child("Selected source files replace entire target files. This selection includes whole files or MCP values that may contain private data. Extension code can execute in Pi; standalone permission policies can change tool access. Hidden or truncated contents are still copied in full to the named targets."))
                .on_ok(move |_, _, cx| entity.update(cx, |this, cx| {
                    if this.busy.is_some() || this.snapshot.plan_id.as_ref() != Some(&plan_id) { return false; }
                    this.send(Command::Apply { plan_id: plan_id.clone(), choices: choices.clone() }, "Applying the reviewed changes", cx);
                    true
                }).unwrap_or(false))
        });
    }

    fn review_choice(&self, change: &Change) -> Option<Choice> {
        self.choices
            .get(&change.id)
            .copied()
            .or_else(|| (!change.conflict && change.source_available).then_some(Choice::Source))
    }

    fn review_needs_decision(&self, change: &Change) -> bool {
        self.review_choice(change).is_none()
            || (!change.source_available && self.review_choice(change) == Some(Choice::Source))
    }

    fn review_device_name(device: &Device) -> String {
        if device.is_local && !device.name.eq_ignore_ascii_case("This computer") {
            format!("{} · this computer", device.name)
        } else {
            device.name.clone()
        }
    }

    fn choose_review_side(&mut self, id: &str, choice: Choice, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.snapshot.plan_id.is_none() {
            return;
        }
        let Some(change) = self.snapshot.changes.iter().find(|change| change.id == id) else {
            return;
        };
        if choice == Choice::Source && !change.source_available {
            return;
        }
        // This only stages a decision. All writes still require Confirm sync.
        self.choices.insert(id.to_owned(), choice);
        cx.notify();
    }

    fn review_item_visible(&self, change: &Change) -> bool {
        !self.review_pending_only || self.review_needs_decision(change)
    }

    fn active_review_change(&self) -> Option<&Change> {
        let visible = |change: &&Change| self.review_item_visible(change);
        self.snapshot
            .changes
            .iter()
            .filter(visible)
            .find(|change| self.review_change_id.as_ref() == Some(&change.id))
            .or_else(|| self.snapshot.changes.iter().find(visible))
    }

    /// Traverse hunks before crossing the boundary to the adjacent visible item.
    /// Resolve by change ID, not path: different targets can review the same file.
    fn review_destination(&self, id: &str, forward: bool) -> Option<ReviewDestination> {
        if self.active_review_change()?.id != id {
            return None;
        }
        if let Some(diff) = self.review_diffs.get(id)
            && diff.can_move(forward)
        {
            return Some(ReviewDestination {
                change_id: id.to_owned(),
                hunk: Some(if forward {
                    diff.active + 1
                } else {
                    diff.active - 1
                }),
            });
        }
        let changes = &self.snapshot.changes;
        let current = changes.iter().position(|change| change.id == id)?;
        let next = if forward {
            changes[current + 1..]
                .iter()
                .find(|change| self.review_item_visible(change))
        } else {
            changes[..current]
                .iter()
                .rev()
                .find(|change| self.review_item_visible(change))
        }?;
        Some(ReviewDestination {
            change_id: next.id.clone(),
            hunk: self.review_diffs.get(&next.id).and_then(|diff| {
                if forward {
                    (!diff.hunks.is_empty()).then_some(0)
                } else {
                    diff.hunks.len().checked_sub(1)
                }
            }),
        })
    }

    fn review_navigation_label(&self, id: &str, forward: bool) -> String {
        match self.review_destination(id, forward) {
            Some(destination) if destination.change_id == id => if forward {
                "Next hunk in this item"
            } else {
                "Previous hunk in this item"
            }
            .into(),
            Some(destination) => {
                let change = self
                    .snapshot
                    .changes
                    .iter()
                    .find(|change| change.id == destination.change_id)
                    .unwrap();
                format!(
                    "{} item: {} · {}",
                    if forward { "Next" } else { "Previous" },
                    change.path,
                    change.device
                )
            }
            None => if forward {
                "End of visible review"
            } else {
                "Start of visible review"
            }
            .into(),
        }
    }

    fn navigate_review(&mut self, id: &str, forward: bool, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.snapshot.plan_id.is_none() {
            return;
        }
        let Some(destination) = self.review_destination(id, forward) else {
            return;
        };
        if destination.change_id != id {
            self.review_change_id = Some(destination.change_id.clone());
            // A press begun on the old item cannot select content on the new one.
            self.review_pointer = None;
            self.review_pointer_blocked = true;
        }
        if let Some(hunk) = destination.hunk
            && let Some(diff) = self.review_diffs.get_mut(&destination.change_id)
        {
            diff.active = hunk;
            if let Some(range) = diff.hunks[hunk].first() {
                let position =
                    DiffLinePosition::new(range.path().clone(), range.side(), range.start());
                diff.state
                    .update(cx, |state, cx| state.scroll_to_line(position, cx));
            }
        }
        cx.notify();
    }

    fn activate_review_edge(&mut self, id: &str, side: Choice, cx: &mut Context<Self>) {
        // An implicit default is not a first click. The edge always chooses
        // before it navigates. A destination item needs its own explicit choice.
        if self
            .active_review_change()
            .is_none_or(|change| change.id != id)
        {
            return;
        }
        if self.choices.get(id) == Some(&side) {
            self.navigate_review(id, side == Choice::Target, cx);
        } else {
            self.choose_review_side(id, side, cx);
        }
    }

    fn review_keyboard(
        &mut self,
        side: Option<Choice>,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let Some(id) = self.active_review_change().map(|change| change.id.clone()) else {
            return;
        };
        if let Some(side) = side {
            self.choose_review_side(&id, side, cx);
        } else {
            self.navigate_review(&id, forward, cx);
        }
    }

    fn select_all_source_changes(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.snapshot.plan_id.is_none() {
            return;
        }
        for change in &self.snapshot.changes {
            if change.source_available {
                self.choices.insert(change.id.clone(), Choice::Source);
            }
        }
        // Unavailable sources remain unchanged, requiring an explicit keep-target
        // decision if unresolved. MCP command-detail review is still enforced.
        cx.notify();
    }

    fn hover_review_side(
        &mut self,
        owner: &'static str,
        side: Option<Choice>,
        cx: &mut Context<Self>,
    ) {
        if side.is_some() {
            if self.review_hover_owner != Some(owner) || self.review_hovered_side != side {
                self.review_hover_owner = Some(owner);
                self.review_hovered_side = side;
                cx.notify();
            }
        } else if self.review_hover_owner == Some(owner) {
            self.review_hover_owner = None;
            self.review_hovered_side = None;
            cx.notify();
        }
    }

    fn render_review_detail(&self, change: &Change, cx: &mut Context<Self>) -> impl IntoElement {
        let source = self
            .snapshot
            .devices
            .iter()
            .find(|device| self.snapshot.source_id.as_ref() == Some(&device.id));
        // Use the stable backend change prefix, never a potentially duplicate display name.
        let target = self
            .snapshot
            .devices
            .iter()
            .find(|device| change.id.starts_with(&format!("{}:", device.id)));
        let source_name = source
            .map(Self::review_device_name)
            .unwrap_or_else(|| "Selected source".into());
        let target_name = target
            .map(Self::review_device_name)
            .unwrap_or_else(|| change.device.clone());
        let destination_name = target
            .map(|device| device.name.clone())
            .unwrap_or_else(|| change.device.clone());
        let whole_file = change.id.contains(":files:") || change.path.starts_with("models.json");
        let diff = self.review_diffs.get(&change.id);
        let previous = self.review_destination(&change.id, false);
        let next = self.review_destination(&change.id, true);
        let previous_label = self.review_navigation_label(&change.id, false);
        let next_label = self.review_navigation_label(&change.id, true);
        let position_label = if let Some(diff) = diff {
            format!("Hunk {} of {}", diff.active + 1, diff.hunks.len())
        } else if self
            .snapshot
            .text_previews
            .get(&change.id)
            .is_some_and(|(before, after)| before == after)
            || (!whole_file && change.before == change.after)
        {
            "No text differences".into()
        } else {
            "No navigable text preview".into()
        };
        let unavailable = self
            .snapshot
            .changes
            .iter()
            .filter(|change| !change.source_available)
            .count();
        let eligible = self.snapshot.changes.len() - unavailable;
        let click_id = change.id.clone();
        let down_id = change.id.clone();
        let measured_bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
        let paint_bounds = measured_bounds.clone();
        let click_bounds = measured_bounds.clone();
        let pointer_entity = cx.entity().downgrade();
        let permission_policy = self
            .snapshot
            .file_review_details
            .get(&change.id)
            .is_some_and(|details| details.permission_policy);
        div().flex().flex_col().size_full().min_w_0().min_h_0()
            .child(div().px_4().py_2().border_b_1().border_color(cx.theme().border)
                .flex().items_center().gap_2()
                .child(div().flex_1().min_w_0().font_weight(FontWeight::SEMIBOLD).child(change.path.clone()))
                .when(permission_policy, |el| el.child(div().id("permission-policy-warning").test_support()
                    .text_xs().text_color(cx.theme().warning)
                    .aria_label("Permission policy change: may grant or remove tool access")
                    .tooltip(|window, cx| Tooltip::new("This whole-file policy replacement may grant or remove tool access.").build(window, cx))
                    .child("Permission policy")))
                .when(!whole_file, |el| el.child(div().text_xs().text_color(cx.theme().muted_foreground).child("Value snippet")))
                .when(whole_file && diff.is_none(), |el| el.child(div().text_xs().text_color(cx.theme().muted_foreground).child("Safe preview")))
                .child(div().id("review-hunk-position").test_support().text_xs().text_color(cx.theme().muted_foreground)
                    .aria_label(position_label.clone()).child(position_label)
                    .tooltip(|window, cx| Tooltip::new("Navigate through hunks, then continue to the next or previous visible review item.").build(window, cx)))
                .when(diff.is_some() || previous.is_some() || next.is_some(), |el| {
                    let previous_id = change.id.clone();
                    let next_id = change.id.clone();
                    el.child(Button::new("previous-diff-hunk").small().ghost().label("↑")
                        .accessibility_label(previous_label.clone()).tooltip(format!("{previous_label} · Alt+↑"))
                        .disabled(self.busy.is_some() || previous.is_none())
                        .on_click(cx.listener(move |this, _, _, cx| this.navigate_review(&previous_id, false, cx))))
                      .child(Button::new("next-diff-hunk").small().ghost().label("↓")
                        .accessibility_label(next_label.clone()).tooltip(format!("{next_label} · Alt+↓"))
                        .disabled(self.busy.is_some() || next.is_none())
                        .on_click(cx.listener(move |this, _, _, cx| this.navigate_review(&next_id, true, cx))))
                })
                .child(Button::new("use-source-all").small().label("Use source for all changes →")
                    .tooltip(format!("Select source for {eligible} changes. {unavailable} unavailable sources are excluded. Nothing is written until you confirm sync."))
                    .disabled(self.busy.is_some() || eligible == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.select_all_source_changes(cx)))))
            .child(div().flex().items_stretch()
                .child(self.review_column_header(change, Choice::Source, source_name, source.map(|d| d.address.clone()).unwrap_or_default(), destination_name.clone(), cx))
                .child(self.review_column_header(change, Choice::Target, target_name, target.map(|d| d.address.clone()).unwrap_or_default(), destination_name, cx)))
            .child(div().relative().flex().flex_1().min_h_0().min_w_0()
                .child(div().id("review-code-surface").test_support().relative().flex_1().min_h_0().min_w_0()
                    .capture_any_mouse_down(cx.listener(move |this, event: &MouseDownEvent, _, _| {
                        this.review_pointer = Some((down_id.clone(), event.position, false));
                        // A nested control that claims the press leaves it blocked.
                        this.review_pointer_blocked = true;
                    }))
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                        this.review_pointer_blocked = gpui_kit::base::GlobalState::is_text_selection_suppressed(cx);
                    }))
                    .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                        if let Some(bounds) = measured_bounds.get() {
                            let side = if event.position.x < bounds.left() + bounds.size.width / 2. { Choice::Source } else { Choice::Target };
                            this.hover_review_side("code", Some(side), cx);
                        }
                    }))
                    .on_scroll(cx.listener(|this, _, _, _| {
                        this.review_pointer_blocked = true;
                    }))
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if !*hovered {
                            if let Some((_, _, moved)) = &mut this.review_pointer { *moved = true; }
                            this.hover_review_side("code", None, cx);
                        }
                    }))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        if !review_content_click(event) { return; }
                        if matches!(event, ClickEvent::Mouse(_)) && (this.review_pointer_blocked
                            || !this.review_pointer.as_ref().is_some_and(|(id, _, moved)| id == &click_id && !moved)) { return; }
                        // Native gutter/text selection is not a version decision.
                        // Read the current selection rather than retained line metadata,
                        // which clears asynchronously after a new plain click.
                        if this.review_diffs.get(&click_id).is_some_and(|diff|
                            !diff.state.read(cx).selected_text(cx).is_empty()) { return; }
                        if let Some(bounds) = click_bounds.get() {
                            let side = if event.position().x < bounds.left() + bounds.size.width / 2. { Choice::Source } else { Choice::Target };
                            this.choose_review_side(&click_id, side, cx);
                        }
                    }))
                    .child(canvas(move |bounds, _, _| paint_bounds.set(Some(bounds)), move |_, _, window, _| {
                        // Capture movement across the whole window, including an
                        // outside-and-back text drag that leaves this pane.
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase.capture() && event.pressed_button == Some(MouseButton::Left) {
                                let _ = pointer_entity.update(cx, |this, _| {
                                    if let Some((_, origin, moved)) = &mut this.review_pointer {
                                        let delta = event.position - *origin;
                                        *moved |= delta.x.abs() > px(3.) || delta.y.abs() > px(3.);
                                    }
                                });
                            }
                        });
                    }).absolute().inset_0())
                    .child(if let Some(diff) = diff {
                        Diff::new(&diff.state).size_full().header_visible(false).line_number(true)
                            .hunk_separator(DiffHunkSeparator::Simple).change_background(true).soft_wrap(true)
                            .decorations(diff.hunks.get(diff.active).into_iter().flatten().cloned()
                                .map(|range| DiffDecoration::new(range).with_background(cx.theme().primary.opacity(0.14))))
                            .into_any_element()
                    } else {
                        div().id(SharedString::from(format!("opaque-preview-{}", change.id))).test_support()
                            .size_full().overflow_y_scroll().child(div().flex().items_stretch().min_h_full()
                                .child(self.review_opaque_side(change, Choice::Source, change.after.clone(), cx))
                                .child(self.review_opaque_side(change, Choice::Target, change.before.clone(), cx)))
                            .into_any_element()
                    }))
                .child(self.review_edge(change, Choice::Source, cx))
                .child(self.review_edge(change, Choice::Target, cx)))
            .when(!change.source_available, |el| el.child(div().px_4().py_2().border_t_1().border_color(cx.theme().border)
                .text_xs().text_color(cx.theme().danger)
                .child("Source unavailable. Choose the target version.")))
    }

    fn review_edge(
        &self,
        change: &Change,
        side: Choice,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let source = side == Choice::Source;
        let selected = self.choices.get(&change.id) == Some(&side);
        let can_move = self.review_destination(&change.id, !source).is_some();
        let label = if selected {
            self.review_navigation_label(&change.id, !source)
        } else if source {
            "Choose source version first".into()
        } else {
            "Choose target version first".into()
        };
        let id = change.id.clone();
        let hovered = self.review_hovered_side == Some(side);
        let hover_owner = if source { "source-edge" } else { "target-edge" };
        div()
            .id(if source {
                "review-source-overlay"
            } else {
                "review-target-overlay"
            })
            .absolute()
            .top(relative(0.5))
            .mt(px(-28.))
            .w(px(40.))
            .h(px(56.))
            .when(source, |el| el.left(px(8.)))
            // Keep the native vertical scrollbar unobstructed.
            .when(!source, |el| el.right(px(16.)))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.hover_review_side(hover_owner, (*hovered).then_some(side), cx);
            }))
            .child(
                Button::new(if source {
                    "review-source-edge"
                } else {
                    "review-target-edge"
                })
                .custom(
                    ButtonCustomVariant::new(cx)
                        .color(cx.theme().background.opacity(0.76))
                        .foreground(cx.theme().foreground.opacity(0.92))
                        .hover(cx.theme().muted.opacity(0.82))
                        .active(cx.theme().muted.opacity(0.94)),
                )
                .rounded(px(8.))
                .border_1()
                .border_color(cx.theme().border.opacity(0.7))
                .size_full()
                .p_0()
                .opacity(if hovered { 1. } else { 0. })
                .focus_visible(|style| style.opacity(1.))
                .child(
                    Icon::new(if source {
                        IconName::ChevronLeft
                    } else {
                        IconName::ChevronRight
                    })
                    .size(px(24.)),
                )
                .accessibility_label(label.clone())
                .tooltip(label)
                .disabled(
                    self.busy.is_some()
                        || (source && !change.source_available)
                        || (selected && !can_move),
                )
                .on_click(cx.listener(move |this, event, _, cx| {
                    // Edge double-clicks are intentional; dragging the edge is not.
                    if let ClickEvent::Mouse(mouse) = event {
                        let delta = mouse.up.position - mouse.down.position;
                        if delta.x.abs() > px(3.)
                            || delta.y.abs() > px(3.)
                            || mouse.down.modifiers != Modifiers::default()
                            || mouse.up.modifiers != Modifiers::default()
                        {
                            return;
                        }
                    }
                    this.activate_review_edge(&id, side, cx);
                })),
            )
    }

    fn review_column_header(
        &self,
        change: &Change,
        side: Choice,
        name: String,
        address: String,
        destination_name: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.review_choice(change) == Some(side);
        let source = side == Choice::Source;
        let disabled = self.busy.is_some() || (source && !change.source_available);
        let hovered = self.review_hovered_side == Some(side) && !disabled;
        let role = if source { "Source" } else { "Target" };
        let action = if source {
            format!("Use on {destination_name} →")
        } else {
            format!("Keep on {destination_name}")
        };
        let cue = if source && !change.source_available {
            "Unavailable".to_string()
        } else if hovered {
            action.clone()
        } else if selected {
            "Selected".to_string()
        } else {
            String::new()
        };
        let cue = if source && !change.source_available {
            cue
        } else if selected {
            format!("✓ {cue}")
        } else if hovered {
            format!("○ {cue}")
        } else {
            "○".to_string()
        };
        let details = self
            .snapshot
            .file_review_details
            .get(&change.id)
            .map(|details| {
                let hash = if source {
                    &details.source_hash
                } else {
                    &details.target_hash
                };
                format!(
                    " Whole-file replacement. {}",
                    hash.as_ref()
                        .map(|hash| format!("SHA256 {hash}"))
                        .unwrap_or_else(|| "File not present or unavailable.".into())
                )
            })
            .unwrap_or_default();
        let hover_owner = if source {
            "source-header"
        } else {
            "target-header"
        };
        let id = change.id.clone();
        div().id(SharedString::from(format!("pane-header-{side:?}-{id}"))).flex_1().min_w_0()
        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
            this.hover_review_side(hover_owner, (*hovered).then_some(side), cx);
        }))
        .child(Button::new(SharedString::from(format!(
            "choose-{}-{id}",
            if source { "source" } else { "target" }
        )))
        .accessibility_label(format!(
            "{action}: {name}{}",
            if selected { ", selected" } else { "" }
        ))
        .tooltip(format!(
            "{name} · {address}. {action}.{details} Click the content pane to choose, or use Alt+Left / Alt+Right. Tab then Enter or Space also works."
        ))
        .custom(
            ButtonCustomVariant::new(cx)
                .color(cx.theme().background)
                .foreground(cx.theme().foreground)
                .hover(cx.theme().muted)
                .active(cx.theme().muted),
        )
        .w_full()
        .min_w_0()
        .h_auto()
        .rounded_none()
        .border_0()
        .border_b_2()
        .border_color(if selected || hovered {
            cx.theme().primary
        } else {
            cx.theme().border
        })
        .when(selected, |el| el.bg(cx.theme().primary.opacity(0.06)))
        .px_4()
        .py_3()
        .disabled(disabled)
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(role),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(name),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(if selected || hovered {
                            cx.theme().primary
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(cue),
                ),
        )
        .selected(selected)
        .toggled(selected)

        .on_click(cx.listener(move |this, _, _, cx| this.choose_review_side(&id, side, cx))))
    }

    fn review_opaque_side(
        &self,
        change: &Change,
        side: Choice,
        content: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let disabled = self.busy.is_some() || (side == Choice::Source && !change.source_available);
        div()
            .id(SharedString::from(format!(
                "opaque-side-{side:?}-{}",
                change.id
            )))
            .flex_1()
            .min_w_0()
            .p_4()
            .text_sm()
            .when(!disabled, |el| {
                el.cursor_pointer()
                    .hover(|el| el.bg(cx.theme().muted))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        this.hover_review_side(
                            if side == Choice::Source {
                                "source-opaque"
                            } else {
                                "target-opaque"
                            },
                            (*hovered).then_some(side),
                            cx,
                        );
                    }))
            })
            .child(content)
    }

    fn render_review(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let unresolved = self.unresolved();
        let needs_details = self.snapshot.needs_command_details(&self.choices);
        let changes = &self.snapshot.changes;
        let visible: Vec<_> = changes
            .iter()
            .filter(|change| self.review_item_visible(change))
            .collect();
        let active = self.active_review_change();
        let copies = changes
            .iter()
            .filter(|change| {
                self.review_choice(change) == Some(Choice::Source) && change.source_available
            })
            .count();
        let unavailable = changes
            .iter()
            .filter(|change| !change.source_available)
            .count();
        let kept = changes.len() - copies - unresolved;
        div().flex().flex_col().size_full().min_w_0()
            .when(!self.snapshot.blocked_targets.is_empty(), |el| el.child(
                div().id("blocked-review-notice").test_support().px_5().py_3().border_b_1().border_color(cx.theme().border)
                    .text_color(cx.theme().danger)
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Review incomplete · some targets could not be checked"))
                    .children(self.snapshot.blocked_targets.iter().map(|(device, reason)| div().text_sm().child(format!("{device}: {reason}"))))))
            .when(changes.iter().any(|change| change.path.starts_with("mcp.json")), |el| el.child(
                div().px_5().py_3().flex().items_center().gap_3().border_b_1().border_color(cx.theme().border)
                    .child(Button::new("reveal-command-details").small().label(if self.snapshot.command_details_reviewed { "Command details shown" } else { "Reveal command details" })
                        .disabled(self.busy.is_some() || self.snapshot.command_details_reviewed)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(plan_id) = this.snapshot.plan_id.clone() { this.send(Command::RevealDetails { plan_id }, "Reading command details for this review", cx); }
                        })))
                    .child(div().flex_1().min_w_0().text_xs().text_color(cx.theme().muted_foreground)
                        .child("May contain secrets. Required before copying MCP commands."))))
            .child(div().flex_1().min_h_0().child(
                h_resizable("review-panes")
                    .child(resizable_panel().size(window.rem_size() * 14.).size_range(window.rem_size() * 11. ..window.rem_size() * 23.)
                        .child(div().flex().flex_col().size_full().min_h_0().bg(cx.theme().background)
                            .child(div().px_3().py_3().flex().items_center().gap_2()
                                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("Changes {}", changes.len())))
                                .child(div().flex_1())
                                .child(Checkbox::new("review-pending").small().label("Pending")
                                    .checked(self.review_pending_only)
                                    .on_click(cx.listener(|this, checked, _, cx| { this.review_pending_only = *checked; cx.notify(); }))))
                            .child(div().id("review-scroll").flex_1().min_h_0().overflow_y_scroll()
                                .child(div().px_2().pb_2().flex().flex_col().gap_1()
                                    .children(visible.iter().enumerate().map(|(row, change)| {
                                        let id = change.id.clone();
                                        let selected = active.is_some_and(|active| active.id == change.id);
                                        let status = if self.review_needs_decision(change) { if change.source_available { "Decision needed" } else { "Source unavailable · keep target" } } else { match self.review_choice(change) {
                                            Some(Choice::Source) => "Copy to target", Some(Choice::Target) => "Keep target", _ => "Skipped",
                                        }};
                                        let short_path = change.path.split(" · ").next().unwrap_or(&change.path);
                                        let title = short_path.rsplit('/').next().unwrap_or(short_path);
                                        let group_start = self.snapshot.target_ids.len() > 1 && (row == 0 || visible[row - 1].device != change.device);
                                        div()
                                            .when(group_start, |el| el.child(div().px_2().pt_2().pb_1().text_xs()
                                                .text_color(cx.theme().muted_foreground).child(change.device.clone())))
                                            .child(Button::new(SharedString::from(format!("review-item-{id}")))
                                                .accessibility_label(format!("{} on {}: {}", change.path, change.device, status))
                                                .tooltip(format!("{} · {} · {}", change.path, change.device, status))
                                                .custom(ButtonCustomVariant::new(cx)
                                                    .color(cx.theme().background)
                                                    .foreground(cx.theme().foreground)
                                                    .hover(cx.theme().muted)
                                                    .active(cx.theme().muted))
                                                .selected(selected).w_full().h_auto().px_2().py_2()
                                                .child(div().w_full().min_w_0().flex().items_center().gap_2()
                                                    .child(div().text_xs().text_color(cx.theme().muted_foreground)
                                                        .child(if !change.source_available { "!" } else if self.review_needs_decision(change) { "○" } else { "✓" }))
                                                    .child(div().flex_1().min_w_0().truncate().text_sm()
                                                        .font_weight(if selected { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                                                        .text_color(if selected { cx.theme().foreground } else { cx.theme().muted_foreground })
                                                        .child(title.to_owned())))
                                                .on_click(cx.listener(move |this, _, _, cx| { this.review_change_id = Some(id.clone()); this.review_hovered_side = None; this.review_hover_owner = None; cx.notify(); })))
                                    }))
                                    .when(visible.is_empty(), |el| el.child(div().p_3().text_color(cx.theme().muted_foreground).child(if changes.is_empty() { "No changes to review" } else { "No pending decisions" })))) )
))
                    .child(resizable_panel().size_range(window.rem_size() * 35. ..window.rem_size() * 120.)
                        .child(if let Some(change) = active { self.render_review_detail(change, cx).into_any_element() } else {
                            div().p_5().child(if changes.is_empty() {
                                if self.snapshot.blocked_targets.is_empty() { "Already in sync. The selected files were read successfully and no differences were found." } else { "One or more targets could not be checked. This does not mean they are in sync." }
                            } else { "All decisions are made. Clear Pending to revisit an item." }).into_any_element()
                        }))))
            .child(div().px_5().py_3().border_t_1().border_color(cx.theme().border).flex().items_center().gap_3()
                .child(div().flex_1().min_w_0().text_sm().child(
                    if unresolved > 0 { format!("{unresolved} {} left · {copies} to copy", if unresolved == 1 { "decision" } else { "decisions" }) }
                    else if needs_details { "Review MCP command details to continue".into() }
                    else { format!("{copies} to copy · {kept} kept") }))
                .when(unavailable > 0, |el| el.child(div().text_xs().text_color(cx.theme().danger).child(format!("{unavailable} source unavailable"))))
                .child(Button::new("back-to-selection").label("Cancel review").disabled(self.busy.is_some())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.invalidate_review();
                        this.send(Command::CancelReview, "Returning to your saved selection", cx);
                    })))
                .child(Button::new("apply-reviewed").primary().label("Confirm sync…")
                    .disabled(self.busy.is_some() || unresolved > 0 || needs_details || changes.is_empty())
                    .on_click(cx.listener(|this, _, window, cx| this.confirm_apply(window, cx)))))
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
            .on_action(cx.listener(|this, _: &SelectAllSourceChanges, _, cx| this.select_all_source_changes(cx)))
            .on_action(cx.listener(|this, _: &ChooseReviewSource, window, cx| this.review_keyboard(Some(Choice::Source), false, window, cx)))
            .on_action(cx.listener(|this, _: &ChooseReviewTarget, window, cx| this.review_keyboard(Some(Choice::Target), false, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousReviewHunk, window, cx| this.review_keyboard(None, false, window, cx)))
            .on_action(cx.listener(|this, _: &NextReviewHunk, window, cx| this.review_keyboard(None, true, window, cx)))
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
                        self.render_review(window, cx).into_any_element()
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
            .when(self.snapshot.plan_id.is_none() || self.busy.is_some(), |el| el.child(
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
                    .when(self.busy.is_some(), |el| el.child(
                        Button::new("cancel-operation").small().label("Cancel operation")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.busy.is_some() {
                                    this.send(Command::CancelOperation, "Cancelling… Inspect targets after an interrupted apply", cx);
                                }
                            }))
                    ))
                    .child("SSH passwords are not saved"),
            ))
    }
}

#[cfg(test)]
#[path = "workspace_test.rs"]
mod tests;

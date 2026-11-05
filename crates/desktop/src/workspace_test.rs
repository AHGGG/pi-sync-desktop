//! Real production-view integration tests. Fixtures exist only in this module.
use super::Workspace;
use crate::ui_model::{Change, Choice, Command, ConfigItem, Device, Snapshot};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext, Bounds, Entity, TestAppContext, WindowBounds, WindowOptions, px,
    size,
};
use std::sync::mpsc::{self, Receiver};

fn window(
    cx: &mut TestAppContext,
    review: bool,
) -> (AnyWindowHandle, Entity<Workspace>, Receiver<Command>) {
    cx.update(gpui_kit::init);
    let (commands, received) = mpsc::channel();
    let (_events, events) = mpsc::channel();
    let (handle, view) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(1240.), px(820.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                cx.new(|cx| {
                    let mut view = Workspace::new(commands, events, window, cx);
                    view.busy = None;
                    view.initial_snapshot_received = true;
                    view.snapshot = Snapshot {
                        devices: vec![
                            Device {
                                id: "source".into(),
                                name: "Source".into(),
                                address: "localhost".into(),
                                status: "Ready".into(),
                                is_local: true,
                                platform: "linux".into(),
                            },
                            Device {
                                id: "target".into(),
                                name: "Target".into(),
                                address: "test-host".into(),
                                status: "Ready".into(),
                                is_local: false,
                                platform: "ssh".into(),
                            },
                        ],
                        source_id: Some("source".into()),
                        items: vec![ConfigItem {
                            id: "settings".into(),
                            name: "Settings".into(),
                            category: "Configuration".into(),
                            detail: "Pi preferences".into(),
                        }],
                        ..Default::default()
                    };
                    if review {
                        view.snapshot.plan_id = Some("review-1".into());
                        view.snapshot.changes = vec![Change {
                            id: "change".into(),
                            device: "Target".into(),
                            path: "settings.json/theme".into(),
                            before: "old".into(),
                            after: "new".into(),
                            conflict: true,
                            source_available: true,
                        }];
                    }
                    view
                })
            },
        )
        .expect("open production workspace")
    });
    assert!(matches!(received.try_recv(), Ok(Command::Refresh)));
    (handle, view, received)
}

#[gpui_kit::test]
fn staging_uses_real_controls_and_keyboard_review(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, false);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("prepare-review").disabled(), Some(true));
        window.click("select-settings", cx);
        assert_eq!(window.find("select-settings").checked(), Some(true));
        window.click("target-target", cx);
        assert_eq!(window.find("target-target").checked(), Some(true));
        assert_ne!(window.find("prepare-review").disabled(), Some(true));
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-enter"
            } else {
                "ctrl-enter"
            },
            cx,
        );
    })
    .unwrap();
    match next_action(&received).expect("prepare command") {
        Command::Prepare {
            source,
            targets,
            items,
        } => {
            assert_eq!(source, "source");
            assert_eq!(targets, ["target"]);
            assert_eq!(items, ["settings"]);
        }
        _ => panic!("only Prepare is permitted before review"),
    }
    cx.update(|cx| assert!(view.read(cx).busy.is_some()));
    assert!(received.try_recv().is_err());
}

#[gpui_kit::test]
fn conflict_blocks_apply_until_keyboard_resolution_and_confirmation(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("apply-reviewed").disabled(), Some(true));
        window.within("resolve-change").click(0usize, cx);
        assert_eq!(view.read(cx).choices.get("change"), Some(&Choice::Source));
        // The native radio does not steal focus on pointer activation. Tab
        // through the actual focus chain to the second choice, then activate.
        for _ in 0..24 {
            window.press("tab", cx);
            if window.within("resolve-change").find(1usize).focused() == Some(true) {
                break;
            }
        }
        assert_eq!(
            window.within("resolve-change").find(1usize).focused(),
            Some(true)
        );
        window.press("space", cx);
        assert_eq!(view.read(cx).choices.get("change"), Some(&Choice::Target));
        assert_ne!(window.find("apply-reviewed").disabled(), Some(true));
        window.click("apply-reviewed", cx);
        assert!(
            received.try_recv().is_err(),
            "opening confirmation must not apply"
        );
        window.within("dialog").click("ok", cx);
    })
    .unwrap();
    match received.try_recv().expect("explicit Apply command") {
        Command::Apply { plan_id, choices } => {
            assert_eq!(plan_id, "review-1");
            assert_eq!(choices["change"], Choice::Target);
        }
        _ => panic!("confirmation must submit the reviewed plan"),
    }
}

#[gpui_kit::test]
fn authentication_masks_input_and_sends_only_on_confirmation(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, false);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.authenticate(
                "request".into(),
                "Password for test user".into(),
                window,
                cx,
            )
        });
        window.render_frame(cx);
        window.within("dialog").click("ssh-secret", cx);
        window.input("not-a-real-password", cx);
        assert_ne!(
            window.within("dialog").find("ssh-secret").value(),
            Some("not-a-real-password")
        );
        assert!(received.try_recv().is_err());
        window.within("dialog").click("ok", cx);
    })
    .unwrap();
    match received.try_recv().expect("one-shot authentication") {
        Command::Authenticate { request_id, secret } => {
            assert_eq!(request_id, "request");
            assert_eq!(secret.as_str(), "not-a-real-password");
        }
        _ => panic!("authentication should be a one-shot secret, not a profile"),
    }
}

#[gpui_kit::test]
fn drag_stages_item_without_applying(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, false);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.drag_to("config-settings", "queue-drop-zone", cx);
        assert!(view.read(cx).selected.contains("settings"));
        assert_eq!(window.find("select-settings").checked(), Some(true));
        assert_eq!(window.find("prepare-review").disabled(), Some(true));
    })
    .unwrap();
    assert!(matches!(
        received.try_recv(),
        Ok(Command::SaveSelection { .. })
    ));
    assert!(
        received.try_recv().is_err(),
        "dropping must only save, not apply or prepare"
    );
}

#[gpui_kit::test]
fn unavailable_model_source_is_disabled_but_skip_can_be_confirmed(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.snapshot.changes[0].path = "models.json · entire private file".into();
            view.snapshot.changes[0].source_available = false;
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(
            window.within("resolve-change").find(0usize).disabled(),
            Some(true)
        );
        window.within("resolve-change").click(0usize, cx);
        assert!(!view.read(cx).choices.contains_key("change"));
        assert_eq!(window.find("apply-reviewed").disabled(), Some(true));
        window.within("resolve-change").click(2usize, cx);
        assert_eq!(view.read(cx).choices.get("change"), Some(&Choice::Skip));
        assert_ne!(window.find("apply-reviewed").disabled(), Some(true));
        window.click("apply-reviewed", cx);
        assert!(received.try_recv().is_err());
        window.within("dialog").click("ok", cx);
    })
    .unwrap();
    match received.try_recv().expect("explicit Apply command") {
        Command::Apply { choices, .. } => assert_eq!(choices["change"], Choice::Skip),
        _ => panic!("only explicit confirmation applies the skipped selection"),
    }
}

// Persisting checkbox choices is deliberately separate from preparing/applying.
fn next_action(received: &Receiver<Command>) -> Option<Command> {
    while let Ok(command) = received.try_recv() {
        if !matches!(command, Command::SaveSelection { .. }) {
            return Some(command);
        }
    }
    None
}

#[gpui_kit::test]
fn first_entry_saves_connections_before_explicit_next(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, false);
    cx.update_window(handle, |_, window, cx| {
        let mut initial = view.read(cx).snapshot.clone();
        let saved_device = initial.devices.pop().unwrap();
        initial.selection_loaded = true;
        view.update(cx, |view, cx| {
            view.initial_snapshot_received = false;
            view.receive(
                crate::ui_model::Event::Snapshot(initial.clone()),
                window,
                cx,
            );
        });
        window.render_frame(cx);
        assert_eq!(window.find("setup-next").disabled(), Some(true));
        assert!(window.find_all("prepare-review").is_empty());
        window.click("setup-device-name", cx);
        window.input("Example workstation", cx);
        window.click("setup-ssh-command", cx);
        window.input("ssh user@example.invalid", cx);
        window.click("save-connection", cx);
        match received.try_recv().unwrap() {
            Command::AddDevice {
                name,
                ssh_line,
                root,
            } => {
                assert_eq!(name, "Example workstation");
                assert_eq!(ssh_line, "ssh user@example.invalid");
                assert!(root.is_empty());
            }
            _ => panic!("saving a connection must not start syncing"),
        }
        initial.devices.push(saved_device);
        view.update(cx, |view, cx| {
            view.receive(crate::ui_model::Event::Snapshot(initial), window, cx)
        });
        window.render_frame(cx);
        assert!(
            view.read(cx).onboarding,
            "a successful save must still wait for Next"
        );
        assert_ne!(window.find("setup-next").disabled(), Some(true));
        window.click("setup-next", cx);
        assert!(!view.read(cx).onboarding);
        assert!(view.read(cx).targets.contains("target"));
        assert_eq!(window.find("prepare-review").disabled(), Some(true));
        assert!(matches!(
            received.try_recv(),
            Ok(Command::SaveSelection { .. })
        ));
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn startup_restores_selection_once_and_save_ack_does_not_clear_busy(cx: &mut TestAppContext) {
    let (handle, view, _) = window(cx, false);
    cx.update_window(handle, |_, window, cx| {
        let mut saved = view.read(cx).snapshot.clone();
        saved.selection_loaded = true;
        saved.selected_item_ids = vec!["settings".into()];
        saved.target_ids = vec!["target".into()];
        view.update(cx, |view, cx| {
            view.initial_snapshot_received = false;
            view.receive(crate::ui_model::Event::Snapshot(saved.clone()), window, cx);
            assert!(view.selected.contains("settings"));
            assert!(view.targets.contains("target"));
            view.selected.clear();
            view.receive(crate::ui_model::Event::Snapshot(saved), window, cx);
            assert!(
                view.selected.is_empty(),
                "refresh may not overwrite unsent choices"
            );
            view.busy = Some("Preparing".into());
            view.receive(
                crate::ui_model::Event::SelectionSaved {
                    has_last_selection: true,
                },
                window,
                cx,
            );
            assert_eq!(view.busy.as_deref(), Some("Preparing"));
        });
    })
    .unwrap();
}

#[gpui_kit::test]
fn custom_path_is_remembered_only_on_dialog_confirmation(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, false);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("add-path", cx);
        window.within("dialog").click("custom-path", cx);
        window.input("extensions", cx);
        window.within("dialog").click("cancel", cx);
        assert!(view.read(cx).selected.is_empty());
        assert!(received.try_recv().is_err());
        window.click("add-path", cx);
        window.within("dialog").click("custom-path", cx);
        window.input("extensions", cx);
        window.within("dialog").click("ok", cx);
        assert!(view.read(cx).selected.contains("path:extensions"));
        match received.try_recv().unwrap() {
            Command::SaveSelection { items, .. } => assert_eq!(items, ["path:extensions"]),
            _ => panic!("only save selection"),
        }
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn review_replaces_selection_and_cancel_discards_plan_not_scope(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, _| {
            view.selected.insert("settings".into());
            view.targets.insert("target".into());
        });
        window.render_frame(cx);
        assert!(window.find_all("prepare-review").is_empty());
        assert!(window.find_all("target-target").is_empty());
        window.click("back-to-selection", cx);
        assert!(matches!(received.try_recv(), Ok(Command::CancelReview)));
        assert!(view.read(cx).snapshot.plan_id.is_none());
        assert!(view.read(cx).selected.contains("settings"));
        assert!(view.read(cx).targets.contains("target"));
        assert!(view.read(cx).choices.is_empty());
        window.render_frame(cx);
        assert_eq!(window.find("prepare-review").disabled(), Some(true));
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn cancelling_confirmation_writes_nothing_and_apply_is_single_shot(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("resolve-change").click(0usize, cx);
        window.click("apply-reviewed", cx);
        window.within("dialog").click("cancel", cx);
        assert!(received.try_recv().is_err());
        assert!(view.read(cx).snapshot.plan_id.is_some());
        window.click("apply-reviewed", cx);
        window.within("dialog").click("ok", cx);
        assert!(matches!(received.try_recv(), Ok(Command::Apply { .. })));
        assert!(view.read(cx).busy.is_some());
        assert_eq!(window.find("apply-reviewed").disabled(), Some(true));
        window.click("apply-reviewed", cx);
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn bulk_file_choice_still_requires_separate_confirmation(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.snapshot.changes[0].id = "target:files:extensions/example.ts".into();
            view.snapshot.changes[0].path = "extensions/example.ts · entire file".into();
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(window.find("apply-reviewed").disabled(), Some(true));
        window.click("use-source-files", cx);
        assert_eq!(
            view.read(cx)
                .choices
                .get("target:files:extensions/example.ts"),
            Some(&Choice::Source)
        );
        assert_ne!(window.find("apply-reviewed").disabled(), Some(true));
        assert!(
            received.try_recv().is_err(),
            "choosing incoming files cannot apply"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn failed_connection_is_a_blocked_review_not_zero_differences(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.snapshot.changes.clear();
            view.snapshot.blocked_targets = vec![("Target".into(), "Connection failed".into())];
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(window.find_all("blocked-review-notice").len(), 1);
        assert_eq!(window.find("apply-reviewed").disabled(), Some(true));
        view.update(cx, |view, cx| {
            view.snapshot.blocked_targets.clear();
            cx.notify();
        });
        window.render_frame(cx);
        assert!(window.find_all("blocked-review-notice").is_empty());
        assert_eq!(window.find("apply-reviewed").disabled(), Some(true));
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn busy_cancel_sends_only_cancel_and_keeps_apply_disabled(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.busy = Some("Applying the changes you reviewed…".into());
            cx.notify();
        });
        window.render_frame(cx);
        window.click("cancel-operation", cx);
        assert!(matches!(received.try_recv(), Ok(Command::CancelOperation)));
        assert!(received.try_recv().is_err());
        assert!(
            view.read(cx)
                .busy
                .as_deref()
                .unwrap()
                .contains("Inspect targets")
        );
        window.render_frame(cx);
        assert_eq!(window.find("apply-reviewed").disabled(), Some(true));
        window.click("apply-reviewed", cx);
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn finished_authentication_closes_prompt_and_erases_unsent_secret(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, false);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.receive(
                crate::ui_model::Event::Authentication {
                    request_id: "auth-cancelled".into(),
                    prompt: "Password".into(),
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        window.within("dialog").click("ssh-secret", cx);
        window.input("never-send-this", cx);
        let secret = view
            .read(cx)
            .authentication
            .as_ref()
            .unwrap()
            .1
            .clone()
            .unwrap();
        view.update(cx, |view, cx| {
            view.receive(
                crate::ui_model::Event::AuthenticationFinished {
                    request_id: "unrelated-old-prompt".into(),
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        assert!(!window.find_all("ssh-secret").is_empty());
        view.update(cx, |view, cx| {
            view.receive(
                crate::ui_model::Event::AuthenticationFinished {
                    request_id: "auth-cancelled".into(),
                },
                window,
                cx,
            );
            view.receive(
                crate::ui_model::Event::Error("Operation cancelled".into()),
                window,
                cx,
            );
        });
        window.render_frame(cx);
        assert!(window.find_all("ssh-secret").is_empty());
        assert!(view.read(cx).authentication.is_none());
        assert!(secret.read(cx).value().is_empty());
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn worker_error_closes_pending_host_trust_without_accepting(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, false);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.receive(
                crate::ui_model::Event::TrustHost {
                    request_id: "trust-cancelled".into(),
                    host: "example.invalid".into(),
                    fingerprint: "SHA256:test".into(),
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        assert!(!window.find_all("dialog").is_empty());
        view.update(cx, |view, cx| {
            view.receive(
                crate::ui_model::Event::Error("Operation cancelled".into()),
                window,
                cx,
            );
        });
        window.render_frame(cx);
        assert!(window.find_all("dialog").is_empty());
        assert!(view.read(cx).authentication.is_none());
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

//! Real production-view integration tests. Fixtures exist only in this module.
use super::{LocalPathKind, Workspace, combined_local_picker};
use crate::ui_model::{Change, Choice, Command, ConfigItem, Device, Snapshot};
use gpui_kit::test::{TestQueryExt, TestWindowExt};
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
        window.click("choose-source-change", cx);
        assert_eq!(view.read(cx).choices.get("change"), Some(&Choice::Source));
        // The native pane header does not steal focus on pointer activation. Tab
        // through the actual focus chain to the second choice, then activate.
        for _ in 0..24 {
            window.press("tab", cx);
            if window.find("choose-target-change").focused() == Some(true) {
                break;
            }
        }
        assert_eq!(window.find("choose-target-change").focused(), Some(true));
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
fn unavailable_model_source_is_disabled_but_target_can_be_confirmed(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.snapshot.changes[0].path = "models.json · entire private file".into();
            view.snapshot.changes[0].source_available = false;
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(window.find("choose-source-change").disabled(), Some(true));
        window.click("choose-source-change", cx);
        assert!(!view.read(cx).choices.contains_key("change"));
        assert_eq!(window.find("apply-reviewed").disabled(), Some(true));
        window.click("choose-target-change", cx);
        assert_eq!(view.read(cx).choices.get("change"), Some(&Choice::Target));
        assert_ne!(window.find("apply-reviewed").disabled(), Some(true));
        window.click("apply-reviewed", cx);
        assert!(received.try_recv().is_err());
        window.within("dialog").click("ok", cx);
    })
    .unwrap();
    match received.try_recv().expect("explicit Apply command") {
        Command::Apply { choices, .. } => assert_eq!(choices["change"], Choice::Target),
        _ => panic!("only explicit confirmation applies the keep-target selection"),
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
fn remote_path_is_remembered_only_on_dialog_confirmation(cx: &mut TestAppContext) {
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

fn make_local_source(view: &Entity<Workspace>, cx: &mut gpui_kit::App) {
    view.update(cx, |view, cx| {
        view.snapshot.source_id = Some("local".into());
        view.snapshot.devices[0].id = "local".into();
        view.snapshot.devices[0].address = "/fixture/pi-agent".into();
        cx.notify();
    });
}

#[gpui_kit::test]
fn resource_defaults_follow_source_changes_but_never_override_same_source_opt_out(
    cx: &mut TestAppContext,
) {
    let (handle, view, received) = window(cx, false);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            let mut local = view.snapshot.clone();
            local.source_id = Some("local".into());
            local.devices[0].id = "local".into();
            local.items.push(ConfigItem { id: "path:extensions".into(), name: "Extensions".into(), category: "Pi resources".into(), detail: "extensions/".into() });
            local.selected_item_ids = vec!["path:extensions".into()];
            view.receive(crate::ui_model::Event::Snapshot(local), window, cx);
        });
        window.render_frame(cx);
        assert_eq!(window.find("select-path:extensions").checked(), Some(true));
        window.click("select-path:extensions", cx);
        assert!(!view.read(cx).selected.contains("path:extensions"));
        assert!(matches!(received.try_recv(), Ok(Command::SaveSelection { items, .. }) if items.is_empty()));
        view.update(cx, |view, cx| {
            let stale = view.snapshot.clone();
            view.receive(crate::ui_model::Event::Snapshot(stale), window, cx);
        });
        window.render_frame(cx);
        assert_eq!(window.find("select-path:extensions").checked(), Some(false));
        window.click("change-source", cx);
        window.click("source-local", cx);
        assert_eq!(window.find("select-path:extensions").checked(), Some(false));
        assert!(received.try_recv().is_err(), "reselecting the same source preserves opt-out without saving a different scope");
    }).unwrap();
}

#[test]
fn combined_picker_requires_macos_and_native_support() {
    assert!(combined_local_picker("macos", true));
    assert!(!combined_local_picker("macos", false));
    assert!(!combined_local_picker("windows", true));
    assert!(!combined_local_picker("windows", false));
    assert!(!combined_local_picker("linux", true));
    assert!(!combined_local_picker("linux", false));
}

#[gpui_kit::test]
fn picker_button_layout_uses_the_native_platform_capability(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, false);
    cx.update_window(handle, |_, window, cx| {
        make_local_source(&view, cx);
        window.render_frame(cx);
        window.click("add-path", cx);
        let mixed =
            combined_local_picker(std::env::consts::OS, cx.can_select_mixed_files_and_dirs());
        assert_eq!(
            window.within("dialog").find_all("choose-local-items").len(),
            usize::from(mixed)
        );
        assert_eq!(
            window.within("dialog").find_all("choose-local-files").len(),
            usize::from(!mixed)
        );
        assert_eq!(
            window
                .within("dialog")
                .find_all("choose-local-folders")
                .len(),
            usize::from(!mixed)
        );
        window.within("dialog").click("cancel", cx);
    })
    .unwrap();
    assert!(received.try_recv().is_err());
    assert!(!cx.did_prompt_for_paths());
}

#[gpui_kit::test]
fn combined_picker_uses_one_button_and_accepts_files_and_folders_together(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, false);
    for response in [
        None,
        Some(vec![
            "/fixture/pi-agent/file.txt".into(),
            "/fixture/pi-agent/skills".into(),
        ]),
    ] {
        cx.update_window(handle, |_, window, cx| {
            make_local_source(&view, cx);
            view.update(cx, |view, cx| {
                view.show_local_path_picker_with_mixed(true, window, cx)
            });
            window.render_frame(cx);
            assert!(
                window
                    .within("dialog")
                    .find_all("choose-local-files")
                    .is_empty()
            );
            assert!(
                window
                    .within("dialog")
                    .find_all("choose-local-folders")
                    .is_empty()
            );
            window.within("dialog").click("choose-local-items", cx);
            assert!(view.read(cx).path_picker_open);
            assert!(received.try_recv().is_err());
        })
        .unwrap();
        let expected = response.clone();
        cx.simulate_path_prompt_response(|options| {
            assert!(options.files && options.directories && options.multiple);
            response
        });
        cx.run_until_parked();
        if let Some(expected) = expected {
            match received.try_recv().unwrap() {
                Command::AddLocalPaths { paths, .. } => assert_eq!(paths, expected),
                _ => panic!("combined picker must request validation only"),
            }
        } else {
            assert!(received.try_recv().is_err(), "cancel must not save or sync");
        }
        cx.update(|cx| {
            assert!(!view.read(cx).path_picker_open);
            assert!(
                view.read(cx).selected.is_empty(),
                "mixed selections need worker validation too"
            );
        });
    }
}

#[gpui_kit::test]
fn native_file_picker_stages_multiple_paths_only_after_worker_validation(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, false);
    cx.update_window(handle, |_, window, cx| {
        make_local_source(&view, cx);
        view.update(cx, |view, _| {
            view.selected.insert("settings".into());
            view.targets.insert("target".into());
        });
        window.render_frame(cx);
        view.update(cx, |view, cx| {
            view.show_local_path_picker_with_mixed(false, window, cx)
        });
        window.render_frame(cx);
        assert!(window.within("dialog").find_all("custom-path").is_empty());
        window.within("dialog").click("copy-pi-root", cx);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("/fixture/pi-agent")
        );
        window.within("dialog").click("choose-local-files", cx);
        assert!(window.find_all("dialog").is_empty());
        assert!(view.read(cx).path_picker_open);
        assert_eq!(window.find("change-source").disabled(), Some(true));
        assert!(received.try_recv().is_err());
    })
    .unwrap();
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|options| {
        assert!(options.files);
        assert!(!options.directories);
        assert!(options.multiple);
        Some(vec![
            "/fixture/pi-agent/a.txt".into(),
            "/fixture/pi-agent/b.txt".into(),
        ])
    });
    cx.run_until_parked();
    match received.try_recv().expect("worker validation request") {
        Command::AddLocalPaths {
            paths,
            targets,
            items,
        } => {
            assert_eq!(
                paths,
                [
                    std::path::PathBuf::from("/fixture/pi-agent/a.txt"),
                    "/fixture/pi-agent/b.txt".into()
                ]
            );
            assert_eq!(targets, ["target"]);
            assert_eq!(items, ["settings"]);
        }
        _ => panic!("picker must only request worker validation, never review or apply"),
    }
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(
            view.read(cx).selected.len(),
            1,
            "unvalidated files are not staged"
        );
        assert!(view.read(cx).busy.is_some());
        view.update(cx, |view, cx| {
            let mut saved = view.snapshot.clone();
            saved.selected_item_ids =
                vec!["settings".into(), "path:a.txt".into(), "path:b.txt".into()];
            saved.target_ids = vec!["target".into()];
            saved.has_last_selection = true;
            view.receive(crate::ui_model::Event::LocalPathsAdded(saved), window, cx);
        });
        assert!(view.read(cx).selected.contains("path:a.txt"));
        assert!(view.read(cx).selected.contains("path:b.txt"));
        assert!(view.read(cx).targets.contains("target"));
        assert!(view.read(cx).busy.is_none());
        assert!(view.read(cx).snapshot.has_last_selection);
        assert!(view.read(cx).snapshot.plan_id.is_none());
    })
    .unwrap();
    assert!(received.try_recv().is_err());
}

#[gpui_kit::test]
fn picker_cancel_empty_result_and_repeated_open_leave_selection_unchanged(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, false);
    cx.update_window(handle, |_, window, cx| {
        make_local_source(&view, cx);
        view.update(cx, |view, _| {
            view.selected.insert("settings".into());
        });
        window.render_frame(cx);
        view.update(cx, |view, cx| {
            view.show_local_path_picker_with_mixed(false, window, cx)
        });
        window.render_frame(cx);
        window.within("dialog").click("cancel", cx);
        assert!(window.find_all("dialog").is_empty());
    })
    .unwrap();
    assert!(!cx.did_prompt_for_paths());
    for response in [None, Some(vec![])] {
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.show_local_path_picker_with_mixed(false, window, cx)
            });
            window.render_frame(cx);
            window.within("dialog").click("choose-local-folders", cx);
            view.update(cx, |view, cx| {
                view.pick_local_paths(LocalPathKind::Folders, window, cx)
            });
            assert!(view.read(cx).path_picker_open);
        })
        .unwrap();
        cx.simulate_path_prompt_response(|options| {
            assert!(!options.files);
            assert!(options.directories && options.multiple);
            response
        });
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(!view.read(cx).path_picker_open);
            assert!(!view.read(cx).is_busy());
            assert!(view.read(cx).error.is_none());
            assert_eq!(
                view.read(cx).selected.iter().cloned().collect::<Vec<_>>(),
                ["settings"]
            );
            assert_ne!(window.find("add-path").disabled(), Some(true));
        })
        .unwrap();
        assert!(received.try_recv().is_err());
    }
}

#[gpui_kit::test]
fn native_picker_cannot_attach_result_to_a_changed_source(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, false);
    cx.update_window(handle, |_, window, cx| {
        make_local_source(&view, cx);
        window.render_frame(cx);
        view.update(cx, |view, cx| {
            view.show_local_path_picker_with_mixed(false, window, cx)
        });
        window.render_frame(cx);
        window.within("dialog").click("choose-local-files", cx);
        view.update(cx, |view, cx| {
            let mut changed = view.snapshot.clone();
            changed.source_id = Some("target".into());
            view.receive(crate::ui_model::Event::Snapshot(changed), window, cx);
        });
        assert!(
            view.read(cx).is_busy(),
            "snapshot cannot unlock an outstanding OS dialog"
        );
    })
    .unwrap();
    cx.simulate_path_prompt_response(|_| Some(vec!["/fixture/pi-agent/a.txt".into()]));
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(view.read(cx).selected.is_empty());
        assert!(
            view.read(cx)
                .error
                .as_deref()
                .unwrap()
                .contains("source changed")
        );
        assert!(!view.read(cx).path_picker_open);
    });
    assert!(received.try_recv().is_err());
}

#[gpui_kit::test]
fn picker_validation_error_leaves_previous_selection_and_allows_retry(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, false);
    cx.update_window(handle, |_, window, cx| {
        make_local_source(&view, cx);
        view.update(cx, |view, _| {
            view.selected.insert("settings".into());
        });
        window.render_frame(cx);
        view.update(cx, |view, cx| {
            view.show_local_path_picker_with_mixed(false, window, cx)
        });
        window.render_frame(cx);
        window.within("dialog").click("choose-local-folders", cx);
    })
    .unwrap();
    cx.simulate_path_prompt_response(|_| Some(vec!["/outside-root".into()]));
    cx.run_until_parked();
    assert!(matches!(
        received.try_recv(),
        Ok(Command::AddLocalPaths { .. })
    ));
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.receive(
                crate::ui_model::Event::Error("Choose items inside the Pi directory".into()),
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(
            view.read(cx).selected.iter().cloned().collect::<Vec<_>>(),
            ["settings"]
        );
        assert!(!view.read(cx).is_busy());
        view.update(cx, |view, cx| {
            view.show_local_path_picker_with_mixed(false, window, cx)
        });
        window.render_frame(cx);
        assert!(
            !window
                .within("dialog")
                .find_all("choose-local-files")
                .is_empty()
        );
        window.within("dialog").click("cancel", cx);
    })
    .unwrap();
    assert!(received.try_recv().is_err());
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
        window.click("choose-source-change", cx);
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
fn bulk_source_choice_still_requires_separate_confirmation(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.snapshot.changes[0].id = "target:files:extensions/example.ts".into();
            view.snapshot.changes[0].path = "extensions/example.ts · entire file".into();
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(window.find("apply-reviewed").disabled(), Some(true));
        window.click("use-source-all", cx);
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

fn review_snapshot(
    view: &Entity<Workspace>,
    changes: Vec<Change>,
    previews: Vec<(&str, &str, &str)>,
    window: &mut gpui_kit::Window,
    cx: &mut gpui_kit::App,
) {
    let mut snapshot = view.read(cx).snapshot.clone();
    snapshot.changes = changes;
    snapshot.text_previews = previews
        .into_iter()
        .map(|(id, target, source)| (id.to_owned(), (target.to_owned(), source.to_owned())))
        .collect();
    view.update(cx, |view, cx| {
        view.receive(crate::ui_model::Event::Snapshot(snapshot), window, cx)
    });
    window.render_frame(cx);
}

fn review_change(id: &str, path: &str, available: bool) -> Change {
    Change {
        id: id.into(),
        device: "Target".into(),
        path: path.into(),
        before: "Target preview".into(),
        after: "Source preview".into(),
        conflict: true,
        source_available: available,
    }
}

// A tall hunk with stable anchors is cheap to diff within the production 20 ms
// budget. At most four equal rows between edits keep it one hunk at radius 3.
fn tall_review_hunk(side: &str) -> String {
    (0..90)
        .map(|i| {
            if i < 2 || i % 5 == 0 {
                format!("{side} {i}\n")
            } else {
                format!("line {i}\n")
            }
        })
        .collect()
}

fn click_point(
    window: &mut gpui_kit::Window,
    position: gpui_kit::Point<gpui_kit::Pixels>,
    cx: &mut gpui_kit::App,
) {
    use gpui_kit::{MouseButton, MouseDownEvent, MouseUpEvent, PlatformInput};
    window.dispatch_event(
        PlatformInput::MouseDown(MouseDownEvent {
            position,
            button: MouseButton::Left,
            click_count: 1,
            ..Default::default()
        }),
        cx,
    );
    window.render_frame(cx);
    window.dispatch_event(
        PlatformInput::MouseUp(MouseUpEvent {
            position,
            button: MouseButton::Left,
            click_count: 1,
            ..Default::default()
        }),
        cx,
    );
    window.render_frame(cx);
}

fn hover_point(
    window: &mut gpui_kit::Window,
    position: gpui_kit::Point<gpui_kit::Pixels>,
    cx: &mut gpui_kit::App,
) {
    window.dispatch_event(
        gpui_kit::PlatformInput::MouseMove(gpui_kit::MouseMoveEvent {
            position,
            ..Default::default()
        }),
        cx,
    );
    window.render_frame(cx);
}

#[gpui_kit::test]
fn native_diff_sides_stay_aligned_and_clicks_do_not_apply(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    let long_line = format!("source-long-{}", "x".repeat(300));
    cx.update_window(handle, |_, window, cx| {
        let source = format!("source first\nsource second\n{long_line}\n");
        let target = "target first\ntarget second\ntarget short\n";
        review_snapshot(
            &view,
            vec![review_change(
                "target:files:example.txt",
                "example.txt",
                true,
            )],
            vec![("target:files:example.txt", target, &source)],
            window,
            cx,
        );
    })
    .unwrap();
    // Diff measures its viewport after paint; process that native layout update.
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let left = window
            .find("choose-source-target:files:example.txt")
            .bounds();
        let right = window
            .find("choose-target-target:files:example.txt")
            .bounds();
        let source_line = window.find_by_label("source first").bounds();
        let target_line = window.find_by_label("target first").bounds();
        assert!(source_line.left() >= left.left() && source_line.right() <= left.right());
        assert!(
            target_line.left() >= right.left() && target_line.right() <= right.right(),
            "target line {target_line:?}; right header {right:?}; left header {left:?}"
        );
        assert!(window.find_by_label(&long_line).bounds().right() <= left.right());
        hover_point(window, source_line.center(), cx);
        assert_eq!(view.read(cx).review_hovered_side, Some(Choice::Source));
        hover_point(
            window,
            window.find_by_label("source second").bounds().center(),
            cx,
        );
        assert_eq!(view.read(cx).review_hovered_side, Some(Choice::Source));
        hover_point(window, target_line.center(), cx);
        assert_eq!(view.read(cx).review_hovered_side, Some(Choice::Target));
        window.hover("choose-source-target:files:example.txt", cx);
        assert_eq!(view.read(cx).review_hovered_side, Some(Choice::Source));
        assert_eq!(view.read(cx).review_hover_owner, Some("source-header"));
        window.hover("choose-target-target:files:example.txt", cx);
        assert_eq!(view.read(cx).review_hovered_side, Some(Choice::Target));
        assert_eq!(view.read(cx).review_hover_owner, Some("target-header"));
        hover_point(window, gpui_kit::point(px(40.), px(30.)), cx);
        assert_eq!(view.read(cx).review_hovered_side, None);
        click_point(window, source_line.center(), cx);
        assert_eq!(
            view.read(cx).choices["target:files:example.txt"],
            Choice::Source
        );
        click_point(window, target_line.center(), cx);
        assert_eq!(
            view.read(cx).choices["target:files:example.txt"],
            Choice::Target
        );
        view.update(cx, |view, _| {
            view.choices.clear();
        });
        window.render_frame(cx);
        let point = window.find_by_label("source first").bounds().center();
        window.drag(point, point + gpui_kit::point(px(25.), px(0.)), cx);
        assert!(
            view.read(cx).choices.is_empty(),
            "dragging text is not a version choice"
        );
        assert!(
            received.try_recv().is_err(),
            "preview interaction never writes"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn bulk_selection_excludes_unavailable_and_preserves_mcp_gate(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    cx.update_window(handle, |_, window, cx| {
        review_snapshot(
            &view,
            vec![
                review_change("target:files:example.txt", "example.txt", true),
                review_change("target:settings:/theme", "settings.json/theme", true),
                review_change(
                    "target:models:/",
                    "models.json · entire private file",
                    false,
                ),
                review_change("target:mcp:/server", "mcp.json/server", true),
            ],
            vec![],
            window,
            cx,
        );
        window.click("use-source-all", cx);
        assert_eq!(view.read(cx).choices.len(), 3);
        assert!(!view.read(cx).choices.contains_key("target:models:/"));
        assert_eq!(window.find("apply-reviewed").disabled(), Some(true));
        window.click("review-item-target:models:/", cx);
        assert_eq!(
            window.find("choose-source-target:models:/").disabled(),
            Some(true)
        );
        window.click("choose-target-target:models:/", cx);
        assert_eq!(view.read(cx).choices["target:models:/"], Choice::Target);
        assert_eq!(view.read(cx).unresolved(), 0);
        assert_eq!(
            window.find("apply-reviewed").disabled(),
            Some(true),
            "MCP details still require review"
        );
        assert!(received.try_recv().is_err());
        window.click("reveal-command-details", cx);
        assert!(matches!(
            received.try_recv(),
            Ok(Command::RevealDetails { .. })
        ));
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn pending_navigation_and_cancel_clear_ephemeral_review(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    cx.update_window(handle, |_, window, cx| {
        let mut automatic = review_change("target:settings:/auto", "settings.json/auto", true);
        automatic.conflict = false;
        review_snapshot(
            &view,
            vec![
                review_change("target:files:first.txt", "first.txt", true),
                review_change("target:files:second.txt", "second.txt", true),
                automatic,
            ],
            vec![("target:files:first.txt", "old\n", "new\n")],
            window,
            cx,
        );
        window.click("review-pending", cx);
        assert!(
            window
                .find_all("review-item-target:settings:/auto")
                .is_empty()
        );
        window.click("choose-source-target:files:first.txt", cx);
        assert!(
            window
                .find_all("review-item-target:files:first.txt")
                .is_empty()
        );
        assert!(
            !window
                .find_all("choose-target-target:files:second.txt")
                .is_empty()
        );
        window.click("choose-target-target:files:second.txt", cx);
        assert!(
            window
                .find_all("choose-target-target:files:second.txt")
                .is_empty()
        );
        window.click("review-pending", cx);
        window.click("review-item-target:files:first.txt", cx);
        assert_eq!(
            window
                .find("choose-source-target:files:first.txt")
                .checked(),
            Some(true)
        );
        assert!(window.find_all("next-review-decision").is_empty());
        window.click("back-to-selection", cx);
        let state = view.read(cx);
        assert!(state.snapshot.text_previews.is_empty());
        assert!(state.snapshot.file_review_details.is_empty());
        assert!(state.review_diffs.is_empty());
        assert!(state.choices.is_empty());
        assert!(matches!(received.try_recv(), Ok(Command::CancelReview)));
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn policy_marker_hashes_and_safe_opaque_previews_remain_discoverable(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    cx.update_window(handle, |_, window, cx| {
        let id = "target:files:permissions.json";
        view.update(cx, |view, _| {
            view.snapshot.file_review_details.insert(
                id.into(),
                crate::ui_model::FileReviewDetails {
                    source_hash: Some("source-hash".into()),
                    target_hash: Some("target-hash".into()),
                    permission_policy: true,
                },
            );
        });
        review_snapshot(
            &view,
            vec![review_change(id, "permissions.json", true)],
            vec![(id, "{\"allow\": false}\n", "{\"allow\": true}\n")],
            window,
            cx,
        );
        assert!(!window.find_all("permission-policy-warning").is_empty());
        assert!(
            window
                .find("choose-source-target:files:permissions.json")
                .description()
                .unwrap()
                .contains("SHA256 source-hash")
        );
        assert!(
            window
                .find("choose-target-target:files:permissions.json")
                .description()
                .unwrap()
                .contains("SHA256 target-hash")
        );
        window.click("choose-target-target:files:permissions.json", cx);
        assert!(
            !window.find_all("permission-policy-warning").is_empty(),
            "policy marker does not depend on choice"
        );
        let mut private =
            review_change("target:models:/", "models.json · entire private file", true);
        private.before = "Private values hidden. SHA256 old".into();
        private.after = "Private values hidden. SHA256 new".into();
        review_snapshot(&view, vec![private], vec![], window, cx);
        assert!(view.read(cx).review_diffs.is_empty());
        assert!(!window.find_all("opaque-preview-target:models:/").is_empty());
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

fn click_review_edge(window: &mut gpui_kit::Window, source: bool, cx: &mut gpui_kit::App) {
    let body = window.find("review-code-surface").bounds();
    hover_point(
        window,
        gpui_kit::point(
            body.left() + body.size.width * if source { 0.25 } else { 0.75 },
            body.bottom() - px(20.),
        ),
        cx,
    );
    window.click(
        if source {
            "review-source-edge"
        } else {
            "review-target-edge"
        },
        cx,
    );
}

fn click_review_content(window: &mut gpui_kit::Window, source: bool, cx: &mut gpui_kit::App) {
    let body = window.find("review-code-surface").bounds();
    let point = gpui_kit::point(
        body.left() + body.size.width * if source { 0.25 } else { 0.75 },
        body.bottom() - px(20.),
    );
    hover_point(window, point, cx);
    click_point(window, point, cx);
}

fn separated_hunks() -> (String, String) {
    let context = (0..10)
        .map(|ix| format!("unchanged {ix}\n"))
        .collect::<String>();
    (
        format!("source first\n{context}source middle\n{context}source last\n"),
        format!("target first\n{context}target middle\n{context}target last\n"),
    )
}

#[gpui_kit::test]
fn review_hunks_have_visible_position_and_bounded_navigation(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    let id = "target:files:three.txt";
    let (source, target) = separated_hunks();
    cx.update_window(handle, |_, window, cx| {
        review_snapshot(
            &view,
            vec![review_change(id, "three.txt", true)],
            vec![(id, &target, &source)],
            window,
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).review_diffs[id].hunks.len(), 3);
        assert!(!window.find_all_by_label("Hunk 1 of 3").is_empty());
        assert_eq!(window.find("previous-diff-hunk").disabled(), Some(true));
        window.click("next-diff-hunk", cx);
        assert_eq!(view.read(cx).review_diffs[id].active, 1);
        assert!(!window.find_all_by_label("Hunk 2 of 3").is_empty());
        window.click("next-diff-hunk", cx);
        assert_eq!(view.read(cx).review_diffs[id].active, 2);
        assert_eq!(window.find("next-diff-hunk").disabled(), Some(true));
        window.click("next-diff-hunk", cx);
        assert_eq!(
            view.read(cx).review_diffs[id].active,
            2,
            "last hunk never wraps"
        );
        window.click("previous-diff-hunk", cx);
        assert_eq!(view.read(cx).review_diffs[id].active, 1);
        assert!(
            view.read(cx).choices.is_empty(),
            "hunk navigation never chooses a version"
        );
        assert!(
            view.read(cx).review_change_id.is_none(),
            "hunks are not sidebar items"
        );
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn content_choices_and_overlay_navigation_are_independent(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    let id = "target:files:three.txt";
    let (source, target) = separated_hunks();
    cx.update_window(handle, |_, window, cx| {
        review_snapshot(
            &view,
            vec![review_change(id, "three.txt", true)],
            vec![(id, &target, &source)],
            window,
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("review-source-edge").disabled(), Some(true));
        click_review_edge(window, false, cx);
        assert_eq!(view.read(cx).review_diffs[id].active, 1);
        assert!(
            view.read(cx).choices.is_empty(),
            "an undecided arrow only navigates"
        );
        assert_eq!(window.find("apply-reviewed").disabled(), Some(true));
        click_review_content(window, true, cx);
        assert_eq!(view.read(cx).choices[id], Choice::Source);
        assert_eq!(
            view.read(cx).review_diffs[id].active,
            1,
            "content never navigates"
        );
        click_review_edge(window, false, cx);
        assert_eq!(view.read(cx).review_diffs[id].active, 2);
        assert_eq!(
            view.read(cx).choices[id],
            Choice::Source,
            "right arrow must preserve a source choice"
        );
        assert_eq!(window.find("review-target-edge").disabled(), Some(true));
        click_review_edge(window, false, cx);
        assert_eq!(
            view.read(cx).choices[id],
            Choice::Source,
            "disabled arrow must not choose the content beneath it"
        );
        click_review_content(window, false, cx);
        assert_eq!(view.read(cx).choices[id], Choice::Target);
        click_review_edge(window, true, cx);
        assert_eq!(view.read(cx).review_diffs[id].active, 1);
        assert_eq!(
            view.read(cx).choices[id],
            Choice::Target,
            "left arrow must preserve a target choice"
        );
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn review_hunk_keyboard_and_unchanged_snapshot_preserve_state(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    let id = "target:files:three.txt";
    let (source, target) = separated_hunks();
    cx.update_window(handle, |_, window, cx| {
        review_snapshot(
            &view,
            vec![review_change(id, "three.txt", true)],
            vec![(id, &target, &source)],
            window,
            cx,
        );
        window.press("alt-right", cx);
        assert_eq!(view.read(cx).choices[id], Choice::Target);
        window.press("alt-down", cx);
        assert_eq!(view.read(cx).review_diffs[id].active, 1);
        let original_state = view.read(cx).review_diffs[id].state.clone();
        review_snapshot(
            &view,
            vec![review_change(id, "three.txt", true)],
            vec![(id, &target, &source)],
            window,
            cx,
        );
        assert_eq!(view.read(cx).review_diffs[id].active, 1);
        assert_eq!(view.read(cx).review_diffs[id].state, original_state);
        window.press("alt-up", cx);
        assert_eq!(view.read(cx).review_diffs[id].active, 0);
        window.press("alt-left", cx);
        assert_eq!(view.read(cx).choices[id], Choice::Source);
        view.update(cx, |view, _| view.busy = Some("Working".into()));
        window.render_frame(cx);
        window.press("alt-down", cx);
        window.press("alt-right", cx);
        assert_eq!(view.read(cx).review_diffs[id].active, 0);
        assert_eq!(view.read(cx).choices[id], Choice::Source);
        view.update(cx, |view, _| view.busy = None);
        review_snapshot(
            &view,
            vec![review_change(id, "three.txt", true)],
            vec![(id, "one old\n", "one new\n")],
            window,
            cx,
        );
        assert_eq!(view.read(cx).review_diffs[id].active, 0);
        assert_eq!(view.read(cx).review_diffs[id].hunks.len(), 1);
        assert_ne!(view.read(cx).review_diffs[id].state, original_state);
        assert_eq!(window.find("previous-diff-hunk").disabled(), Some(true));
        assert_eq!(window.find("next-diff-hunk").disabled(), Some(true));
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn review_content_selection_gestures_do_not_choose(cx: &mut TestAppContext) {
    use gpui_kit::{
        Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput,
    };
    let (handle, view, received) = window(cx, true);
    let id = "target:files:selection.txt";
    cx.update_window(handle, |_, window, cx| {
        review_snapshot(
            &view,
            vec![review_change(id, "selection.txt", true)],
            vec![(id, "target content\n", "source content\n")],
            window,
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let point = window.find_by_label("source content").bounds().center();
        for (click_count, modifiers) in [
            (2, Modifiers::default()),
            (3, Modifiers::default()),
            (
                1,
                Modifiers {
                    shift: true,
                    ..Default::default()
                },
            ),
            (
                1,
                Modifiers {
                    control: true,
                    ..Default::default()
                },
            ),
        ] {
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    position: point,
                    button: MouseButton::Left,
                    click_count,
                    modifiers,
                    ..Default::default()
                }),
                cx,
            );
            window.render_frame(cx);
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    position: point,
                    button: MouseButton::Left,
                    click_count,
                    modifiers,
                }),
                cx,
            );
            window.render_frame(cx);
            assert!(
                view.read(cx).choices.is_empty(),
                "text selection and modifiers must not choose"
            );
        }
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                position: point,
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }),
            cx,
        );
        window.render_frame(cx);
        for position in [point + gpui_kit::point(px(25.), px(0.)), point] {
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position,
                    pressed_button: Some(MouseButton::Left),
                    ..Default::default()
                }),
                cx,
            );
            window.render_frame(cx);
        }
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                position: point,
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }),
            cx,
        );
        window.render_frame(cx);
        assert!(
            view.read(cx).choices.is_empty(),
            "dragging back to the start is still text selection"
        );
        let gutter = window.within("old-gutter").find(("line", 0usize)).bounds();
        click_point(window, gutter.center(), cx);
        assert!(
            view.read(cx).choices.is_empty(),
            "line-number selection is not a version choice"
        );
        click_point(window, point, cx);
        assert_eq!(
            view.read(cx).choices[id],
            Choice::Source,
            "fresh clicks work after selecting text"
        );
        view.update(cx, |view, _| view.choices.clear());
        window.render_frame(cx);
        let body = window.find("review-code-surface").bounds();
        let blank = gpui_kit::point(
            body.left() + body.size.width * 0.25,
            body.bottom() - px(20.),
        );
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                position: blank,
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }),
            cx,
        );
        window.render_frame(cx);
        for position in [gpui_kit::point(body.left() - px(50.), blank.y), blank] {
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position,
                    pressed_button: Some(MouseButton::Left),
                    ..Default::default()
                }),
                cx,
            );
            window.render_frame(cx);
        }
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                position: blank,
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }),
            cx,
        );
        window.render_frame(cx);
        assert!(
            view.read(cx).choices.is_empty(),
            "outside-and-back drag is not a choice"
        );
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                position: point,
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }),
            cx,
        );
        window.render_frame(cx);
        review_snapshot(
            &view,
            vec![review_change(id, "selection.txt", true)],
            vec![(id, "changed target\n", "changed source\n")],
            window,
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                position: point,
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }),
            cx,
        );
        window.render_frame(cx);
        assert!(
            view.read(cx).choices.is_empty(),
            "a replaced preview invalidates an in-flight press"
        );
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn pure_add_delete_hidden_and_unavailable_previews_are_safe(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    let id = "target:files:empty.txt";
    cx.update_window(handle, |_, window, cx| {
        for (source, target, side) in [
            (
                "only source\n",
                "",
                gpui_kit::component::diff::DiffSide::Original,
            ),
            (
                "",
                "only target\n",
                gpui_kit::component::diff::DiffSide::Modified,
            ),
        ] {
            review_snapshot(
                &view,
                vec![review_change(id, "empty.txt", true)],
                vec![(id, target, source)],
                window,
                cx,
            );
            let diff = &view.read(cx).review_diffs[id];
            assert_eq!(diff.hunks.len(), 1);
            assert_eq!(diff.hunks[0][0].side(), side);
            assert_eq!(window.find("next-diff-hunk").disabled(), Some(true));
        }
        review_snapshot(
            &view,
            vec![review_change(id, "empty.txt", true)],
            vec![(id, "same\n", "same\n")],
            window,
            cx,
        );
        assert!(view.read(cx).review_diffs.is_empty());
        assert!(window.find_all("next-diff-hunk").is_empty());
        let private_id = "target:models:/";
        review_snapshot(
            &view,
            vec![review_change(
                private_id,
                "models.json · entire private file",
                false,
            )],
            vec![],
            window,
            cx,
        );
        assert!(view.read(cx).review_diffs.is_empty());
        assert_eq!(window.find("review-source-edge").disabled(), Some(true));
        let body = window.find("review-code-surface").bounds();
        click_point(
            window,
            gpui_kit::point(
                body.left() + body.size.width * 0.25,
                body.bottom() - px(20.),
            ),
            cx,
        );
        assert!(!view.read(cx).choices.contains_key(private_id));
        assert_eq!(window.find("review-target-edge").disabled(), Some(true));
        click_review_content(window, false, cx);
        assert_eq!(view.read(cx).choices[private_id], Choice::Target);
        assert_eq!(window.find("review-target-edge").disabled(), Some(true));
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn navigation_reveals_offscreen_hunks_and_preserves_copy_selection(cx: &mut TestAppContext) {
    use gpui_kit::component::diff::{DiffLineRange, DiffSide};
    let (handle, view, received) = window(cx, true);
    let id = "target:files:long.txt";
    let context = (0..10)
        .map(|ix| format!("context {ix}\n"))
        .collect::<String>();
    let source = format!("{}{context}final source hunk\n", tall_review_hunk("source"));
    let target = format!("{}{context}final target hunk\n", tall_review_hunk("target"));
    cx.update_window(handle, |_, window, cx| {
        review_snapshot(
            &view,
            vec![review_change(id, "long.txt", true)],
            vec![(id, &target, &source)],
            window,
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(view.read(cx).review_diffs[id].hunks.len(), 2);
        window.render_frame(cx);
        assert!(window.find_all_by_label("final source hunk").is_empty());
        let state = view.read(cx).review_diffs[id].state.clone();
        state.update(cx, |state, cx| {
            state.set_selected_lines(
                Some(DiffLineRange::new("preview.txt", DiffSide::Original, 1, 2)),
                cx,
            )
        });
        assert_eq!(state.read(cx).selected_text(cx), "source 0\nsource 1\n");
        view.update(cx, |view, cx| view.navigate_review(id, true, cx));
        window.render_frame(cx);
        let body = window.find("review-code-surface").bounds();
        let last = window.find_by_label("final source hunk").bounds();
        assert!(last.top() >= body.top() && last.bottom() <= body.bottom());
        assert_eq!(
            state.read(cx).selected_text(cx),
            "source 0\nsource 1\n",
            "highlight never overwrites text selection"
        );
        window.click("previous-diff-hunk", cx);
        let first = window.find_by_label("source 0").bounds();
        assert!(first.top() >= body.top() && first.bottom() <= body.bottom());
        assert!(view.read(cx).choices.is_empty());
        window.scroll(
            "review-code-surface",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-160.))),
            cx,
        );
        assert!(
            view.read(cx).choices.is_empty(),
            "scrolling does not choose"
        );
        let scrollbar = gpui_kit::point(body.right() - px(3.), body.top() + px(30.));
        hover_point(window, scrollbar, cx);
        click_point(window, scrollbar, cx);
        assert!(
            view.read(cx).choices.is_empty(),
            "scrollbar click does not choose"
        );
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn expanding_native_context_does_not_choose_a_version(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    let id = "target:files:fold.txt";
    cx.update_window(handle, |_, window, cx| {
        review_snapshot(
            &view,
            vec![review_change(id, "fold.txt", true)],
            vec![(id, "target\none\ntwo\nthree\n", "source\none\ntwo\nthree\n")],
            window,
            cx,
        );
        let state = view.read(cx).review_diffs[id].state.clone();
        state.update(cx, |state, cx| state.set_context_lines(Some(0), cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(("expand", 1usize), cx);
        assert!(
            view.read(cx).choices.is_empty(),
            "native expand control must not bubble into a choice"
        );
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn transparent_arrows_follow_pane_hover_and_keyboard_focus_without_layout_shift(
    cx: &mut TestAppContext,
) {
    let (handle, view, received) = window(cx, true);
    let id = "target:files:overlay.txt";
    let (source, target) = separated_hunks();
    cx.update_window(handle, |_, window, cx| {
        review_snapshot(
            &view,
            vec![review_change(id, "overlay.txt", true)],
            vec![(id, &target, &source)],
            window,
            cx,
        );
        window.click("next-diff-hunk", cx);
        window.click("next-diff-hunk", cx);
        hover_point(window, gpui_kit::point(px(100.), px(30.)), cx);
        assert!(window.find_all("review-interaction-hint").is_empty());
        let body = window.find("review-code-surface").bounds();
        assert!(!window.find("review-source-edge").visible());
        assert!(!window.find("review-target-edge").visible());
        hover_point(
            window,
            gpui_kit::point(
                body.left() + body.size.width * 0.25,
                body.bottom() - px(20.),
            ),
            cx,
        );
        assert!(window.find("review-source-edge").visible());
        assert!(!window.find("review-target-edge").visible());
        assert_eq!(window.find("review-code-surface").bounds(), body);
        hover_point(
            window,
            gpui_kit::point(
                body.left() + body.size.width * 0.75,
                body.bottom() - px(20.),
            ),
            cx,
        );
        assert!(!window.find("review-source-edge").visible());
        assert!(window.find("review-target-edge").visible());
        assert_eq!(window.find("review-code-surface").bounds(), body);
        let arrow = window.find("review-target-edge").bounds();
        assert!(arrow.left() > body.center().x && arrow.right() < body.right());
        hover_point(window, arrow.center(), cx);
        assert!(
            window.find("review-target-edge").visible(),
            "overlay remains visible while entering it"
        );
        hover_point(window, gpui_kit::point(px(100.), px(30.)), cx);
        for _ in 0..24 {
            window.press("tab", cx);
            if window.find("review-source-edge").focused() == Some(true) {
                break;
            }
        }
        assert_eq!(window.find("review-source-edge").focused(), Some(true));
        assert!(
            window.find("review-source-edge").visible(),
            "keyboard focus reveals the overlay"
        );
        window.press("enter", cx);
        assert!(
            view.read(cx).choices.is_empty(),
            "keyboard arrow does not choose"
        );
        assert_eq!(view.read(cx).review_diffs[id].active, 1);
        window.press("space", cx);
        assert_eq!(view.read(cx).review_diffs[id].active, 0);
        assert!(view.read(cx).choices.is_empty());
        assert_eq!(window.find("review-source-edge").disabled(), Some(true));
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn review_navigation_crosses_items_only_after_final_hunk(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    let first = "target:files:first.txt";
    let second = "target:files:second.txt";
    let (source, target) = separated_hunks();
    cx.update_window(handle, |_, window, cx| {
        review_snapshot(
            &view,
            vec![
                review_change(first, "first.txt", true),
                review_change(second, "second.txt", true),
            ],
            vec![(first, &target, &source), (second, &target, &source)],
            window,
            cx,
        );
        for hunk in 1..3 {
            window.click("next-diff-hunk", cx);
            assert_eq!(view.read(cx).active_review_change().unwrap().id, first);
            assert_eq!(view.read(cx).review_diffs[first].active, hunk);
        }
        assert!(
            !window
                .find_all_by_label("Next item: second.txt · Target")
                .is_empty()
        );
        assert_ne!(window.find("next-diff-hunk").disabled(), Some(true));
        window.click("next-diff-hunk", cx);
        assert_eq!(view.read(cx).active_review_change().unwrap().id, second);
        assert_eq!(view.read(cx).review_diffs[second].active, 0);
        window.click("next-diff-hunk", cx);
        assert_eq!(view.read(cx).review_diffs[second].active, 1);
        window.click("review-item-target:files:first.txt", cx);
        window.click("next-diff-hunk", cx);
        assert_eq!(
            view.read(cx).review_diffs[second].active,
            0,
            "forward entry resets a previously visited item's hunk"
        );
        window.click("previous-diff-hunk", cx);
        assert_eq!(view.read(cx).active_review_change().unwrap().id, first);
        assert_eq!(
            view.read(cx).review_diffs[first].active,
            2,
            "reverse entry starts at the final hunk"
        );
        for _ in 0..2 {
            window.press("alt-up", cx);
        }
        assert_eq!(window.find("previous-diff-hunk").disabled(), Some(true));
        window.press("alt-up", cx);
        assert_eq!(view.read(cx).active_review_change().unwrap().id, first);
        assert_eq!(view.read(cx).review_diffs[first].active, 0);
        for _ in 0..5 {
            window.press("alt-down", cx);
        }
        assert_eq!(view.read(cx).active_review_change().unwrap().id, second);
        assert_eq!(view.read(cx).review_diffs[second].active, 2);
        assert_eq!(window.find("next-diff-hunk").disabled(), Some(true));
        window.press("alt-down", cx);
        assert_eq!(
            view.read(cx).review_diffs[second].active,
            2,
            "end of review does not wrap"
        );
        assert!(view.read(cx).choices.is_empty());
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn review_arrows_preserve_independent_choices_across_files(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    let first = "target:files:first.txt";
    let second = "target:files:second.txt";
    cx.update_window(handle, |_, window, cx| {
        let mut automatic = review_change(first, "first.txt", true);
        automatic.conflict = false;
        review_snapshot(
            &view,
            vec![automatic, review_change(second, "second.txt", true)],
            vec![
                (first, "old\n", "new\n"),
                (second, "old two\n", "new two\n"),
            ],
            window,
            cx,
        );
        click_review_edge(window, false, cx);
        assert_eq!(view.read(cx).active_review_change().unwrap().id, second);
        assert!(
            view.read(cx).choices.is_empty(),
            "implicit and undecided choices remain unchanged"
        );
        assert_eq!(view.read(cx).unresolved(), 1);
        assert_eq!(window.find("review-target-edge").disabled(), Some(true));
        assert!(window.find("review-target-edge").visible());
        click_review_edge(window, true, cx);
        assert_eq!(view.read(cx).active_review_change().unwrap().id, first);
        click_review_content(window, true, cx);
        click_review_edge(window, false, cx);
        assert_eq!(view.read(cx).active_review_change().unwrap().id, second);
        assert_eq!(view.read(cx).choices[first], Choice::Source);
        assert!(!view.read(cx).choices.contains_key(second));
        click_review_content(window, false, cx);
        click_review_edge(window, true, cx);
        assert_eq!(view.read(cx).active_review_change().unwrap().id, first);
        assert_eq!(view.read(cx).choices[first], Choice::Source);
        assert_eq!(view.read(cx).choices[second], Choice::Target);
        assert_eq!(window.find("review-source-edge").disabled(), Some(true));
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn boundary_navigation_uses_pending_order_and_stable_target_ids(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    let first = "target:files:same.txt";
    let automatic = "target:settings:/automatic";
    let second = "other:files:same.txt";
    let resolved = "other:settings:/resolved";
    let last = "other:models:/";
    cx.update_window(handle, |_, window, cx| {
        let mut implicit = review_change(automatic, "settings.json/automatic", true);
        implicit.conflict = false;
        let mut other = review_change(second, "same.txt", true);
        other.device = "Other target".into();
        review_snapshot(
            &view,
            vec![
                review_change(first, "same.txt", true),
                implicit,
                other,
                review_change(resolved, "settings.json/resolved", true),
                review_change(last, "models.json · private file", false),
            ],
            vec![(first, "old\n", "new\n"), (second, "old\n", "new\n")],
            window,
            cx,
        );
        view.update(cx, |view, _| {
            view.choices.insert(resolved.into(), Choice::Target);
        });
        window.click("review-pending", cx);
        assert_eq!(view.read(cx).active_review_change().unwrap().id, first);
        window.click("next-diff-hunk", cx);
        assert_eq!(
            view.read(cx).active_review_change().unwrap().id,
            second,
            "same path on a different target is a distinct item"
        );
        click_review_edge(window, false, cx);
        assert_eq!(
            view.read(cx).active_review_change().unwrap().id,
            last,
            "resolved and implicit decisions are skipped"
        );
        click_review_edge(window, true, cx);
        assert_eq!(view.read(cx).active_review_change().unwrap().id, second);
        assert_eq!(
            view.read(cx).choices.len(),
            1,
            "Pending navigation does not resolve anything"
        );
        assert!(
            !window
                .find_all("review-item-target:files:same.txt")
                .is_empty()
        );
        assert!(
            !window
                .find_all("review-item-other:files:same.txt")
                .is_empty()
        );
        click_review_content(window, false, cx);
        assert_eq!(view.read(cx).choices[second], Choice::Target);
        assert_eq!(
            view.read(cx).active_review_change().unwrap().id,
            first,
            "choosing removes only this item; no second navigation runs"
        );
        view.update(cx, |view, cx| {
            view.navigate_review(second, true, cx);
            view.navigate_review(second, false, cx);
        });
        assert_eq!(
            view.read(cx).choices[second],
            Choice::Target,
            "a stale item callback cannot change the removed item"
        );
        assert!(!view.read(cx).choices.contains_key(first));
        click_review_content(window, false, cx);
        assert_eq!(view.read(cx).active_review_change().unwrap().id, last);
        assert!(!view.read(cx).choices.contains_key(last));
        click_review_content(window, false, cx);
        assert!(view.read(cx).active_review_change().is_none());
        window.press("alt-down", cx);
        window.press("alt-up", cx);
        assert!(window.find_all("next-diff-hunk").is_empty());
        assert_eq!(view.read(cx).choices.len(), 4);
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn boundary_navigation_traverses_safe_previews_without_choices_or_writes(cx: &mut TestAppContext) {
    use gpui_kit::component::WindowExt;
    let (handle, view, received) = window(cx, true);
    let ids = [
        "target:files:first.txt",
        "target:models:/",
        "target:files:equal.txt",
        "target:files:unavailable.txt",
        "target:files:last.txt",
    ];
    cx.update_window(handle, |_, window, cx| {
        review_snapshot(
            &view,
            vec![
                review_change(ids[0], "first.txt", true),
                review_change(ids[1], "models.json · private file", true),
                review_change(ids[2], "equal.txt", true),
                review_change(ids[3], "unavailable.txt", false),
                review_change(ids[4], "last.txt", true),
            ],
            vec![
                (ids[0], "old\n", "new\n"),
                (ids[2], "same\n", "same\n"),
                (ids[4], "old\n", "new\n"),
            ],
            window,
            cx,
        );
        view.update(cx, |view, _| view.busy = Some("Working".into()));
        window.render_frame(cx);
        window.press("alt-down", cx);
        assert_eq!(view.read(cx).active_review_change().unwrap().id, ids[0]);
        view.update(cx, |view, _| view.busy = None);
        window.open_alert_dialog(cx, |dialog, _, _| dialog.title("Test dialog").confirm());
        window.render_frame(cx);
        window.press("alt-down", cx);
        assert_eq!(view.read(cx).active_review_change().unwrap().id, ids[0]);
        window.within("dialog").click("cancel", cx);
        for id in &ids[1..] {
            window.click("next-diff-hunk", cx);
            assert_eq!(&view.read(cx).active_review_change().unwrap().id, id);
            assert!(
                !window.find_all("previous-diff-hunk").is_empty(),
                "safe previews still expose navigation"
            );
            if *id == ids[3] {
                assert_ne!(
                    window.find("review-source-edge").disabled(),
                    Some(true),
                    "unavailable source does not block previous navigation"
                );
                assert_eq!(
                    window
                        .find("choose-source-target:files:unavailable.txt")
                        .disabled(),
                    Some(true)
                );
            }
        }
        for id in ids[..4].iter().rev() {
            click_review_edge(window, true, cx);
            assert_eq!(&view.read(cx).active_review_change().unwrap().id, id);
        }
        assert!(view.read(cx).choices.is_empty());
        view.update(cx, |view, _| view.snapshot.plan_id = None);
        view.update(cx, |view, cx| view.navigate_review(ids[0], true, cx));
        assert_eq!(
            view.read(cx).active_review_change().unwrap().id,
            ids[0],
            "no plan means no navigation"
        );
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn entering_previous_item_reveals_its_last_hunk(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    let first = "target:files:long.txt";
    let second = "target:files:short.txt";
    let unchanged = (0..10)
        .map(|i| format!("context {i}\n"))
        .collect::<String>();
    let source = format!(
        "{}{unchanged}last source hunk\n",
        tall_review_hunk("source")
    );
    let target = format!(
        "{}{unchanged}last target hunk\n",
        tall_review_hunk("target")
    );
    cx.update_window(handle, |_, window, cx| {
        review_snapshot(
            &view,
            vec![
                review_change(first, "long.txt", true),
                review_change(second, "short.txt", true),
            ],
            vec![(first, &target, &source), (second, "old\n", "new\n")],
            window,
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(view.read(cx).review_diffs[first].hunks.len(), 2);
        window.render_frame(cx);
        window.click("review-item-target:files:short.txt", cx);
        window.click("previous-diff-hunk", cx);
        assert_eq!(view.read(cx).active_review_change().unwrap().id, first);
        assert_eq!(view.read(cx).review_diffs[first].active, 1);
        let body = window.find("review-code-surface").bounds();
        let anchor = window.find_by_label("last source hunk").bounds();
        assert!(
            anchor.top() >= body.top() && anchor.bottom() <= body.bottom(),
            "previous item's final hunk must actually be visible"
        );
        window.click("next-diff-hunk", cx);
        window.click("review-item-target:files:long.txt", cx);
        window.click("previous-diff-hunk", cx);
        window.click("next-diff-hunk", cx);
        window.click("next-diff-hunk", cx);
        assert_eq!(view.read(cx).active_review_change().unwrap().id, second);
        assert!(view.read(cx).choices.is_empty());
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn changing_review_item_during_a_press_cannot_choose_the_destination(cx: &mut TestAppContext) {
    use gpui_kit::{MouseButton, MouseDownEvent, MouseUpEvent, PlatformInput};
    let (handle, view, received) = window(cx, true);
    let first = "target:files:first.txt";
    let second = "target:files:second.txt";
    cx.update_window(handle, |_, window, cx| {
        review_snapshot(
            &view,
            vec![
                review_change(first, "first.txt", true),
                review_change(second, "second.txt", true),
                review_change("target:files:third.txt", "third.txt", true),
            ],
            vec![
                (first, "old\n", "new\n"),
                (second, "old two\n", "new two\n"),
                ("target:files:third.txt", "old three\n", "new three\n"),
            ],
            window,
            cx,
        );
        for control in ["content", "review-target-edge", "next-diff-hunk"] {
            view.update(cx, |view, cx| {
                view.review_change_id = None;
                cx.notify();
            });
            window.render_frame(cx);
            let body = window.find("review-code-surface").bounds();
            let content = gpui_kit::point(
                body.left() + body.size.width * 0.75,
                body.bottom() - px(20.),
            );
            hover_point(window, content, cx);
            let position = if control == "content" {
                content
            } else {
                window.find(control).bounds().center()
            };
            hover_point(window, position, cx);
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    position,
                    button: MouseButton::Left,
                    click_count: 1,
                    ..Default::default()
                }),
                cx,
            );
            window.render_frame(cx);
            window.press("alt-down", cx);
            assert_eq!(view.read(cx).active_review_change().unwrap().id, second);
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    position,
                    button: MouseButton::Left,
                    click_count: 1,
                    ..Default::default()
                }),
                cx,
            );
            window.render_frame(cx);
            assert_eq!(
                view.read(cx).active_review_change().unwrap().id,
                second,
                "old press cannot advance an extra item ({control})"
            );
            assert!(
                view.read(cx).choices.is_empty(),
                "old press must not choose new content ({control})"
            );
        }
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn arrow_hover_has_no_version_choice_cue_and_content_hover_recovers(cx: &mut TestAppContext) {
    let painted = |window: &gpui_kit::Window, expected: &str| {
        gpui_kit::base::test_support::texts(window)
            .iter()
            .any(|text| text.text() == expected && text.visible())
    };
    let (handle, view, received) = window(cx, true);
    let id = "target:files:hover.txt";
    let (source, target) = separated_hunks();
    cx.update_window(handle, |_, window, cx| {
        review_snapshot(
            &view,
            vec![review_change(id, "hover.txt", true)],
            vec![(id, &target, &source)],
            window,
            cx,
        );
        window.click("next-diff-hunk", cx);
        click_review_content(window, true, cx);
        assert_eq!(view.read(cx).choices[id], Choice::Source);
        let body = window.find("review-code-surface").bounds();
        let target_point = gpui_kit::point(
            body.left() + body.size.width * 0.75,
            body.bottom() - px(20.),
        );
        hover_point(window, target_point, cx);
        assert!(painted(window, "○ Keep on Target"));
        window.hover("review-target-edge", cx);
        assert_eq!(view.read(cx).review_hover_owner, Some("target-edge"));
        assert!(window.find("review-target-edge").visible());
        assert!(
            !painted(window, "○ Keep on Target"),
            "arrow hover must not advertise a target choice"
        );
        assert!(painted(window, "✓ Selected"));
        window.click("review-target-edge", cx);
        assert_eq!(view.read(cx).choices[id], Choice::Source);
        assert_eq!(view.read(cx).review_diffs[id].active, 2);
        assert!(!painted(window, "○ Keep on Target"));
        hover_point(window, target_point, cx);
        assert!(
            painted(window, "○ Keep on Target"),
            "ordinary content regains its choice cue"
        );
        click_review_content(window, false, cx);
        hover_point(
            window,
            gpui_kit::point(
                body.left() + body.size.width * 0.25,
                body.bottom() - px(20.),
            ),
            cx,
        );
        assert!(painted(window, "○ Use on Target →"));
        window.hover("review-source-edge", cx);
        assert_eq!(view.read(cx).review_hover_owner, Some("source-edge"));
        assert!(!painted(window, "○ Use on Target →"));
        assert!(painted(window, "✓ Selected"));
        window.click("review-source-edge", cx);
        assert_eq!(view.read(cx).choices[id], Choice::Target);
        assert_eq!(view.read(cx).review_diffs[id].active, 1);
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

fn full_file_page(change_id: &str, page: usize, format: &str) -> crate::ui_model::FilePreview {
    crate::ui_model::FilePreview {
        plan_id: "review-1".into(),
        change_id: change_id.into(),
        file_name: "models.json".into(),
        page,
        page_count: 2,
        format: format.into(),
        unresolved: 0,
        before: "{\n  \"fixture\": \"before\"\n}\n".into(),
        after: "{\n  \"fixture\": \"after\"\n}\n".into(),
    }
}

#[gpui_kit::test]
fn full_file_review_requires_reveal_and_never_selects_or_applies(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, _| {
            view.snapshot.changes[0].id = "target:models:/".into();
            view.snapshot.changes[0].path = "models.json · entire private file".into();
        });
        window.render_frame(cx);
        window.click("review-full-file", cx);
        assert!(
            received.try_recv().is_err(),
            "opening approval must not fetch contents"
        );
        window.within("dialog").click("cancel", cx);
        assert!(received.try_recv().is_err());
        window.click("review-full-file", cx);
        window.within("dialog").click("ok", cx);
        match received.try_recv().expect("approved read") {
            Command::ReviewFile {
                plan_id,
                change_id,
                page,
                choices,
            } => {
                assert_eq!(plan_id, "review-1");
                assert_eq!(change_id, "target:models:/");
                assert_eq!(page, 0);
                assert!(choices.is_empty());
            }
            _ => panic!("only full-file read is allowed"),
        }
        view.update(cx, |view, cx| {
            view.receive(
                crate::ui_model::Event::FilePreview(full_file_page("target:models:/", 0, "text")),
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(!window.find_all("full-file-review").is_empty());
        assert!(window.find_all("choose-source-target:models:/").is_empty());
        assert_eq!(window.find("previous-file-page").disabled(), Some(true));
        assert_ne!(window.find("next-file-page").disabled(), Some(true));
        assert_eq!(window.find("apply-reviewed").disabled(), Some(true));
        window.press("alt-left", cx);
        window.press("alt-right", cx);
        assert!(view.read(cx).choices.is_empty());
        assert!(received.try_recv().is_err());
        window.click("next-file-page", cx);
        assert!(matches!(
            received.try_recv(),
            Ok(Command::ReviewFile { page: 1, .. })
        ));
        view.update(cx, |view, cx| {
            view.receive(
                crate::ui_model::Event::FilePreview(full_file_page("target:models:/", 1, "text")),
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(window.find("next-file-page").disabled(), Some(true));
        assert_ne!(window.find("previous-file-page").disabled(), Some(true));
        assert!(view.read(cx).choices.is_empty());
        window.click("close-full-file", cx);
        assert!(view.read(cx).full_file_review.is_none());
        assert!(view.read(cx).file_review_request.is_none());
        assert!(!window.find_all("choose-source-target:models:/").is_empty());
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

#[gpui_kit::test]
fn full_file_replies_are_discarded_after_navigation_cancel_or_new_plan(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.request_file_review("change".into(), 0, cx)
        });
        assert!(matches!(
            received.try_recv(),
            Ok(Command::ReviewFile { .. })
        ));
        view.update(cx, |view, cx| {
            view.close_file_review();
            view.receive(
                crate::ui_model::Event::FilePreview(full_file_page("change", 0, "json")),
                window,
                cx,
            );
        });
        assert!(view.read(cx).full_file_review.is_none());
        view.update(cx, |view, cx| {
            view.request_file_review("change".into(), 0, cx);
            view.invalidate_review();
            view.receive(
                crate::ui_model::Event::FilePreview(full_file_page("change", 0, "json")),
                window,
                cx,
            );
        });
        assert!(view.read(cx).full_file_review.is_none());
        assert!(view.read(cx).snapshot.plan_id.is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn structured_full_file_request_carries_choices_without_resolving_them(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.snapshot.changes[0].id = "target:mcp:/mcpServers/argent/command".into();
            view.snapshot.changes[0].path = "mcp.json/mcpServers/argent/command".into();
            view.choices
                .insert(view.snapshot.changes[0].id.clone(), Choice::Target);
            view.request_file_review(view.snapshot.changes[0].id.clone(), 0, cx);
        });
        match received.try_recv().expect("file review read") {
            Command::ReviewFile { choices, .. } => assert_eq!(
                choices["target:mcp:/mcpServers/argent/command"],
                Choice::Target
            ),
            _ => panic!("only file review read is allowed"),
        }
        let mut page = full_file_page("target:mcp:/mcpServers/argent/command", 0, "json");
        page.unresolved = 2;
        page.file_name = "mcp.json".into();
        view.update(cx, |view, cx| {
            view.receive(crate::ui_model::Event::FilePreview(page), window, cx)
        });
        window.render_frame(cx);
        assert!(!view.read(cx).snapshot.command_details_reviewed);
        assert!(
            gpui_kit::base::test_support::texts(window)
                .iter()
                .any(|text| text.text().contains("2 undecided fields"))
        );
        assert!(
            !view
                .read(cx)
                .full_file_review
                .as_ref()
                .unwrap()
                .source
                .read(cx)
                .is_editable()
        );
        assert!(
            !view
                .read(cx)
                .full_file_review
                .as_ref()
                .unwrap()
                .target
                .read(cx)
                .is_editable()
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn unavailable_source_cannot_open_full_file_review(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, _| {
            view.snapshot.changes[0].source_available = false
        });
        window.render_frame(cx);
        assert_eq!(window.find("review-full-file").disabled(), Some(true));
        window.click("review-full-file", cx);
        assert!(received.try_recv().is_err());
        assert!(view.read(cx).file_review_request.is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn failed_full_file_read_restores_review_keyboard(cx: &mut TestAppContext) {
    let (handle, view, received) = window(cx, true);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.request_file_review("change".into(), 0, cx)
        });
        assert!(matches!(
            received.try_recv(),
            Ok(Command::ReviewFile { .. })
        ));
        view.update(cx, |view, cx| {
            view.receive(
                crate::ui_model::Event::Error("Target changed. Prepare again.".into()),
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(view.read(cx).file_review_request.is_none());
        window.press("alt-right", cx);
        assert_eq!(view.read(cx).choices.get("change"), Some(&Choice::Target));
        assert!(received.try_recv().is_err());
    })
    .unwrap();
}

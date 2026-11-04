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
                    view.snapshot = Snapshot {
                        devices: vec![
                            Device {
                                id: "source".into(),
                                name: "Source".into(),
                                address: "localhost".into(),
                                status: "Ready".into(),
                                is_local: true,
                            },
                            Device {
                                id: "target".into(),
                                name: "Target".into(),
                                address: "test-host".into(),
                                status: "Ready".into(),
                                is_local: false,
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
    match received.try_recv().expect("prepare command") {
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
    assert!(
        received.try_recv().is_err(),
        "dropping must only stage, not apply or prepare"
    );
}

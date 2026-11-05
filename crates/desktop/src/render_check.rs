//! Explicit real offscreen-renderer check. Uses the production workspace and
//! actual backend startup state. --fixture-review uses clearly synthetic review
//! rows for documentation only; no connection or apply is performed.
mod assets;
mod backend;
mod ui_model;
mod workspace;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::test::{TestQueryExt, TestWindowExt};
use gpui_kit::{AppContext, Bounds, HeadlessAppContext, WindowBounds, WindowOptions, px, size};
use std::{path::PathBuf, sync::Arc, time::Duration};

fn main() -> anyhow::Result<()> {
    if let Some(code) = sync_core::askpass::run_helper_if_requested() {
        std::process::exit(code);
    }
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| "pi-sync-workspace.png".into());
    let (commands, events) = backend::start();
    let mut initial = events.recv_timeout(Duration::from_secs(5))?;
    let fixture_overlay = std::env::args().find(|arg| {
        matches!(
            arg.as_str(),
            "--fixture-overlay-idle" | "--fixture-overlay-hover" | "--fixture-overlay-selected"
        )
    });
    let fixture_full_file = std::env::args().any(|arg| arg == "--fixture-full-file");
    let fixture_picker_macos = std::env::args().any(|arg| arg == "--fixture-picker-macos");
    let fixture_picker =
        fixture_picker_macos || std::env::args().any(|arg| arg == "--fixture-picker");
    let fixture_selection =
        fixture_picker || std::env::args().any(|arg| arg == "--fixture-selection");
    let fixture_next_file = std::env::args().any(|arg| arg == "--fixture-next-file");
    let fixture_navigation = fixture_next_file
        || fixture_overlay.is_some()
        || std::env::args().any(|arg| arg == "--fixture-navigation");
    let fixture_review = fixture_full_file
        || fixture_navigation
        || std::env::args().any(|arg| arg == "--fixture-review");
    if fixture_review || fixture_selection {
        let ui_model::Event::Snapshot(snapshot) = &mut initial else {
            anyhow::bail!("Missing startup snapshot");
        };
        snapshot.devices.push(ui_model::Device {
            id: "example".into(),
            name: "Example workstation (fixture)".into(),
            address: "fixture.example.invalid".into(),
            status: "Synthetic preview".into(),
            is_local: false,
            platform: "linux".into(),
        });
        snapshot.target_ids = vec!["example".into()];
        snapshot.plan_id = Some("synthetic-render-only".into());
        snapshot.selected_item_ids = vec!["settings".into(), "path:extensions".into()];
        snapshot.changes = vec![
            ui_model::Change { id: "example:settings:/theme".into(), device: "Example workstation (fixture)".into(), path: "settings.json/theme".into(), before: "dark".into(), after: "light".into(), conflict: true, source_available: true },
            ui_model::Change { id: "example:files:extensions/example.ts".into(), device: "Example workstation (fixture)".into(), path: "extensions/example.ts · entire file".into(), before: "No previous file".into(), after: "export const greeting = \"Hello\";\n\nWhole-file copy. Extension code may execute in Pi.\nSynthetic documentation fixture, not user configuration.".into(), conflict: true, source_available: true },
        ];
    }
    if fixture_selection {
        let ui_model::Event::Snapshot(snapshot) = &mut initial else {
            unreachable!()
        };
        snapshot.source_id = Some("local".into());
        snapshot
            .devices
            .iter_mut()
            .find(|device| device.id == "local")
            .unwrap()
            .address = "/home/example/.pi/agent".into();
        snapshot.selected_item_ids = vec!["path:extensions".into(), "path:skills".into()];
        snapshot.plan_id = None;
        snapshot.changes.clear();
        snapshot.has_last_selection = true;
    }
    if fixture_navigation {
        let ui_model::Event::Snapshot(snapshot) = &mut initial else {
            unreachable!()
        };
        let context = (1..=10)
            .map(|line| format!("// Unchanged context {line}\n"))
            .collect::<String>();
        let source = format!(
            "export const greeting = \"Hello\";\n{context}export const mode = \"safe\";\n{context}export const retries = 3;\n"
        );
        let target = format!(
            "export const greeting = \"Hi\";\n{context}export const mode = \"review\";\n{context}export const retries = 1;\n"
        );
        snapshot.changes[1].before = "Synthetic target preview".into();
        snapshot.changes[1].after = "Synthetic source preview".into();
        snapshot
            .text_previews
            .insert(snapshot.changes[1].id.clone(), (target, source));
        if fixture_next_file {
            let id = "example:files:extensions/next.ts".to_string();
            snapshot.changes.push(ui_model::Change {
                id: id.clone(),
                device: "Example workstation (fixture)".into(),
                path: "extensions/next.ts · entire file".into(),
                before: "Synthetic target preview".into(),
                after: "Synthetic source preview".into(),
                conflict: true,
                source_available: true,
            });
            snapshot.text_previews.insert(
                id,
                (
                    "export const enabled = false;\n".into(),
                    "export const enabled = true;\n".into(),
                ),
            );
        }
    }
    let mut context = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(assets::AppAssets),
        gpui_kit::platform::current_headless_renderer,
    );
    context.update(|cx| {
        gpui_kit::init(cx);
        if fixture_picker {
            cx.set_reduce_motion(true);
        }
        Theme::change(ThemeMode::Light, None, cx);
    });
    let (handle, view) = context.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(1240.), px(820.)),
                ))),
                focus: false,
                show: false,
                ..Default::default()
            },
            cx,
            move |window, cx| {
                cx.new(|cx| {
                    let mut view = workspace::Workspace::new(commands, events, window, cx);
                    view.receive(initial, window, cx);
                    if fixture_full_file {
                        view.show_fixture_file_review(ui_model::FilePreview {
                            plan_id: "synthetic-render-only".into(), change_id: "example:settings:/theme".into(),
                            file_name: "settings.json".into(), page: 0, page_count: 1, format: "json".into(), unresolved: 0,
                            before: "{\n  \"theme\": \"dark\",\n  \"defaultModel\": \"preserved-target-model\",\n  \"fixture\": true\n}\n".into(),
                            after: "{\n  \"theme\": \"light\",\n  \"defaultModel\": \"preserved-target-model\",\n  \"fixture\": true\n}\n".into(),
                        }, window, cx);
                    }
                    view
                })
            },
        )
    })?;
    context.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        if fixture_review {
            assert_eq!(window.find("apply-reviewed").disabled(), Some(true));
            assert!(window.find_all("prepare-review").is_empty());
        } else if window.find_all("prepare-review").is_empty() {
            assert_eq!(window.find("setup-next").disabled(), Some(true));
        } else {
            assert!(window.find_all("apply-reviewed").is_empty());
        }
    })?;
    // SVG assets are loaded asynchronously. Drain their tasks before capturing
    // a second real frame so device icons are present in the image.
    context.run_until_parked();
    context.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        if fixture_picker {
            if fixture_picker_macos {
                view.update(cx, |view, cx| {
                    view.show_local_path_picker_with_mixed(true, window, cx)
                });
                window.render_frame(cx);
            } else {
                window.click("add-path", cx);
            }
            let mixed = fixture_picker_macos
                || (cfg!(target_os = "macos") && cx.can_select_mixed_files_and_dirs());
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
        }
        if fixture_navigation {
            window.click("review-item-example:files:extensions/example.ts", cx);
            if let Some(state) = &fixture_overlay {
                window.click("next-diff-hunk", cx);
                let body = window.find("review-code-surface").bounds();
                let position = gpui_kit::point(
                    body.left() + body.size.width * 0.75,
                    body.bottom() - px(70.),
                );
                if state == "--fixture-overlay-selected" {
                    window.dispatch_event(
                        gpui_kit::PlatformInput::MouseDown(gpui_kit::MouseDownEvent {
                            position,
                            button: gpui_kit::MouseButton::Left,
                            click_count: 1,
                            ..Default::default()
                        }),
                        cx,
                    );
                    window.render_frame(cx);
                    window.dispatch_event(
                        gpui_kit::PlatformInput::MouseUp(gpui_kit::MouseUpEvent {
                            position,
                            button: gpui_kit::MouseButton::Left,
                            click_count: 1,
                            ..Default::default()
                        }),
                        cx,
                    );
                    window.render_frame(cx);
                }
                window.dispatch_event(
                    gpui_kit::PlatformInput::MouseMove(gpui_kit::MouseMoveEvent {
                        position: if state == "--fixture-overlay-idle" {
                            gpui_kit::point(px(100.), px(30.))
                        } else {
                            position
                        },
                        ..Default::default()
                    }),
                    cx,
                );
                window.render_frame(cx);
            } else {
                let body = window.find("review-code-surface").bounds();
                let source_position = gpui_kit::point(
                    body.left() + body.size.width * 0.25,
                    body.bottom() - px(70.),
                );
                window.dispatch_event(
                    gpui_kit::PlatformInput::MouseDown(gpui_kit::MouseDownEvent {
                        position: source_position,
                        button: gpui_kit::MouseButton::Left,
                        click_count: 1,
                        ..Default::default()
                    }),
                    cx,
                );
                window.render_frame(cx);
                window.dispatch_event(
                    gpui_kit::PlatformInput::MouseUp(gpui_kit::MouseUpEvent {
                        position: source_position,
                        button: gpui_kit::MouseButton::Left,
                        click_count: 1,
                        ..Default::default()
                    }),
                    cx,
                );
                window.render_frame(cx);
                window.dispatch_event(
                    gpui_kit::PlatformInput::MouseMove(gpui_kit::MouseMoveEvent {
                        position: gpui_kit::point(
                            body.left() + body.size.width * 0.75,
                            body.bottom() - px(70.),
                        ),
                        ..Default::default()
                    }),
                    cx,
                );
                window.render_frame(cx);
                window.click("review-target-edge", cx);
                assert!(!window.find_all_by_label("Hunk 2 of 3").is_empty());
                assert_eq!(
                    window
                        .find("choose-source-example:files:extensions/example.ts")
                        .checked(),
                    Some(true)
                );
                assert_ne!(
                    window
                        .find("choose-target-example:files:extensions/example.ts")
                        .checked(),
                    Some(true)
                );
                assert!(!gpui_kit::base::test_support::texts(window).iter().any(
                    |text| text.text() == "○ Keep on Example workstation (fixture)"
                        && text.visible()
                ));
                if fixture_next_file {
                    window.click("review-target-edge", cx);
                    assert!(!window.find_all_by_label("Hunk 3 of 3").is_empty());
                    window.click("review-target-edge", cx);
                    assert!(
                        !window
                            .find_all("choose-target-example:files:extensions/next.ts")
                            .is_empty()
                    );
                    assert!(!window.find_all_by_label("Hunk 1 of 1").is_empty());
                    assert_ne!(
                        window
                            .find("choose-target-example:files:extensions/next.ts")
                            .checked(),
                        Some(true)
                    );
                    assert_eq!(window.find("apply-reviewed").disabled(), Some(true));
                }
            }
        }
    })?;
    context.run_until_parked();
    context.update_window(handle, |_, window, cx| window.render_frame(cx))?;
    let screenshot = context.capture_screenshot(handle)?;
    anyhow::ensure!(
        screenshot.width() > 0 && screenshot.height() > 0,
        "renderer returned empty image"
    );
    let first = screenshot.get_pixel(0, 0);
    anyhow::ensure!(
        screenshot.pixels().any(|pixel| pixel != first),
        "renderer returned a blank image"
    );
    screenshot.save(&output)?;
    println!("Rendered production workspace to {}", output.display());
    Ok(())
}

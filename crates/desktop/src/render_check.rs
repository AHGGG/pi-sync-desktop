//! Explicit real offscreen-renderer check. Uses the production workspace and
//! actual backend startup state. --fixture-review uses clearly synthetic review
//! rows for documentation only; no connection or apply is performed.
mod assets;
mod backend;
mod ui_model;
mod workspace;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, Bounds, HeadlessAppContext, WindowBounds, WindowOptions, px, size};
use std::{path::PathBuf, sync::Arc, time::Duration};

fn main() -> anyhow::Result<()> {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| "pi-sync-workspace.png".into());
    let (commands, events) = backend::start();
    let mut initial = events.recv_timeout(Duration::from_secs(5))?;
    let fixture_review = std::env::args().any(|arg| arg == "--fixture-review");
    if fixture_review {
        let ui_model::Event::Snapshot(snapshot) = &mut initial else {
            anyhow::bail!("Missing startup snapshot");
        };
        snapshot.plan_id = Some("synthetic-render-only".into());
        snapshot.selected_item_ids = vec!["settings".into(), "path:extensions".into()];
        snapshot.changes = vec![
            ui_model::Change { id: "example:settings:/theme".into(), device: "Example workstation (fixture)".into(), path: "settings.json/theme".into(), before: "dark".into(), after: "light".into(), conflict: true, source_available: true },
            ui_model::Change { id: "example:files:extensions/example.ts".into(), device: "Example workstation (fixture)".into(), path: "extensions/example.ts · entire file".into(), before: "No previous file".into(), after: "export const greeting = \"Hello\";\n\nWhole-file copy. Extension code may execute in Pi.\nSynthetic documentation fixture, not user configuration.".into(), conflict: true, source_available: true },
        ];
    }
    let mut context = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(assets::AppAssets),
        gpui_kit::platform::current_headless_renderer,
    );
    context.update(|cx| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Light, None, cx);
    });
    let (handle, _) = context.update(|cx| {
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

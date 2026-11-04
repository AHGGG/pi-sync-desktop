//! Explicit real offscreen-renderer check. Uses the production workspace and
//! actual backend startup state; never substitutes a mock renderer or devices.
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
    let initial = events.recv_timeout(Duration::from_secs(5))?;
    let mut context = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
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
        assert_eq!(window.find("prepare-review").disabled(), Some(true));
    })?;
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

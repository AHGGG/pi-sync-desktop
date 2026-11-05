//! Native GPUI entry point. No webview or embedded browser runtime.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod assets;
mod backend;
mod ui_model;
mod workspace;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

fn main() {
    let (commands, events) = backend::start();
    gpui_kit::application()
        .with_assets(assets::AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Light, None, cx);
            // Physical pixels are used only at the platform window boundary.
            let bounds = Bounds::centered(None, size(px(1240.), px(820.)), cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(960.), px(640.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Pi Sync".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                cx,
                move |window, cx| {
                    cx.new(|cx| workspace::Workspace::new(commands, events, window, cx))
                },
            )
            .expect("could not open Pi Sync window");
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}

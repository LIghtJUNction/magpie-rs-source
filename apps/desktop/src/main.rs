#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd"
)))]
compile_error!("the GPUI desktop app has no backend for this target; use the magpie CLI");

use gpui::{
    App, Bounds, Context, Render, Window, WindowBounds, WindowOptions, div, prelude::*, px, size,
};
use gpui_platform::application;

struct MagpieWindow;

impl Render for MagpieWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .child(format!("Magpie {}", magpie_core::VERSION))
            .child("Gateway not started")
            .child(format!(
                "Default local endpoint: {}",
                magpie_service::default_gateway_addr()
            ))
    }
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(960.0), px(640.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| MagpieWindow),
        )
        .expect("failed to open the Magpie window");
        cx.activate(true);
    });
}

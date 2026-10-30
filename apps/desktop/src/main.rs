#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd"
)))]
compile_error!("the GPUI desktop app has no backend for this target; use the magpie CLI");

use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;
use magpie_desktop::{Assets, MagpieWindow};
use std::{env, process::ExitCode};

fn main() -> ExitCode {
    let mut arguments = env::args_os().skip(1);
    if let Some(argument) = arguments.next() {
        if arguments.next().is_some() {
            eprintln!("magpie-desktop: expected no arguments, --help, or --version");
            return ExitCode::from(2);
        }
        if argument == "--version" || argument == "-V" {
            println!("magpie-desktop {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        if argument == "--help" || argument == "-h" {
            println!(
                "Magpie {} (early version)\n\
                 Native GPUI agent browser with explicit Codex connection and restore actions.\n\n\
                 Usage: magpie-desktop [--help | --version]\n\n\
                 Without arguments, opens the desktop window.\n\
                 --help, -h     Show this help without opening a display\n\
                 --version, -V  Show the version without opening a display\n\n\
                 The app starts and manages its own local Gateway; external services are read only. Saving Agent settings and restoring them require explicit actions.",
                env!("CARGO_PKG_VERSION"),
            );
            return ExitCode::SUCCESS;
        }
        eprintln!("magpie-desktop: unknown argument; use --help");
        return ExitCode::from(2);
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    if env::var_os("WAYLAND_DISPLAY").is_none() && env::var_os("DISPLAY").is_none() {
        eprintln!(
            "magpie-desktop: no desktop display found; launch from a desktop session or use --help / --version"
        );
        return ExitCode::FAILURE;
    }

    application().with_assets(Assets).run(|cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(900.0), px(560.0)), cx);
        let window = cx.open_window(
            WindowOptions {
                app_id: Some("magpie-rs".to_owned()),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(360.0), px(360.0))),
                ..Default::default()
            },
            |window, cx| {
                window.set_window_title("Magpie");
                cx.new(|cx| {
                    let view = MagpieWindow::new(cx);
                    view.focus(window, cx);
                    view
                })
            },
        );
        if let Err(error) = window {
            eprintln!("magpie-desktop: cannot open window: {error}");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
    ExitCode::SUCCESS
}

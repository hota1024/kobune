//! minato-desktop — Minato の GUI。
//!
//! 常時開くものではなく、「今どの環境が動いているか」を確認して開く
//! 用途を想定している。メニューバーに常駐し、ウィンドウは要求された
//! ときだけ出す。
//!
//! **daemon を起動しない。** daemon の面倒を見るのは launchd の仕事で、
//! GUI が二重に管理すると責務が重なる（`docs/DESIGN.md` §15）。

mod app;
mod bridge;
mod state;
mod tray;

use gpui::prelude::*;
use gpui::{Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::Root;
use tracing_subscriber::EnvFilter;

use crate::app::MinatoApp;
use crate::bridge::Notifier;
use crate::state::SharedState;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("MINATO_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    let application = gpui_platform::application().with_assets(gpui_component_assets::Assets);

    application.run(move |cx| {
        gpui_component::init(cx);

        let state = SharedState::new();
        let (notifier, notifications) = Notifier::channel();
        let commands = bridge::spawn(state.clone(), cwd, notifier);

        // tray は作れなくても GUI は動く。
        let tray = tray::Tray::new();

        let bounds = Bounds::centered(None, size(px(880.0), px(660.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Minato".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let opened = cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| {
                let mut app = MinatoApp::new(state.clone(), commands);
                app.follow_system_appearance(window, cx);
                app
            });
            MinatoApp::listen(&view, notifications, cx);

            if let Some(tray) = tray {
                tray::spawn_poller(tray, state, cx);
            }

            cx.new(|cx| Root::new(view, window, cx))
        });

        if let Err(err) = opened {
            tracing::error!("cannot open the window: {err}");
            cx.quit();
            return;
        }

        cx.activate(true);
    });
}

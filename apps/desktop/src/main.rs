//! minato-desktop — Minato's GUI.
//!
//! Not something to keep open. It is for glancing at which environments
//! are running and opening one. It lives in the menu bar, and the window
//! appears only when asked for.
//!
//! **It never starts the daemon.** Looking after the daemon is launchd's
//! job, and a GUI managing it too would split that responsibility
//! (`docs/DESIGN.md` §15).

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

        // The GUI works fine without a tray.
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

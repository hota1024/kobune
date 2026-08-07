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
mod fonts;
mod state;
mod tray;

use tracing_subscriber::EnvFilter;

use crate::app::MinatoApp;
use crate::state::SharedState;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("MINATO_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Minato")
            .with_inner_size([760.0, 620.0])
            .with_min_inner_size([520.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Minato",
        options,
        Box::new(move |cc| {
            // 日本語が出せないと、ブランチ名やパスが豆腐になる。
            fonts::install(&cc.egui_ctx);

            let state = SharedState::new();
            let commands = bridge::spawn(state.clone(), cwd, cc.egui_ctx.clone());

            // tray は作れなくても GUI は動く。
            let tray = tray::Tray::new();

            Ok(Box::new(MinatoApp::new(state, commands, tray)))
        }),
    )
}

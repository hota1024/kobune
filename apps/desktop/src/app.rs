//! 画面。
//!
//! immediate mode なので、状態を毎フレーム読んで描き直す。凝った作りに
//! せず、**情報密度と更新の速さ**で価値を出す（`docs/DESIGN.md` §12）。

use minato_api::{ServiceInfo, ServiceState, WorkspaceInfo};

use crate::bridge::Command;
use crate::state::{Connection, SharedState};
use crate::tray::Tray;

pub struct MinatoApp {
    state: SharedState,
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    tray: Option<Tray>,
    /// ログを自動で最下部に追従させるか。
    follow_tail: bool,
}

impl MinatoApp {
    pub fn new(
        state: SharedState,
        commands: tokio::sync::mpsc::UnboundedSender<Command>,
        tray: Option<Tray>,
    ) -> Self {
        Self {
            state,
            commands,
            tray,
            follow_tail: true,
        }
    }

    fn send(&self, command: Command) {
        // 送れなくても描画は続ける。橋渡しが落ちているだけ。
        let _ = self.commands.send(command);
    }
}

impl eframe::App for MinatoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_tray(ctx);

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(4.0);
            self.header(ui);
            ui.add_space(4.0);
        });

        egui::TopBottomPanel::bottom("logs")
            .resizable(true)
            .default_height(220.0)
            .show(ctx, |ui| self.log_panel(ui));

        egui::CentralPanel::default().show(ctx, |ui| self.workspaces(ui));
    }
}

impl MinatoApp {
    /// tray からの操作を拾う。描画ループで見るしかない。
    fn handle_tray(&mut self, ctx: &egui::Context) {
        let Some(tray) = &self.tray else { return };

        let entries = self
            .state
            .read(|state| state.menu_entries())
            .unwrap_or_default();
        tray.sync(&entries);

        for action in tray.poll() {
            match action {
                crate::tray::Action::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                crate::tray::Action::Open(url) => open_url(&url),
                crate::tray::Action::Quit => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Minato");
            ui.separator();

            match self
                .state
                .read(|state| state.connection())
                .unwrap_or(Connection::Connecting)
            {
                Connection::Connected(pong) => {
                    ui.colored_label(egui::Color32::from_rgb(0x22, 0xa0, 0x55), "接続中");
                    ui.weak(format!("minatod {} / {}", pong.version, pong.runtime));
                }
                Connection::Connecting => {
                    ui.spinner();
                    ui.weak("接続しています");
                }
                Connection::Failed(reason) => {
                    ui.colored_label(egui::Color32::from_rgb(0xc0, 0x39, 0x2b), "未接続");
                    ui.weak(reason);
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("再読み込み").clicked() {
                    self.send(Command::Refresh);
                }

                // 何個の環境が動いているかが、この GUI の一番の関心事。
                if let Some((running, total)) = self
                    .state
                    .read(|state| (state.running_count(), state.workspaces.len()))
                {
                    ui.weak(format!("{running}/{total} 稼働"));
                }
            });
        });
    }

    fn workspaces(&mut self, ui: &mut egui::Ui) {
        if let Some(error) = self.state.read(|state| state.error.clone()).flatten() {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(egui::Color32::from_rgb(0xc0, 0x39, 0x2b), "⚠");
                ui.label(error);
            });
            ui.separator();
        }

        let workspaces = self
            .state
            .read(|state| state.workspaces.clone())
            .unwrap_or_default();

        if workspaces.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.weak("workspace がありません（`minato new <branch>` で作成）");
            });
            return;
        }

        egui::ScrollArea::vertical()
            .id_salt("workspaces")
            .show(ui, |ui| {
                for workspace in &workspaces {
                    self.workspace_card(ui, workspace);
                    ui.add_space(6.0);
                }
            });
    }

    fn workspace_card(&mut self, ui: &mut egui::Ui, workspace: &WorkspaceInfo) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(workspace.display_name());
                ui.weak(&workspace.branch);

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // main worktree のログも読めた方がよい。
                    let label = workspace
                        .workspace
                        .clone()
                        .unwrap_or_else(|| "main".to_string());

                    if ui.small_button("ログ").clicked() {
                        self.follow_tail = true;
                        self.send(Command::FollowLogs { workspace: label });
                    }
                });
            });

            ui.add_space(4.0);

            for service in &workspace.services {
                self.service_row(ui, service);
            }
        });
    }

    fn service_row(&mut self, ui: &mut egui::Ui, service: &ServiceInfo) {
        ui.horizontal(|ui| {
            let (symbol, color) = state_badge(&service.state);
            ui.colored_label(color, symbol);
            ui.add_sized([90.0, 16.0], egui::Label::new(&service.name).truncate());
            ui.add_sized(
                [70.0, 16.0],
                egui::Label::new(egui::RichText::new(service.state.label()).weak()),
            );

            match service.access() {
                Some(url) => {
                    // クリックで開けることが分かるようリンクにする。
                    if ui.link(&url).clicked() {
                        open_url(&url);
                    }

                    if ui.small_button("コピー").clicked() {
                        ui.ctx().copy_text(url.clone());
                    }
                }
                None => {
                    ui.weak(if service.state.is_running() {
                        "(内部のみ)"
                    } else {
                        "-"
                    });
                }
            }
        });
    }

    fn log_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("ログ");

            let target = self.state.read(|state| state.log_target.clone()).flatten();
            match &target {
                Some(workspace) => ui.weak(workspace),
                None => ui.weak("workspace の「ログ」を押すと表示されます"),
            };

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if target.is_some() && ui.small_button("停止").clicked() {
                    self.send(Command::StopLogs);
                }
                ui.checkbox(&mut self.follow_tail, "追従");

                if let Some(count) = self.state.read(|state| state.log_count()) {
                    if count > 0 {
                        ui.weak(format!("{count} 行"));
                    }
                }
            });
        });

        ui.separator();

        let lines = self
            .state
            .read(|state| {
                state
                    .logs()
                    .map(|line| (line.service.clone(), line.line.clone(), line.is_error))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        egui::ScrollArea::vertical()
            .id_salt("logs")
            .stick_to_bottom(self.follow_tail)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (service, line, is_error) in &lines {
                    ui.horizontal_wrapped(|ui| {
                        ui.add_sized(
                            [70.0, 14.0],
                            egui::Label::new(egui::RichText::new(service).weak().monospace())
                                .truncate(),
                        );

                        let text = egui::RichText::new(line).monospace();
                        if *is_error {
                            ui.label(text.color(egui::Color32::from_rgb(0xc0, 0x39, 0x2b)));
                        } else {
                            ui.label(text);
                        }
                    });
                }
            });
    }
}

/// 状態に対応する印と色。
fn state_badge(state: &ServiceState) -> (&'static str, egui::Color32) {
    match state {
        ServiceState::Ready => ("●", egui::Color32::from_rgb(0x22, 0xa0, 0x55)),
        ServiceState::Starting => ("◐", egui::Color32::from_rgb(0xd2, 0x8b, 0x1e)),
        ServiceState::Idle => ("○", egui::Color32::from_rgb(0x7a, 0x7a, 0x7a)),
        ServiceState::Failed { .. } => ("✗", egui::Color32::from_rgb(0xc0, 0x39, 0x2b)),
        ServiceState::Stopped | ServiceState::Unknown => {
            ("○", egui::Color32::from_rgb(0x9a, 0x9a, 0x9a))
        }
    }
}

/// ブラウザで開く。
fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let program = "start";

    if let Err(err) = std::process::Command::new(program).arg(url).spawn() {
        tracing::warn!("{url} を開けません: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_has_a_distinct_colour() {
        // 状態が色で見分けられないと、一覧を眺める意味が薄れる。
        let ready = state_badge(&ServiceState::Ready);
        let stopped = state_badge(&ServiceState::Stopped);
        let failed = state_badge(&ServiceState::failed("boom"));

        assert_ne!(ready.1, stopped.1);
        assert_ne!(ready.1, failed.1);
        assert_ne!(stopped.1, failed.1);
    }

    #[test]
    fn running_states_look_alive() {
        // 起動中と停止中が同じ印だと区別がつかない。
        assert_ne!(
            state_badge(&ServiceState::Ready).0,
            state_badge(&ServiceState::Stopped).0
        );
    }
}

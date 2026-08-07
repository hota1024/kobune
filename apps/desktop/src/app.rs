//! 画面。GPUI + gpui-component。
//!
//! 左に workspace の一覧、右に選んだものの詳細とログ。worktree が
//! 増えても崩れず、「今どれを見ているか」がはっきりする形にしている。
//!
//! 状態は [`SharedState`] にあり、tokio 側が書く。ここは読んで描くだけ。
//! 変更の通知を受けたら `cx.notify()` して描き直す。

use gpui::prelude::*;
use gpui::{AnyElement, App, ClipboardItem, Entity, Hsla, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::label::Label;
use gpui_component::theme::{ActiveTheme, Theme, ThemeMode};
use gpui_component::{Disableable, Icon, IconName, Sizable, StyledExt, h_flex, v_flex};
use minato_api::{ServiceInfo, ServiceState, WorkspaceInfo};

use crate::bridge::Command;
use crate::state::{Connection, SharedState};

/// サイドバーの幅。workspace 名が読める最小限。
const SIDEBAR_WIDTH: f32 = 232.0;

/// ログ欄の高さ。
const LOG_HEIGHT: f32 = 220.0;

pub struct MinatoApp {
    state: SharedState,
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    /// 詳細に出している workspace。
    selected: Option<String>,
    /// 外観の追従を解除するための購読。手放すと止まる。
    _appearance: Option<gpui::Subscription>,
}

impl MinatoApp {
    pub fn new(state: SharedState, commands: tokio::sync::mpsc::UnboundedSender<Command>) -> Self {
        Self {
            state,
            commands,
            selected: None,
            _appearance: None,
        }
    }

    /// システムの外観に追従させる。
    ///
    /// 起動時に合わせるだけでは足りない。macOS は時刻でライト／ダークが
    /// 切り替わるので、動いている間の変化も拾う。
    pub fn follow_system_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        Theme::sync_system_appearance(Some(window), cx);

        self._appearance = Some(window.observe_window_appearance(|window, cx| {
            Theme::sync_system_appearance(Some(window), cx);
        }));
    }

    /// tokio 側からの通知を受けて描き直す。
    pub fn listen(
        entity: &Entity<Self>,
        mut notifications: tokio::sync::mpsc::UnboundedReceiver<()>,
        cx: &mut App,
    ) {
        let entity = entity.downgrade();

        cx.spawn(async move |cx| {
            while notifications.recv().await.is_some() {
                // 参照が切れていれば購読を終える。
                if entity.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    fn send(&self, command: Command) {
        // 送れなくても描画は続ける。橋渡しが落ちているだけ。
        let _ = self.commands.send(command);
    }

    /// workspace を選び、そのログを追い始める。
    ///
    /// 選択とログを別操作にすると、見ている環境と流れているログが
    /// ずれる。それは原因の取り違えに直結する。
    fn select(&mut self, label: String) {
        self.selected = Some(label.clone());
        self.send(Command::FollowLogs { workspace: label });
    }

    /// 今選ばれている workspace。未選択なら既定を選ぶ。
    fn current(&mut self) -> Option<String> {
        if let Some(selected) = &self.selected {
            // 消えた workspace を掴んだままにしない。
            let exists = self
                .state
                .read(|state| state.workspace(selected).is_some())
                .unwrap_or(false);

            if exists {
                return Some(selected.clone());
            }
        }

        let fallback = self.state.read(|state| state.default_selection()).flatten();
        self.selected = fallback.clone();
        fallback
    }
}

impl Render for MinatoApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.current();

        h_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.sidebar(selected.as_deref(), cx))
            .child(self.detail(selected.as_deref(), cx))
    }
}

impl MinatoApp {
    fn sidebar(&self, selected: Option<&str>, cx: &mut Context<Self>) -> AnyElement {
        let workspaces = self
            .state
            .read(|state| state.workspaces.clone())
            .unwrap_or_default();

        let mut rows = Vec::with_capacity(workspaces.len());
        for workspace in &workspaces {
            rows.push(self.sidebar_row(workspace, selected, cx));
        }

        v_flex()
            .w(px(SIDEBAR_WIDTH))
            .flex_shrink_0()
            .h_full()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().border)
            .child(self.brand(cx))
            .child(
                v_flex()
                    .id("workspace-list")
                    .flex_1()
                    .min_h(px(0.0))
                    .p_2()
                    .gap_0p5()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .into_any_element()
    }

    /// サイドバーの頭。接続状態と稼働数だけを出す。
    fn brand(&self, cx: &mut Context<Self>) -> AnyElement {
        let connection = self
            .state
            .read(|state| state.connection())
            .unwrap_or(Connection::Connecting);

        let (running, total) = self
            .state
            .read(|state| (state.running_count(), state.workspaces.len()))
            .unwrap_or((0, 0));

        let is_dark = cx.theme().mode.is_dark();

        let (color, note) = match &connection {
            Connection::Connected(pong) => {
                (cx.theme().success, format!("minatod {}", pong.version))
            }
            Connection::Connecting => (cx.theme().muted_foreground, "Connecting…".to_string()),
            Connection::Failed(_) => (cx.theme().danger, "Disconnected".to_string()),
        };

        v_flex()
            .w_full()
            .px_3()
            .py_3()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(div().text_lg().font_semibold().child("Minato"))
                    .child(div().flex_1())
                    .child(
                        Button::new("theme")
                            .ghost()
                            .xsmall()
                            .icon(if is_dark {
                                IconName::Sun
                            } else {
                                IconName::Moon
                            })
                            .on_click(|_, window, cx| {
                                // 手で切り替えたら、その選択を尊重して
                                // システム追従は上書きしない。
                                let next = if cx.theme().mode.is_dark() {
                                    ThemeMode::Light
                                } else {
                                    ThemeMode::Dark
                                };
                                Theme::change(next, Some(window), cx);
                            }),
                    )
                    .child(
                        Button::new("refresh")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Replace)
                            .on_click(cx.listener(|this, _, _window, _cx| {
                                this.send(Command::Refresh);
                            })),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(dot(color))
                    .child(
                        Label::new(SharedString::from(note))
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(div().flex_1())
                    .child(
                        Label::new(SharedString::from(format!("{running}/{total}")))
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            .into_any_element()
    }

    fn sidebar_row(
        &self,
        workspace: &WorkspaceInfo,
        selected: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = workspace
            .workspace
            .clone()
            .unwrap_or_else(|| "main".to_string());
        let is_selected = selected == Some(label.as_str());

        let running = workspace
            .services
            .iter()
            .filter(|service| service.state.is_running())
            .count();
        let total = workspace.services.len();

        let color = if running > 0 {
            cx.theme().success
        } else {
            cx.theme().muted_foreground
        };

        // accent だけだと明るい地で選択が分かりにくい。選択には
        // 左端の帯を足して、色に頼らず位置でも分かるようにする。
        let accent = cx.theme().accent;
        let primary = cx.theme().primary;
        let for_click = label.clone();

        h_flex()
            .id(SharedString::from(format!("ws-{label}")))
            .w_full()
            .px_2()
            .py_1p5()
            .gap_2()
            .items_center()
            .rounded(cx.theme().radius)
            .when(is_selected, |this| {
                this.bg(accent).border_l_2().border_color(primary)
            })
            .when(!is_selected, |this| {
                // 選択時と幅を揃え、切り替えで行がずれないようにする。
                this.border_l_2()
                    .border_color(gpui::transparent_black())
                    .hover(move |this| this.bg(accent.opacity(0.6)))
            })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _window, cx| {
                this.select(for_click.clone());
                cx.notify();
            }))
            .child(dot(color))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .child(SharedString::from(workspace.display_name().to_string())),
            )
            .child(
                Label::new(SharedString::from(format!("{running}/{total}")))
                    .text_color(cx.theme().muted_foreground),
            )
            .into_any_element()
    }

    fn detail(&self, selected: Option<&str>, cx: &mut Context<Self>) -> AnyElement {
        let workspace = selected.and_then(|label| {
            self.state
                .read(|state| state.workspace(label).cloned())
                .flatten()
        });

        // 接続の失敗と一覧の失敗は原因が別。両方出す。
        let error = self
            .state
            .read(|state| match state.connection() {
                Connection::Failed(reason) => Some(reason),
                _ => state.error.clone(),
            })
            .flatten();

        let body = match workspace {
            Some(workspace) => self.workspace_detail(&workspace, cx),
            None => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .child(Label::new("No workspaces").text_color(cx.theme().muted_foreground))
                .child(
                    Label::new("Create one with `minato new <branch>`")
                        .text_color(cx.theme().muted_foreground),
                )
                .into_any_element(),
        };

        v_flex()
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .when_some(error, |this, error| {
                this.child(
                    h_flex()
                        .w_full()
                        .px_4()
                        .py_2()
                        .gap_2()
                        .items_center()
                        .bg(cx.theme().danger.opacity(0.12))
                        .child(
                            Icon::new(IconName::TriangleAlert)
                                .small()
                                .text_color(cx.theme().danger),
                        )
                        .child(Label::new(SharedString::from(error))),
                )
            })
            .child(v_flex().flex_1().min_h(px(0.0)).child(body))
            .child(self.log_panel(cx))
            .into_any_element()
    }

    fn workspace_detail(&self, workspace: &WorkspaceInfo, cx: &mut Context<Self>) -> AnyElement {
        let label = workspace
            .workspace
            .clone()
            .unwrap_or_else(|| "main".to_string());

        let busy = self
            .state
            .read(|state| state.busy.contains(&label))
            .unwrap_or(false);

        let running = workspace
            .services
            .iter()
            .any(|service| service.state.is_running());

        let mut rows = Vec::with_capacity(workspace.services.len());
        for service in &workspace.services {
            rows.push(self.service_row(workspace, service, cx));
        }

        let for_up = label.clone();
        let for_down = label;

        v_flex()
            .id("detail")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .w_full()
                    .px_4()
                    .py_3()
                    .gap_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .child(
                                div().text_lg().font_semibold().child(SharedString::from(
                                    workspace.display_name().to_string(),
                                )),
                            )
                            .child(div().flex_1())
                            // 処理中は両方とも押せなくする。連打で起動と
                            // 停止が交錯すると、今どちらなのか読めなくなる。
                            .child(
                                Button::new("up")
                                    .primary()
                                    .small()
                                    .icon(IconName::Play)
                                    .label("Start")
                                    .loading(busy)
                                    .disabled(busy || running)
                                    .on_click(cx.listener(move |this, _, _window, _cx| {
                                        this.send(Command::Up {
                                            workspace: for_up.clone(),
                                        });
                                    })),
                            )
                            .child(
                                Button::new("down")
                                    .outline()
                                    .small()
                                    .icon(IconName::Pause)
                                    .label("Stop")
                                    .loading(busy)
                                    .disabled(busy || !running)
                                    .on_click(cx.listener(move |this, _, _window, _cx| {
                                        this.send(Command::Down {
                                            workspace: for_down.clone(),
                                        });
                                    })),
                            ),
                    )
                    .child(
                        Label::new(SharedString::from(workspace.branch.clone()))
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        Label::new(SharedString::from(workspace.path.display().to_string()))
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            .child(v_flex().w_full().p_4().gap_2().children(rows))
            .into_any_element()
    }

    fn service_row(
        &self,
        workspace: &WorkspaceInfo,
        service: &ServiceInfo,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let access = service.access();
        let id = format!("{}-{}", workspace.display_name(), service.name);

        v_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_1()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(dot(state_color(&service.state, cx)))
                    .child(
                        div()
                            .font_semibold()
                            .child(SharedString::from(service.name.clone())),
                    )
                    .child(state_badge(&service.state, cx))
                    .child(div().flex_1())
                    .when_some(service.port, |this, port| {
                        this.child(
                            Label::new(SharedString::from(format!(":{port}")))
                                .text_color(cx.theme().muted_foreground),
                        )
                    }),
            )
            .child(match access {
                Some(url) => {
                    let to_open = url.clone();
                    let to_copy = url.clone();

                    h_flex()
                        .w_full()
                        .gap_1()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .truncate()
                                .text_sm()
                                .font_family("monospace")
                                .text_color(cx.theme().link)
                                .child(SharedString::from(url)),
                        )
                        .child(
                            Button::new(SharedString::from(format!("copy-{id}")))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Copy)
                                .on_click(move |_, _window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        to_copy.clone(),
                                    ));
                                }),
                        )
                        .child(
                            Button::new(SharedString::from(format!("open-{id}")))
                                .ghost()
                                .xsmall()
                                .icon(IconName::ExternalLink)
                                .on_click(move |_, _window, _cx| open_url(&to_open)),
                        )
                        .into_any_element()
                }
                None => Label::new(if service.state.is_running() {
                    "Internal only"
                } else {
                    "Stopped — starts on first request"
                })
                .text_color(cx.theme().muted_foreground)
                .into_any_element(),
            })
            .into_any_element()
    }

    fn log_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let target = self.state.read(|state| state.log_target.clone()).flatten();
        let count = self.state.read(|state| state.log_count()).unwrap_or(0);
        let lines = self
            .state
            .read(|state| {
                state
                    .logs()
                    .map(|line| (line.service.clone(), line.line.clone(), line.is_error))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        v_flex()
            .w_full()
            .h(px(LOG_HEIGHT))
            .flex_shrink_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .child(
                h_flex()
                    .w_full()
                    .px_4()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .child(div().font_semibold().child("Logs"))
                    .child(
                        Label::new(SharedString::from(target.clone().unwrap_or_else(|| {
                            "Select a workspace to stream logs".to_string()
                        })))
                        .text_color(cx.theme().muted_foreground),
                    )
                    .child(div().flex_1())
                    .when(count > 0, |this| {
                        this.child(
                            Label::new(SharedString::from(format!("{count} lines")))
                                .text_color(cx.theme().muted_foreground),
                        )
                    })
                    .when_some(target, |this, _| {
                        this.child(
                            Button::new("stop-logs")
                                .ghost()
                                .xsmall()
                                .label("Stop")
                                .on_click(cx.listener(|this, _, _window, _cx| {
                                    this.send(Command::StopLogs);
                                })),
                        )
                    }),
            )
            .child(
                v_flex()
                    .id("log-lines")
                    .flex_1()
                    .min_h(px(0.0))
                    .px_4()
                    .pb_2()
                    .gap_0p5()
                    .overflow_y_scroll()
                    .children(lines.into_iter().map(|(service, line, is_error)| {
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_start()
                            .text_xs()
                            .font_family("monospace")
                            .child(
                                div()
                                    .w(px(64.0))
                                    .flex_shrink_0()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(service)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .when(is_error, |this| this.text_color(cx.theme().danger))
                                    .child(SharedString::from(line)),
                            )
                    })),
            )
            .into_any_element()
    }
}

/// 状態を示す丸。
///
/// 小さすぎると背景に埋もれる。色の違いだけに頼らず、外側に薄い輪を
/// 付けて明暗どちらの地でも輪郭が出るようにする。
fn dot(color: Hsla) -> impl IntoElement {
    div()
        .size(px(10.0))
        .rounded_full()
        .flex_shrink_0()
        .bg(color)
        .border_2()
        .border_color(color.opacity(0.25))
}

/// 状態を示す札。
///
/// 色だけで区別させない。文字を添えることで、色覚特性や暗い画面でも
/// 読み取れるようにする。
fn state_badge(state: &ServiceState, cx: &App) -> AnyElement {
    let color = state_color(state, cx);

    div()
        .px_1p5()
        .py_0p5()
        .rounded(cx.theme().radius)
        .bg(color.opacity(0.15))
        .text_xs()
        .text_color(color)
        .child(SharedString::from(state.label()))
        .into_any_element()
}

fn state_color(state: &ServiceState, cx: &App) -> Hsla {
    match state {
        ServiceState::Ready => cx.theme().success,
        ServiceState::Starting => cx.theme().warning,
        ServiceState::Failed { .. } => cx.theme().danger,
        ServiceState::Idle | ServiceState::Stopped | ServiceState::Unknown => {
            cx.theme().muted_foreground
        }
    }
}

/// ブラウザで開く。
pub fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let program = "start";

    if let Err(err) = std::process::Command::new(program).arg(url).spawn() {
        tracing::warn!("cannot open {url}: {err}");
    }
}

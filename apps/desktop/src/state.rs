//! 描画スレッドが読む状態。
//!
//! egui の描画ループは同期で、`async` を直接扱えない。tokio 側が書き、
//! 描画側が読む。**ロックは短く持つ**（描画のたびに取るため）。

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

use minato_api::{Pong, WorkspaceInfo};

/// ログビューアが保持する行数の上限。
///
/// 無制限にすると流し続けたときにメモリを食い潰す。開発中に遡りたい
/// 範囲としてはこれで足りる。
const MAX_LOG_LINES: usize = 2000;

/// daemon との接続状態。
#[derive(Debug, Clone)]
pub enum Connection {
    /// まだ繋いでいない。
    Connecting,
    Connected(Box<Pong>),
    /// 繋がらない。理由を添える。
    Failed(String),
}

/// ログ 1 行。
#[derive(Debug, Clone)]
pub struct LogLine {
    pub service: String,
    pub line: String,
    pub is_error: bool,
}

#[derive(Debug, Default)]
pub struct AppState {
    pub connection: Option<Connection>,
    pub workspaces: Vec<WorkspaceInfo>,
    /// 一覧の取得に失敗したときの理由。
    pub error: Option<String>,
    /// ログビューアが選んでいる workspace。
    pub log_target: Option<String>,
    logs: VecDeque<LogLine>,
}

impl AppState {
    pub fn connection(&self) -> Connection {
        self.connection.clone().unwrap_or(Connection::Connecting)
    }

    pub fn push_log(&mut self, line: LogLine) {
        if self.logs.len() >= MAX_LOG_LINES {
            self.logs.pop_front();
        }
        self.logs.push_back(line);
    }

    pub fn logs(&self) -> impl Iterator<Item = &LogLine> {
        self.logs.iter()
    }

    pub fn log_count(&self) -> usize {
        self.logs.len()
    }

    pub fn clear_logs(&mut self) {
        self.logs.clear();
    }

    /// 起動しているサービスを持つ workspace の数。
    pub fn running_count(&self) -> usize {
        self.workspaces
            .iter()
            .filter(|workspace| {
                workspace
                    .services
                    .iter()
                    .any(|service| service.state.is_running())
            })
            .count()
    }

    /// tray のメニューに出す (表示名, URL) の一覧。
    pub fn menu_entries(&self) -> Vec<(String, String)> {
        let mut entries = Vec::new();

        for workspace in &self.workspaces {
            for service in &workspace.services {
                let Some(url) = service.access() else {
                    continue;
                };

                entries.push((
                    format!("{} / {}", workspace.display_name(), service.name),
                    url,
                ));
            }
        }

        entries
    }
}

/// 描画スレッドと tokio スレッドで共有する。
#[derive(Clone, Default)]
pub struct SharedState(Arc<RwLock<AppState>>);

impl SharedState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 読む。ロックが壊れていても描画は続ける。
    pub fn read<T>(&self, f: impl FnOnce(&AppState) -> T) -> Option<T> {
        self.0.read().ok().map(|state| f(&state))
    }

    pub fn write(&self, f: impl FnOnce(&mut AppState)) {
        if let Ok(mut state) = self.0.write() {
            f(&mut state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minato_api::{ServiceInfo, ServiceScope, ServiceState};
    use std::path::PathBuf;

    fn service(name: &str, state: ServiceState, url: Option<&str>) -> ServiceInfo {
        ServiceInfo {
            name: name.into(),
            state,
            scope: ServiceScope::Workspace,
            url: url.map(str::to_string),
            tunnel_url: None,
            endpoint: None,
            port: Some(3000),
            container_id: None,
            image: None,
        }
    }

    fn workspace(label: Option<&str>, services: Vec<ServiceInfo>) -> WorkspaceInfo {
        WorkspaceInfo {
            project: "myapp".into(),
            workspace: label.map(str::to_string),
            branch: "feature/one".into(),
            path: PathBuf::from("/repo"),
            is_main: label.is_none(),
            services,
        }
    }

    #[test]
    fn log_buffer_is_bounded() {
        // 流し続けてもメモリを食い潰さない。
        let mut state = AppState::default();

        for n in 0..(MAX_LOG_LINES + 500) {
            state.push_log(LogLine {
                service: "web".into(),
                line: format!("line {n}"),
                is_error: false,
            });
        }

        assert_eq!(state.log_count(), MAX_LOG_LINES);

        // 古い行から捨てる。直近が残っていないと意味がない。
        let last = state.logs().last().expect("ある");
        assert_eq!(last.line, format!("line {}", MAX_LOG_LINES + 499));
    }

    #[test]
    fn counts_workspaces_with_running_services() {
        let state = AppState {
            workspaces: vec![
                workspace(
                    Some("feat-1"),
                    vec![service("web", ServiceState::Ready, None)],
                ),
                workspace(
                    Some("feat-2"),
                    vec![service("web", ServiceState::Stopped, None)],
                ),
            ],
            ..Default::default()
        };

        assert_eq!(state.running_count(), 1);
    }

    #[test]
    fn menu_lists_only_reachable_services() {
        let state = AppState {
            workspaces: vec![workspace(
                Some("feat-1"),
                vec![
                    service(
                        "web",
                        ServiceState::Ready,
                        Some("https://web.feat-1.myapp.localhost"),
                    ),
                    // URL が無いものは開きようがない。
                    service("db", ServiceState::Ready, None),
                ],
            )],
            ..Default::default()
        };

        let entries = state.menu_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "feat-1 / web");
        assert_eq!(entries[0].1, "https://web.feat-1.myapp.localhost");
    }

    #[test]
    fn main_workspace_is_labelled() {
        let state = AppState {
            workspaces: vec![workspace(
                None,
                vec![service(
                    "web",
                    ServiceState::Ready,
                    Some("https://web.myapp.localhost"),
                )],
            )],
            ..Default::default()
        };

        assert_eq!(state.menu_entries()[0].0, "(main) / web");
    }

    #[test]
    fn shared_state_survives_concurrent_access() {
        let shared = SharedState::new();

        shared.write(|state| {
            state.push_log(LogLine {
                service: "web".into(),
                line: "hello".into(),
                is_error: false,
            });
        });

        let count = shared.read(|state| state.log_count()).expect("読める");
        assert_eq!(count, 1);
    }

    #[test]
    fn connection_defaults_to_connecting() {
        // 接続前に「未接続」と赤字で出すと、起動直後に毎回警告が出る。
        let state = AppState::default();
        assert!(matches!(state.connection(), Connection::Connecting));
    }
}

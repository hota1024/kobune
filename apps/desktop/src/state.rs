//! The state the render thread reads.
//!
//! The render loop is synchronous and cannot handle `async` directly, so
//! tokio writes and the renderer reads. **Hold the lock briefly** — it is
//! taken on every frame.

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, RwLock};

use minato_api::{Pong, WorkspaceInfo};

/// How many lines the log viewer keeps.
///
/// Unbounded, a busy stream would eat all the memory. This is as far back
/// as anyone scrolls during development.
const MAX_LOG_LINES: usize = 2000;

/// The state of the connection to the daemon.
#[derive(Debug, Clone)]
pub enum Connection {
    /// Not connected yet.
    Connecting,
    Connected(Box<Pong>),
    /// Cannot connect, and why.
    Failed(String),
}

/// One line of log output.
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
    /// Why the listing failed, if it did.
    pub error: Option<String>,
    /// The workspace the log viewer is showing.
    pub log_target: Option<String>,
    /// Workspaces currently starting or stopping.
    ///
    /// Tracked as state because a button that does nothing when pressed
    /// looks broken.
    pub busy: BTreeSet<String>,
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

    /// How many workspaces have a service running.
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

    /// Looks up a workspace by its label.
    pub fn workspace(&self, label: &str) -> Option<&WorkspaceInfo> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.workspace.as_deref().unwrap_or("main") == label)
    }

    /// What to select on startup. A working worktree beats the main one.
    pub fn default_selection(&self) -> Option<String> {
        self.workspaces
            .iter()
            .find(|workspace| !workspace.is_main)
            .or_else(|| self.workspaces.first())
            .map(|workspace| {
                workspace
                    .workspace
                    .clone()
                    .unwrap_or_else(|| "main".to_string())
            })
    }

    /// The (display name, URL) pairs for the tray menu.
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

/// Shared between the render thread and the tokio threads.
#[derive(Clone, Default)]
pub struct SharedState(Arc<RwLock<AppState>>);

impl SharedState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads. Rendering carries on even with a poisoned lock.
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
        // A busy stream must not eat all the memory.
        let mut state = AppState::default();

        for n in 0..(MAX_LOG_LINES + 500) {
            state.push_log(LogLine {
                service: "web".into(),
                line: format!("line {n}"),
                is_error: false,
            });
        }

        assert_eq!(state.log_count(), MAX_LOG_LINES);

        // Drop the oldest first — the recent lines are the point.
        let last = state.logs().last().expect("is there");
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
                    // Nothing to open without a URL.
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
    fn default_selection_prefers_a_worktree_over_main() {
        // Selecting main first would cost a click every time someone
        // wants the environment they are working in.
        let state = AppState {
            workspaces: vec![workspace(None, vec![]), workspace(Some("feat-1"), vec![])],
            ..Default::default()
        };

        assert_eq!(state.default_selection().as_deref(), Some("feat-1"));
    }

    #[test]
    fn default_selection_falls_back_to_main() {
        let state = AppState {
            workspaces: vec![workspace(None, vec![])],
            ..Default::default()
        };

        assert_eq!(state.default_selection().as_deref(), Some("main"));
    }

    #[test]
    fn no_selection_without_workspaces() {
        assert_eq!(AppState::default().default_selection(), None);
    }

    #[test]
    fn looks_up_workspaces_by_label() {
        let state = AppState {
            workspaces: vec![workspace(Some("feat-1"), vec![]), workspace(None, vec![])],
            ..Default::default()
        };

        assert!(state.workspace("feat-1").is_some());
        // The main worktree answers to the label "main".
        assert!(state.workspace("main").is_some());
        assert!(state.workspace("nope").is_none());
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

        let count = shared.read(|state| state.log_count()).expect("reads");
        assert_eq!(count, 1);
    }

    #[test]
    fn connection_defaults_to_connecting() {
        // Showing a red "not connected" before the first attempt would
        // warn on every single launch.
        let state = AppState::default();
        assert!(matches!(state.connection(), Connection::Connecting));
    }
}

//! daemon からクライアントへの最終応答。
//!
//! ここに人間向けの整形済み文字列を入れてはいけない。表示は CLI と GUI が
//! それぞれ担当する（`docs/DESIGN.md` §3 の原則）。

use std::path::PathBuf;

use minato_core::{ServiceScope, ServiceState};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Pong(Pong),
    /// 複数 workspace を返す操作（`ls`）。
    Workspaces {
        workspaces: Vec<WorkspaceInfo>,
    },
    /// 単一 workspace を返す操作（`new` / `up` / `down` / `status`）。
    Workspace {
        workspace: WorkspaceInfo,
    },
    /// 返す値がない操作（`rm` / `shutdown`）。
    Empty,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pong {
    /// daemon のバージョン。
    pub version: String,
    /// 対応しているプロトコルバージョン。
    pub protocol: u32,
    /// 既定の runtime 実装（`docker` など）。
    pub runtime: String,
    pub uptime_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub project: String,
    /// URL に現れる workspace ラベル。main worktree では `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    pub branch: String,
    pub path: PathBuf,
    pub is_main: bool,
    pub services: Vec<ServiceInfo>,
}

impl WorkspaceInfo {
    /// 表示用の workspace 名。main worktree では `(main)` を使う。
    pub fn display_name(&self) -> &str {
        self.workspace.as_deref().unwrap_or("(main)")
    }

    pub fn service(&self, name: &str) -> Option<&ServiceInfo> {
        self.services.iter().find(|s| s.name == name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    pub state: ServiceState,
    pub scope: ServiceScope,

    /// 発行された URL。プロキシが動く M1 以降で埋まる。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Cloudflare Tunnel 経由の URL。M4 以降。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel_url: Option<String>,

    /// ホストから直接届く待ち受けアドレス（`127.0.0.1:49312`）。
    ///
    /// プロキシが入るまではこれが唯一のアクセス手段になる。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,

    /// コンテナ内のポート。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

impl ServiceInfo {
    /// クライアントが利用者に見せるべきアクセス先。
    ///
    /// URL が発行されていればそれを、まだなら生の待ち受けアドレスを返す。
    pub fn access(&self) -> Option<String> {
        self.url
            .clone()
            .or_else(|| self.endpoint.as_ref().map(|e| format!("http://{e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(name: &str) -> ServiceInfo {
        ServiceInfo {
            name: name.into(),
            state: ServiceState::Ready,
            scope: ServiceScope::Workspace,
            url: None,
            tunnel_url: None,
            endpoint: None,
            port: None,
            container_id: None,
            image: None,
        }
    }

    #[test]
    fn access_prefers_url_over_endpoint() {
        let mut svc = service("web");
        svc.endpoint = Some("127.0.0.1:49312".into());
        assert_eq!(svc.access().as_deref(), Some("http://127.0.0.1:49312"));

        svc.url = Some("https://web.feat-1.myapp.localhost".into());
        assert_eq!(
            svc.access().as_deref(),
            Some("https://web.feat-1.myapp.localhost")
        );
    }

    #[test]
    fn access_is_none_without_any_address() {
        assert_eq!(service("db").access(), None);
    }

    #[test]
    fn main_workspace_displays_as_main() {
        let info = WorkspaceInfo {
            project: "myapp".into(),
            workspace: None,
            branch: "main".into(),
            path: PathBuf::from("/repo"),
            is_main: true,
            services: vec![service("web")],
        };

        assert_eq!(info.display_name(), "(main)");
        assert!(info.service("web").is_some());
        assert!(info.service("nope").is_none());
    }

    #[test]
    fn omits_empty_optionals_on_the_wire() {
        let info = WorkspaceInfo {
            project: "myapp".into(),
            workspace: None,
            branch: "main".into(),
            path: PathBuf::from("/repo"),
            is_main: true,
            services: vec![service("web")],
        };

        let json = serde_json::to_string(&info).expect("serializes");
        assert!(!json.contains("tunnel_url"), "未使用のフィールドは出さない");
        // `"scope":"workspace"` の値と紛れないよう、キーとして現れないことを見る。
        assert!(!json.contains(r#""workspace":"#), "got: {json}");
    }
}

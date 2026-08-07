//! ホスト名から転送先を引くテーブル。
//!
//! daemon（Supervisor）が書き、プロキシが読む。プロキシは runtime の
//! 実装を知らず、[`Route::endpoint`] に転送するだけでよい。Docker では
//! ホストのフォワードポート、Apple Container ではコンテナ自身の IP が入る。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

/// 1 つのホスト名に対応する転送先。
///
/// **停止中のサービスも登録する。** scale-to-zero では「止まっている」ことと
/// 「存在しない」ことを区別する必要がある。前者はリクエストで起こし、
/// 後者は 404 を返す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// 転送先。停止中は `None`。
    pub endpoint: Option<SocketAddr>,
    /// 診断とログのための識別子。
    pub project: String,
    pub workspace: String,
    pub service: String,
}

impl Route {
    /// 起動中のサービス。
    pub fn new(
        endpoint: SocketAddr,
        project: impl Into<String>,
        workspace: impl Into<String>,
        service: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: Some(endpoint),
            project: project.into(),
            workspace: workspace.into(),
            service: service.into(),
        }
    }

    /// 停止中のサービス。リクエストが来たら起動する。
    pub fn stopped(
        project: impl Into<String>,
        workspace: impl Into<String>,
        service: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: None,
            project: project.into(),
            workspace: workspace.into(),
            service: service.into(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.endpoint.is_some()
    }
}

/// スレッド間で共有するルーティングテーブル。
///
/// 読みが圧倒的に多く（リクエストごと）、書きは稀（サービスの起動停止）
/// なので `RwLock` で足りる。
#[derive(Clone, Default)]
pub struct Routes {
    inner: Arc<RwLock<HashMap<String, Route>>>,
}

impl Routes {
    pub fn new() -> Self {
        Self::default()
    }

    /// ホスト名から転送先を引く。`host` は正規化していなくてよい。
    pub fn get(&self, host: &str) -> Option<Route> {
        let key = normalize_host(host)?;
        self.inner
            .read()
            .expect("ルーティングテーブルのロックが壊れている")
            .get(&key)
            .cloned()
    }

    pub fn insert(&self, host: &str, route: Route) {
        let Some(key) = normalize_host(host) else {
            tracing::warn!("ホスト名として解釈できないため登録しません: {host}");
            return;
        };

        self.inner
            .write()
            .expect("ルーティングテーブルのロックが壊れている")
            .insert(key, route);
    }

    pub fn remove(&self, host: &str) {
        let Some(key) = normalize_host(host) else {
            return;
        };

        self.inner
            .write()
            .expect("ルーティングテーブルのロックが壊れている")
            .remove(&key);
    }

    /// あるプロジェクトのルートをまとめて差し替える。
    ///
    /// 個々の増減を追うより、状態を取り直して丸ごと入れ替える方が
    /// 取りこぼしがない。runtime のラベルを状態の正とする方針と揃う。
    pub fn replace_project(&self, project: &str, entries: Vec<(String, Route)>) {
        let mut guard = self
            .inner
            .write()
            .expect("ルーティングテーブルのロックが壊れている");

        guard.retain(|_, route| route.project != project);

        for (host, route) in entries {
            if let Some(key) = normalize_host(&host) {
                guard.insert(key, route);
            }
        }
    }

    /// 登録されているホスト名と転送先の一覧。診断用。
    pub fn snapshot(&self) -> Vec<(String, Route)> {
        let guard = self
            .inner
            .read()
            .expect("ルーティングテーブルのロックが壊れている");

        let mut entries: Vec<(String, Route)> = guard
            .iter()
            .map(|(host, route)| (host.clone(), route.clone()))
            .collect();

        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    pub fn len(&self) -> usize {
        self.inner
            .read()
            .expect("ルーティングテーブルのロックが壊れている")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// `Host` ヘッダや SNI をテーブルのキーに揃える。
///
/// ブラウザは `web.feat-1.myapp.localhost:8080` のようにポート付きで送り、
/// DNS 由来の名前は末尾にドットが付くことがある。大文字小文字も区別しない。
pub fn normalize_host(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // IPv6 リテラル（`[::1]:8080`）はブラケットの中を取り出す。
    let without_port = if let Some(rest) = trimmed.strip_prefix('[') {
        let end = rest.find(']')?;
        &rest[..end]
    } else {
        match trimmed.split_once(':') {
            Some((host, _port)) => host,
            None => trimmed,
        }
    };

    // 完全修飾名の末尾のドットを落とす。
    let without_dot = without_port.trim_end_matches('.');
    if without_dot.is_empty() {
        return None;
    }

    Some(without_dot.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(port: u16) -> Route {
        Route::new(
            SocketAddr::from(([127, 0, 0, 1], port)),
            "myapp",
            "feat-1",
            "web",
        )
    }

    #[test]
    fn strips_port_from_host_header() {
        // ブラウザは非標準ポートだと Host にポートを付けて送る。
        assert_eq!(
            normalize_host("web.feat-1.myapp.localhost:8443").as_deref(),
            Some("web.feat-1.myapp.localhost")
        );
    }

    #[test]
    fn lowercases_and_strips_trailing_dot() {
        assert_eq!(
            normalize_host("WEB.Feat-1.MyApp.localhost.").as_deref(),
            Some("web.feat-1.myapp.localhost")
        );
    }

    #[test]
    fn handles_ipv6_literals() {
        assert_eq!(normalize_host("[::1]:8080").as_deref(), Some("::1"));
        assert_eq!(normalize_host("[::1]").as_deref(), Some("::1"));
    }

    #[test]
    fn rejects_empty_hosts() {
        assert_eq!(normalize_host(""), None);
        assert_eq!(normalize_host("   "), None);
        assert_eq!(normalize_host("."), None);
        assert_eq!(normalize_host(":8080"), None);
    }

    #[test]
    fn lookup_normalizes_the_query() {
        let routes = Routes::new();
        routes.insert("web.feat-1.myapp.localhost", route(3000));

        // 登録も参照も正規化されるので、どの表記でも引ける。
        assert!(routes.get("web.feat-1.myapp.localhost").is_some());
        assert!(routes.get("WEB.feat-1.myapp.localhost:443").is_some());
        assert!(routes.get("web.feat-1.myapp.localhost.").is_some());
        assert!(routes.get("other.localhost").is_none());
    }

    #[test]
    fn remove_deletes_the_entry() {
        let routes = Routes::new();
        routes.insert("web.feat-1.myapp.localhost", route(3000));
        routes.remove("WEB.feat-1.myapp.localhost:80");

        assert!(routes.is_empty());
    }

    #[test]
    fn replace_project_swaps_only_that_project() {
        let routes = Routes::new();
        routes.insert("web.feat-1.myapp.localhost", route(3000));
        routes.insert(
            "web.other.localhost",
            Route::new(
                SocketAddr::from(([127, 0, 0, 1], 9000)),
                "other",
                "main",
                "web",
            ),
        );

        routes.replace_project(
            "myapp",
            vec![(
                "api.feat-2.myapp.localhost".to_string(),
                Route::new(
                    SocketAddr::from(([127, 0, 0, 1], 4000)),
                    "myapp",
                    "feat-2",
                    "api",
                ),
            )],
        );

        assert!(
            routes.get("web.feat-1.myapp.localhost").is_none(),
            "古いルートは消える"
        );
        assert!(routes.get("api.feat-2.myapp.localhost").is_some());
        assert!(
            routes.get("web.other.localhost").is_some(),
            "他プロジェクトには触れない"
        );
    }

    #[test]
    fn replace_project_with_nothing_clears_it() {
        let routes = Routes::new();
        routes.insert("web.feat-1.myapp.localhost", route(3000));

        routes.replace_project("myapp", vec![]);
        assert!(routes.is_empty(), "全サービス停止でルートが残らない");
    }

    #[test]
    fn stopped_routes_are_registered_without_an_endpoint() {
        // 「止まっている」と「存在しない」を区別できないと、
        // 停止中のサービスを起こせず 404 になってしまう。
        let routes = Routes::new();
        routes.insert(
            "web.feat-1.myapp.localhost",
            Route::stopped("myapp", "feat-1", "web"),
        );

        let route = routes
            .get("web.feat-1.myapp.localhost")
            .expect("登録されている");
        assert!(!route.is_running());
        assert_eq!(route.endpoint, None);

        assert!(
            routes.get("never-created.myapp.localhost").is_none(),
            "存在しないホストは別物"
        );
    }

    #[test]
    fn snapshot_is_sorted_for_stable_output() {
        let routes = Routes::new();
        routes.insert("web.feat-1.myapp.localhost", route(3000));
        routes.insert("api.feat-1.myapp.localhost", route(4000));

        let hosts: Vec<String> = routes.snapshot().into_iter().map(|(h, _)| h).collect();
        assert_eq!(
            hosts,
            vec!["api.feat-1.myapp.localhost", "web.feat-1.myapp.localhost"]
        );
    }
}

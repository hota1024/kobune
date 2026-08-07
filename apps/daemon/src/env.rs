//! サービスに渡す環境変数を組み立てる。
//!
//! 層の重ね方は `docs/DESIGN.md` §8 のとおり。自動注入を最初に置き、
//! 利用者の指定で上書きできるようにする。逆にすると、Minato の都合で
//! 利用者の設定が消える。

use indexmap::IndexMap;
use minato_core::{EnvLayers, EnvScope, MinatoConfig, Paths, WorkspaceRecord, env};

use crate::gateway::Gateway;

/// 自動注入する変数。
///
/// **`MINATO_URL_<SERVICE>` が要。** フロントが API の URL を知る手段が
/// ないと、worktree ごとに URL が変わる構成が成立しない。
pub fn injected(
    config: &MinatoConfig,
    project: &str,
    record: &WorkspaceRecord,
    service: &str,
    gateway: &Gateway,
) -> IndexMap<String, String> {
    let mut values = IndexMap::new();

    values.insert("MINATO_PROJECT".to_string(), project.to_string());
    values.insert("MINATO_WORKSPACE".to_string(), record.label.clone());
    values.insert("MINATO_SERVICE".to_string(), service.to_string());

    let domain = config.domain();
    for (name, service_config) in &config.services {
        if !service_config.exposed() {
            continue;
        }

        let host = minato_core::naming::service_host_in(name, record.url_label(), &domain);
        let Some(url) = gateway.url_for(&host) else {
            // プロキシが動いていなければ URL は存在しない。
            // 空文字を入れると「設定されているのに壊れている」状態になる。
            continue;
        };

        values.insert(url_variable(name), url);
    }

    values
}

/// サービス名から環境変数名を作る。
///
/// `cache-store` → `MINATO_URL_CACHE_STORE`。ハイフンは環境変数名に
/// 使えないのでアンダースコアにする。
pub fn url_variable(service: &str) -> String {
    format!("MINATO_URL_{}", service.to_uppercase().replace('-', "_"))
}

/// 1 サービス分の層を、優先度の低い順に積む。
pub fn layers_for_service(
    config: &MinatoConfig,
    project: &str,
    record: &WorkspaceRecord,
    project_root: &std::path::Path,
    service: &str,
    paths: &Paths,
    gateway: &Gateway,
) -> Result<EnvLayers, env::EnvError> {
    let mut layers = EnvLayers::new();

    // 1. 自動注入（利用者が上書きできるよう最初に置く）
    layers.push(
        EnvScope::Injected,
        injected(config, project, record, service, gateway),
    );

    // 2. global
    layers.push_file(EnvScope::Global, &paths.root().join(env::GLOBAL_ENV_FILE))?;

    // 3. project の共通ファイル
    layers.push_file(EnvScope::Project, &env::project_env_path(project_root))?;

    // 4. minato.toml のサービス個別指定（project より具体的）
    if let Ok(service_config) = config.service(service) {
        let values: IndexMap<String, String> = service_config
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        layers.push(EnvScope::Project, values);
    }

    // 5. workspace（最も具体的なので最後）
    layers.push_file(EnvScope::Workspace, &env::workspace_env_path(&record.path))?;

    Ok(layers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn config(toml: &str) -> MinatoConfig {
        let config: MinatoConfig = toml::from_str(toml).expect("構文は正しい");
        config.validate().expect("意味も正しい");
        config
    }

    const SAMPLE: &str = r#"
        [project]
        name = "myapp"
        [services.web]
        image = "node:22"
        port = 3000
        [services.api-server]
        image = "node:22"
        port = 8080
        [services.db]
        image = "postgres:16"
        port = 5432
        expose = false
    "#;

    fn record(label: &str, is_main: bool) -> WorkspaceRecord {
        WorkspaceRecord {
            label: label.to_string(),
            branch: "feature/one".to_string(),
            path: PathBuf::from("/repo/wt/feat-1"),
            is_main,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn injects_urls_for_every_exposed_service() {
        // これが無いと、フロントは API の URL を知る手段がない。
        let values = injected(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            "web",
            &Gateway::with_ports(Some(80), Some(443)),
        );

        assert_eq!(
            values.get("MINATO_URL_WEB").map(String::as_str),
            Some("https://web.feat-1.myapp.localhost")
        );
        assert_eq!(
            values.get("MINATO_URL_API_SERVER").map(String::as_str),
            Some("https://api-server.feat-1.myapp.localhost"),
            "ハイフンはアンダースコアにする"
        );
        assert!(
            !values.contains_key("MINATO_URL_DB"),
            "expose = false のサービスに URL は無い"
        );
    }

    #[test]
    fn injects_the_service_own_identity() {
        let values = injected(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            "web",
            &Gateway::inert(),
        );

        assert_eq!(
            values.get("MINATO_PROJECT").map(String::as_str),
            Some("myapp")
        );
        assert_eq!(
            values.get("MINATO_WORKSPACE").map(String::as_str),
            Some("feat-1")
        );
        assert_eq!(
            values.get("MINATO_SERVICE").map(String::as_str),
            Some("web")
        );
    }

    #[test]
    fn omits_urls_when_the_proxy_is_down() {
        // 空文字を入れると「設定されているのに繋がらない」状態になる。
        let values = injected(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            "web",
            &Gateway::inert(),
        );

        assert!(!values.keys().any(|key| key.starts_with("MINATO_URL_")));
    }

    #[test]
    fn main_workspace_urls_omit_the_label() {
        let values = injected(
            &config(SAMPLE),
            "myapp",
            &record("main", true),
            "web",
            &Gateway::with_ports(Some(80), Some(443)),
        );

        assert_eq!(
            values.get("MINATO_URL_WEB").map(String::as_str),
            Some("https://web.myapp.localhost")
        );
    }

    #[test]
    fn url_variable_names_are_shell_safe() {
        assert_eq!(url_variable("web"), "MINATO_URL_WEB");
        assert_eq!(url_variable("api-server"), "MINATO_URL_API_SERVER");
    }
}

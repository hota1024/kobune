//! `minato.toml` のスキーマとバリデーション。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::naming;

/// 設定ファイルの名前。
pub const CONFIG_FILE: &str = "minato.toml";

/// worktree のソースをコンテナ内にマウントする先。
pub const MOUNT_TARGET: &str = "/workspace";

/// `idle_timeout` を省略したときの既定値。
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MinatoConfig {
    pub project: ProjectSection,

    #[serde(default)]
    pub runtime: RuntimeSection,

    #[serde(default)]
    pub services: IndexMap<String, ServiceConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSection {
    pub name: String,

    /// URL の接尾辞。省略時は `{name}.localhost`。
    #[serde(default)]
    pub domain: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSection {
    #[serde(default = "default_runtime")]
    pub default: String,
}

impl Default for RuntimeSection {
    fn default() -> Self {
        Self {
            default: default_runtime(),
        }
    }
}

fn default_runtime() -> String {
    "docker".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    /// 既製イメージを使う場合のイメージ名。`build` と排他。
    #[serde(default)]
    pub image: Option<String>,

    /// Dockerfile のあるディレクトリ。`image` と排他。
    #[serde(default)]
    pub build: Option<String>,

    /// サービスが待ち受けるコンテナ内のポート。
    #[serde(default)]
    pub port: Option<u16>,

    /// 起動コマンド。省略時はイメージの CMD を使う。
    #[serde(default)]
    pub command: Option<String>,

    /// コンテナ内の作業ディレクトリ。省略時は [`MOUNT_TARGET`]。
    #[serde(default)]
    pub workdir: Option<String>,

    /// 起動完了の判定方法。scale-to-zero で使う。
    #[serde(default)]
    pub health: Option<HealthCheck>,

    /// 無アクセスでの自動停止までの時間。
    #[serde(default, with = "humantime_serde::option")]
    pub idle_timeout: Option<Duration>,

    #[serde(default)]
    pub env: BTreeMap<String, String>,

    #[serde(default)]
    pub depends_on: Vec<String>,

    #[serde(default)]
    pub scope: ServiceScope,

    /// URL を生やすかどうか。省略時は `port` があれば true。
    #[serde(default)]
    pub expose: Option<bool>,

    #[serde(default)]
    pub volumes: Vec<String>,
}

impl ServiceConfig {
    /// URL を生やすかどうかの実効値。
    pub fn exposed(&self) -> bool {
        self.expose.unwrap_or(self.port.is_some())
    }

    pub fn workdir(&self) -> &str {
        self.workdir.as_deref().unwrap_or(MOUNT_TARGET)
    }

    pub fn idle_timeout(&self) -> Duration {
        self.idle_timeout.unwrap_or(DEFAULT_IDLE_TIMEOUT)
    }
}

/// サービスのインスタンスを worktree ごとに分けるか、プロジェクトで共有するか。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceScope {
    /// worktree ごとに独立したインスタンスを立てる。
    #[default]
    Workspace,
    /// 同一プロジェクトの全 worktree で 1 インスタンスを共有する。
    Project,
}

/// 起動完了の判定方法。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub enum HealthCheck {
    /// 2xx / 3xx が返れば ready。
    Http(String),
    /// TCP 接続が確立できれば ready。
    Tcp(String),
    /// コンテナ内で実行し、終了コード 0 なら ready。
    Cmd(String),
}

impl FromStr for HealthCheck {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix("cmd:") {
            let cmd = rest.trim();
            if cmd.is_empty() {
                return Err("cmd: の後にコマンドがありません".to_string());
            }
            return Ok(Self::Cmd(cmd.to_string()));
        }
        if s.starts_with("http://") || s.starts_with("https://") {
            return Ok(Self::Http(s.to_string()));
        }
        if let Some(rest) = s.strip_prefix("tcp://") {
            if rest.is_empty() {
                return Err("tcp:// の後にアドレスがありません".to_string());
            }
            return Ok(Self::Tcp(rest.to_string()));
        }
        Err(format!(
            "`{s}` は health の形式として解釈できません。\
             `http://...`, `https://...`, `tcp://host:port`, `cmd:...` のいずれかを指定してください"
        ))
    }
}

impl TryFrom<String> for HealthCheck {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<HealthCheck> for String {
    fn from(value: HealthCheck) -> Self {
        match value {
            HealthCheck::Http(url) => url,
            HealthCheck::Tcp(addr) => format!("tcp://{addr}"),
            HealthCheck::Cmd(cmd) => format!("cmd:{cmd}"),
        }
    }
}

impl MinatoConfig {
    /// `start` から上位ディレクトリへ向かって `minato.toml` を探す。
    ///
    /// 見つけたファイルのパスと、パース済みの設定を返す。
    pub fn find(start: &Path) -> Result<(PathBuf, Self)> {
        let mut dir = Some(start);
        while let Some(current) = dir {
            let candidate = current.join(CONFIG_FILE);
            if candidate.is_file() {
                let config = Self::load(&candidate)?;
                return Ok((candidate, config));
            }
            dir = current.parent();
        }
        Err(Error::ConfigNotFound(start.to_path_buf()))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::ConfigRead {
            path: path.to_path_buf(),
            source,
        })?;

        let config: Self = toml::from_str(&text).map_err(|source| Error::ConfigParse {
            path: path.to_path_buf(),
            source,
        })?;

        config.validate()?;
        Ok(config)
    }

    /// URL の接尾辞。`[project] domain` を省略した場合は `{name}.localhost`。
    pub fn domain(&self) -> String {
        self.project
            .domain
            .clone()
            .unwrap_or_else(|| format!("{}.localhost", self.project.name))
    }

    pub fn service(&self, name: &str) -> Result<&ServiceConfig> {
        self.services
            .get(name)
            .ok_or_else(|| Error::ServiceNotFound(name.to_string()))
    }

    /// 構文としては正しい設定が、意味的に成立しているかを検査する。
    pub fn validate(&self) -> Result<()> {
        if !naming::is_valid_label(&self.project.name) {
            return Err(Error::ConfigInvalid(format!(
                "[project] name = \"{}\" は DNS ラベルとして使えません。\
                 小文字・数字・ハイフンのみ、63 文字以内にしてください",
                self.project.name
            )));
        }

        if self.services.is_empty() {
            return Err(Error::ConfigInvalid(
                "サービスが 1 つも定義されていません".to_string(),
            ));
        }

        for (name, svc) in &self.services {
            self.validate_service(name, svc)?;
        }

        self.validate_no_dependency_cycle()?;
        Ok(())
    }

    fn validate_service(&self, name: &str, svc: &ServiceConfig) -> Result<()> {
        if !naming::is_valid_label(name) {
            return Err(Error::ConfigInvalid(format!(
                "サービス名 `{name}` は DNS ラベルとして使えません。\
                 小文字・数字・ハイフンのみ、63 文字以内にしてください"
            )));
        }

        match (&svc.image, &svc.build) {
            (Some(_), Some(_)) => {
                return Err(Error::ConfigInvalid(format!(
                    "サービス `{name}`: image と build は同時に指定できません"
                )));
            }
            (None, None) => {
                return Err(Error::ConfigInvalid(format!(
                    "サービス `{name}`: image か build のどちらかが必要です"
                )));
            }
            _ => {}
        }

        if svc.expose == Some(true) && svc.port.is_none() {
            return Err(Error::ConfigInvalid(format!(
                "サービス `{name}`: expose = true には port の指定が必要です"
            )));
        }

        for dep in &svc.depends_on {
            let target = self.services.get(dep).ok_or_else(|| {
                Error::ConfigInvalid(format!(
                    "サービス `{name}`: depends_on に未定義のサービス `{dep}` が指定されています"
                ))
            })?;

            // 共有インスタンスが worktree 固有のインスタンスに依存すると、
            // どの worktree の相手に繋ぐべきかが決まらない。
            if svc.scope == ServiceScope::Project && target.scope == ServiceScope::Workspace {
                return Err(Error::ConfigInvalid(format!(
                    "サービス `{name}` (scope = \"project\") が \
                     `{dep}` (scope = \"workspace\") に依存しています。\
                     共有サービスは worktree 固有のサービスに依存できません"
                )));
            }
        }

        Ok(())
    }

    fn validate_no_dependency_cycle(&self) -> Result<()> {
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            Unvisited,
            InProgress,
            Done,
        }

        let mut marks: IndexMap<&str, Mark> = self
            .services
            .keys()
            .map(|k| (k.as_str(), Mark::Unvisited))
            .collect();

        // 明示的なスタックで DFS する。パスを保持して循環を可読なメッセージにする。
        for root in self.services.keys() {
            if marks[root.as_str()] != Mark::Unvisited {
                continue;
            }

            let mut path: Vec<&str> = vec![root.as_str()];
            let mut cursor: Vec<usize> = vec![0];
            marks[root.as_str()] = Mark::InProgress;

            while let Some(&node) = path.last() {
                let index = *cursor.last().expect("cursor と path は同じ深さで積む");
                let deps = &self.services[node].depends_on;

                if index >= deps.len() {
                    marks[node] = Mark::Done;
                    path.pop();
                    cursor.pop();
                    continue;
                }

                *cursor.last_mut().expect("同上") += 1;
                let dep = deps[index].as_str();

                match marks[dep] {
                    Mark::Done => {}
                    Mark::InProgress => {
                        let start = path.iter().position(|n| *n == dep).unwrap_or(0);
                        let mut chain: Vec<&str> = path[start..].to_vec();
                        chain.push(dep);
                        return Err(Error::ConfigInvalid(format!(
                            "depends_on が循環しています: {}",
                            chain.join(" -> ")
                        )));
                    }
                    Mark::Unvisited => {
                        marks[dep] = Mark::InProgress;
                        path.push(dep);
                        cursor.push(0);
                    }
                }
            }
        }

        Ok(())
    }

    /// depends_on を満たす順にサービス名を並べる。
    ///
    /// [`Self::validate`] を通っていれば循環はないので、必ず全サービスを返す。
    pub fn startup_order(&self) -> Vec<&str> {
        let mut ordered: Vec<&str> = Vec::with_capacity(self.services.len());
        let mut visited: IndexMap<&str, bool> =
            self.services.keys().map(|k| (k.as_str(), false)).collect();

        for root in self.services.keys() {
            let mut stack = vec![(root.as_str(), 0usize)];

            while let Some((node, index)) = stack.pop() {
                if visited[node] {
                    continue;
                }

                let deps = &self.services[node].depends_on;
                if index < deps.len() {
                    stack.push((node, index + 1));
                    let dep = deps[index].as_str();
                    if !visited[dep] {
                        stack.push((dep, 0));
                    }
                } else {
                    visited[node] = true;
                    ordered.push(node);
                }
            }
        }

        ordered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<MinatoConfig> {
        let config: MinatoConfig = toml::from_str(text).expect("構文は正しい前提");
        config.validate()?;
        Ok(config)
    }

    const MINIMAL: &str = r#"
        [project]
        name = "myapp"

        [services.web]
        image = "node:22"
        port = 3000
    "#;

    #[test]
    fn parses_minimal_config() {
        let config = parse(MINIMAL).expect("valid");
        assert_eq!(config.project.name, "myapp");
        assert_eq!(config.runtime.default, "docker");
        assert_eq!(config.domain(), "myapp.localhost");

        let web = config.service("web").expect("exists");
        assert!(web.exposed());
        assert_eq!(web.workdir(), MOUNT_TARGET);
        assert_eq!(web.idle_timeout(), DEFAULT_IDLE_TIMEOUT);
    }

    #[test]
    fn parses_full_config() {
        let config = parse(
            r#"
            [project]
            name = "myapp"
            domain = "dev.example.com"

            [runtime]
            default = "docker"

            [services.web]
            image = "node:22"
            port = 3000
            command = "pnpm dev"
            health = "http://localhost:3000/healthz"
            idle_timeout = "15m"
            depends_on = ["db"]
            env = { NODE_ENV = "development" }

            [services.db]
            image = "postgres:16"
            port = 5432
            scope = "project"
            expose = false
            volumes = ["pgdata:/var/lib/postgresql/data"]
            health = "tcp://localhost:5432"
        "#,
        )
        .expect("valid");

        assert_eq!(config.domain(), "dev.example.com");

        let web = config.service("web").expect("exists");
        assert_eq!(web.idle_timeout(), Duration::from_secs(15 * 60));
        assert_eq!(
            web.health,
            Some(HealthCheck::Http("http://localhost:3000/healthz".into()))
        );

        let db = config.service("db").expect("exists");
        assert_eq!(db.scope, ServiceScope::Project);
        assert!(
            !db.exposed(),
            "expose = false なら port があっても公開しない"
        );
        assert_eq!(db.health, Some(HealthCheck::Tcp("localhost:5432".into())));
    }

    #[test]
    fn rejects_image_and_build_together() {
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            build = "./web"
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("同時に指定できません"));
    }

    #[test]
    fn rejects_service_without_source() {
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            port = 3000
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("image か build"));
    }

    #[test]
    fn rejects_expose_without_port() {
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            expose = true
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("port の指定が必要"));
    }

    #[test]
    fn rejects_unknown_dependency() {
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            depends_on = ["nope"]
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("未定義のサービス"));
    }

    #[test]
    fn rejects_shared_service_depending_on_workspace_service() {
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.cache]
            image = "redis:7"
            scope = "project"
            depends_on = ["web"]
            [services.web]
            image = "node:22"
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("共有サービスは"));
    }

    #[test]
    fn rejects_dependency_cycle() {
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.a]
            image = "x"
            depends_on = ["b"]
            [services.b]
            image = "x"
            depends_on = ["a"]
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("循環"), "got: {err}");
    }

    #[test]
    fn rejects_self_dependency() {
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.a]
            image = "x"
            depends_on = ["a"]
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("循環"), "got: {err}");
    }

    #[test]
    fn rejects_invalid_project_name() {
        let err = parse(
            r#"
            [project]
            name = "My App"
            [services.web]
            image = "node:22"
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("DNS ラベル"));
    }

    #[test]
    fn rejects_unknown_field() {
        let result: std::result::Result<MinatoConfig, _> = toml::from_str(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            porrt = 3000
        "#,
        );
        assert!(result.is_err(), "typo は検出されるべき");
    }

    #[test]
    fn startup_order_respects_dependencies() {
        let config = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "x"
            depends_on = ["api"]
            [services.api]
            image = "x"
            depends_on = ["db"]
            [services.db]
            image = "x"
        "#,
        )
        .expect("valid");

        let order = config.startup_order();
        assert_eq!(order.len(), 3);

        let pos = |name: &str| order.iter().position(|s| *s == name).expect("含まれる");
        assert!(pos("db") < pos("api"));
        assert!(pos("api") < pos("web"));
    }

    #[test]
    fn startup_order_handles_diamond() {
        let config = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "x"
            depends_on = ["api", "worker"]
            [services.api]
            image = "x"
            depends_on = ["db"]
            [services.worker]
            image = "x"
            depends_on = ["db"]
            [services.db]
            image = "x"
        "#,
        )
        .expect("valid");

        let order = config.startup_order();
        assert_eq!(order.len(), 4, "重複なく全サービスが並ぶ: {order:?}");

        let pos = |name: &str| order.iter().position(|s| *s == name).expect("含まれる");
        assert!(pos("db") < pos("api"));
        assert!(pos("db") < pos("worker"));
        assert!(pos("api") < pos("web"));
        assert!(pos("worker") < pos("web"));
    }

    #[test]
    fn health_check_roundtrip() {
        let cases = [
            (
                "http://localhost:3000/healthz",
                "http://localhost:3000/healthz",
            ),
            ("tcp://localhost:5432", "tcp://localhost:5432"),
            ("cmd:pg_isready", "cmd:pg_isready"),
        ];

        for (input, expected) in cases {
            let parsed: HealthCheck = input.parse().expect("parses");
            let back: String = parsed.into();
            assert_eq!(back, expected);
        }
    }

    #[test]
    fn health_check_rejects_unknown_scheme() {
        assert!("ftp://x".parse::<HealthCheck>().is_err());
        assert!("localhost:3000".parse::<HealthCheck>().is_err());
    }
}

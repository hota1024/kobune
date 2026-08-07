//! `minato init` — `minato.toml` のひな形を作る。
//!
//! 対話プロンプトは出さない。エージェントが実行できなくなるため、
//! 推定できるものは推定し、残りはコメント付きのひな形にする。

use std::path::{Path, PathBuf};

use minato_core::config::CONFIG_FILE;
use minato_core::{Repository, naming};

#[derive(Debug)]
pub struct InitOutcome {
    pub path: PathBuf,
    pub project: String,
}

pub fn run(cwd: &Path, force: bool) -> anyhow::Result<InitOutcome> {
    // リポジトリのルートに置く。worktree の中で実行されても main worktree に作る。
    let root = match Repository::discover(cwd) {
        Ok(repo) => repo.main_root,
        Err(_) => cwd.to_path_buf(),
    };

    let path = root.join(CONFIG_FILE);
    if path.exists() && !force {
        anyhow::bail!(
            "{} は既に存在します。上書きするには --force を付けてください",
            path.display()
        );
    }

    let project = project_name_from(&root);
    std::fs::write(&path, template(&project))?;

    Ok(InitOutcome { path, project })
}

/// ディレクトリ名からプロジェクト名を導く。
fn project_name_from(root: &Path) -> String {
    let raw = root
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "app".to_string());

    naming::sanitize_label(&raw)
}

fn template(project: &str) -> String {
    format!(
        r#"[project]
name = "{project}"

# URL の接尾辞。省略すると {project}.localhost になる。
# domain = "{project}.localhost"

[runtime]
# docker または apple（Apple Container）
default = "docker"

# サービスを 1 つ以上定義する。
# worktree のソースは各コンテナの /workspace にマウントされる。
[services.app]
image = "node:22"
port = 3000
command = "sh -c 'echo minato ready; sleep infinity'"

# 起動完了の判定。scale-to-zero（M2）で使う。
# health = "http://localhost:3000/healthz"

# 無アクセスでの自動停止までの時間。
# idle_timeout = "30m"

# [services.db]
# image = "postgres:16"
# port = 5432
# scope = "project"    # worktree 間で 1 つのインスタンスを共有する
# expose = false       # URL を生やさない
# volumes = ["pgdata:/var/lib/postgresql/data"]
# env = {{ POSTGRES_PASSWORD = "postgres" }}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use minato_core::MinatoConfig;

    #[test]
    fn generated_template_is_valid() {
        // ひな形が設定として通らないと、init 直後に up が失敗する。
        let text = template("myapp");
        let config: MinatoConfig = toml::from_str(&text).expect("構文が正しい");
        config.validate().expect("意味も正しい");

        assert_eq!(config.project.name, "myapp");
        assert_eq!(config.runtime.default, "docker");
        assert!(config.services.contains_key("app"));
    }

    #[test]
    fn derives_project_name_from_directory() {
        assert_eq!(project_name_from(Path::new("/x/My_App")), "my-app");
        assert_eq!(project_name_from(Path::new("/x/myapp")), "myapp");
    }

    #[test]
    fn writes_config_and_refuses_to_clobber() {
        let dir = tempfile::tempdir().expect("tempdir");

        let outcome = run(dir.path(), false).expect("作れる");
        assert!(outcome.path.is_file());

        let err = run(dir.path(), false).unwrap_err();
        assert!(err.to_string().contains("--force"), "got: {err}");

        run(dir.path(), true).expect("force なら上書きできる");
    }
}

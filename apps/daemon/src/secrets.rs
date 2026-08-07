//! シークレット参照の解決。
//!
//! **解決した値はディスクに書かない。** メモリ上でコンテナに渡すだけに
//! 留める。これがあるおかげで、リポジトリに置くのは参照だけで済む。
//!
//! 解決に失敗しても daemon は落とさない。1Password にサインインして
//! いないだけということが多く、その 1 つのために環境全体が起動しない
//! 方が困る。失敗は警告として伝え、そのキーだけ落とす。

use std::collections::HashMap;

use minato_core::SecretRef;
use tokio::process::Command;

/// 外部コマンドを待つ上限。
///
/// 1Password はサインインを求めて対話的に止まることがある。daemon は
/// 応答できないので、待ち続けずに諦める。
const RESOLVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 解決の結果。
pub struct Resolved {
    /// 解決できた値。
    pub values: HashMap<String, String>,
    /// 解決できなかったキーと理由。
    pub failures: Vec<(String, String)>,
}

/// シークレット参照を解決する。
///
/// `entries` は（キー, 参照）の並び。参照でない値はここに渡さない。
pub async fn resolve(entries: &[(String, SecretRef)]) -> Resolved {
    let mut values = HashMap::new();
    let mut failures = Vec::new();

    // 同じ参照が複数のキーから使われることがある。1 度だけ引く。
    let mut cache: HashMap<SecretRef, Result<String, String>> = HashMap::new();

    for (key, reference) in entries {
        let outcome = match cache.get(reference) {
            Some(cached) => cached.clone(),
            None => {
                let fetched = fetch(reference).await;
                cache.insert(reference.clone(), fetched.clone());
                fetched
            }
        };

        match outcome {
            Ok(value) => {
                values.insert(key.clone(), value);
            }
            Err(reason) => failures.push((key.clone(), reason)),
        }
    }

    Resolved { values, failures }
}

async fn fetch(reference: &SecretRef) -> Result<String, String> {
    match reference {
        SecretRef::Env(name) => std::env::var(name)
            .map_err(|_| format!("daemon の環境変数 `{name}` が設定されていません")),
        SecretRef::OnePassword(uri) => run("op", &["read", "--no-newline", uri]).await,
        SecretRef::Keychain { service, account } => {
            run(
                "security",
                &["find-generic-password", "-s", service, "-a", account, "-w"],
            )
            .await
        }
    }
}

async fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let output = tokio::time::timeout(RESOLVE_TIMEOUT, Command::new(program).args(args).output())
        .await
        .map_err(|_| {
            format!(
                "{program} が {}秒 以内に応答しませんでした（サインインを求めている可能性があります）",
                RESOLVE_TIMEOUT.as_secs()
            )
        })?;

    let output = output.map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            format!("`{program}` が見つかりません")
        } else {
            format!("`{program}` を実行できません: {err}")
        }
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!(
                "`{program}` が終了コード {} で失敗しました",
                output.status.code().unwrap_or(-1)
            )
        } else {
            stderr
        });
    }

    // `security -w` は末尾に改行を付ける。値に混ぜると認証に失敗する。
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end_matches('\n')
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolves_from_the_daemon_environment() {
        // SAFETY: テストは同一プロセスで並行実行されるが、この変数名は
        // このテストでしか使わない。
        unsafe { std::env::set_var("MINATO_TEST_SECRET", "s3cret") };

        let resolved = resolve(&[(
            "API_KEY".to_string(),
            SecretRef::Env("MINATO_TEST_SECRET".into()),
        )])
        .await;

        assert_eq!(
            resolved.values.get("API_KEY").map(String::as_str),
            Some("s3cret")
        );
        assert!(resolved.failures.is_empty());
    }

    #[tokio::test]
    async fn missing_environment_variable_is_reported_not_fatal() {
        let resolved = resolve(&[(
            "API_KEY".to_string(),
            SecretRef::Env("MINATO_TEST_DEFINITELY_UNSET".into()),
        )])
        .await;

        assert!(resolved.values.is_empty());
        assert_eq!(resolved.failures.len(), 1);
        assert_eq!(resolved.failures[0].0, "API_KEY");
        assert!(resolved.failures[0].1.contains("設定されていません"));
    }

    #[tokio::test]
    async fn other_keys_survive_one_failure() {
        // 1 つ解決できないだけで環境全体が起動しないのは困る。
        unsafe { std::env::set_var("MINATO_TEST_OK", "fine") };

        let resolved = resolve(&[
            (
                "BAD".to_string(),
                SecretRef::Env("MINATO_TEST_MISSING".into()),
            ),
            ("GOOD".to_string(), SecretRef::Env("MINATO_TEST_OK".into())),
        ])
        .await;

        assert_eq!(
            resolved.values.get("GOOD").map(String::as_str),
            Some("fine")
        );
        assert_eq!(resolved.failures.len(), 1);
    }

    #[tokio::test]
    async fn missing_program_is_reported_clearly() {
        let err = run("minato-definitely-not-a-program", &[])
            .await
            .unwrap_err();

        assert!(err.contains("見つかりません"), "got: {err}");
    }

    #[tokio::test]
    async fn trailing_newline_is_stripped() {
        // `security -w` も `op read` も改行を付けることがある。
        // 値に混ざると認証に失敗し、原因が非常に分かりにくい。
        let value = run("printf", &["token\n"]).await.expect("実行できる");
        assert_eq!(value, "token");
    }

    #[tokio::test]
    async fn nonzero_exit_carries_stderr() {
        let err = run("sh", &["-c", "echo 詳細 >&2; exit 1"])
            .await
            .unwrap_err();

        assert!(err.contains("詳細"), "got: {err}");
    }

    #[tokio::test]
    async fn identical_references_are_fetched_once() {
        unsafe { std::env::set_var("MINATO_TEST_SHARED", "shared") };

        let reference = SecretRef::Env("MINATO_TEST_SHARED".into());
        let resolved = resolve(&[
            ("A".to_string(), reference.clone()),
            ("B".to_string(), reference),
        ])
        .await;

        assert_eq!(resolved.values.len(), 2);
        assert_eq!(resolved.values.get("A"), resolved.values.get("B"));
    }
}

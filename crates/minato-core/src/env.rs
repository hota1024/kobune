//! 環境変数の層と、その読み書き。
//!
//! 3 つの層を後勝ちで重ねる。
//!
//! | 層 | 置き場所 | 想定 |
//! | --- | --- | --- |
//! | global | `~/.minato/env` | 全プロジェクト共通 |
//! | project | `minato.toml` の `env` と `.minato/env` | リポジトリにコミットする |
//! | workspace | `.minato/env.local` | worktree 固有。gitignore |
//!
//! **平文のシークレットをリポジトリに入れさせない。** 値に参照
//! （`op://` など）を書けるようにし、解決は起動時に daemon が行う。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// プロジェクト／workspace が環境変数を置くディレクトリ。
pub const ENV_DIR: &str = ".minato";

/// プロジェクト共通の環境変数ファイル（コミットする）。
pub const PROJECT_ENV_FILE: &str = "env";

/// worktree 固有の環境変数ファイル（gitignore）。
pub const WORKSPACE_ENV_FILE: &str = "env.local";

/// global の環境変数ファイル（`$MINATO_HOME` 直下）。
pub const GLOBAL_ENV_FILE: &str = "env";

/// 環境変数がどこで定義されたか。
///
/// 並び順が優先順位。後のものが前を上書きする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvScope {
    /// 全プロジェクト共通。
    Global,
    /// プロジェクト共通。
    Project,
    /// worktree 固有。
    Workspace,
    /// Minato が自動で入れるもの。利用者は上書きできる。
    Injected,
}

impl EnvScope {
    /// `minato env set` で指定できる層。
    pub const WRITABLE: &'static [EnvScope] =
        &[EnvScope::Global, EnvScope::Project, EnvScope::Workspace];

    pub fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
            Self::Workspace => "workspace",
            Self::Injected => "injected",
        }
    }

    /// 書き込める層かどうか。
    pub fn is_writable(self) -> bool {
        Self::WRITABLE.contains(&self)
    }
}

impl std::str::FromStr for EnvScope {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "global" => Ok(Self::Global),
            "project" => Ok(Self::Project),
            "workspace" => Ok(Self::Workspace),
            other => Err(format!(
                "`{other}` は環境変数の層として不正です。global / project / workspace のいずれかを指定してください"
            )),
        }
    }
}

/// 解決前の 1 エントリ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvEntry {
    pub key: String,
    /// 書かれたままの値。シークレット参照の場合は参照文字列そのもの。
    pub raw: String,
    pub scope: EnvScope,
}

impl EnvEntry {
    /// シークレット参照かどうか。
    pub fn secret_ref(&self) -> Option<SecretRef> {
        SecretRef::parse(&self.raw)
    }
}

/// 層を重ねたもの。
#[derive(Debug, Default, Clone)]
pub struct EnvLayers {
    layers: Vec<(EnvScope, IndexMap<String, String>)>,
}

impl EnvLayers {
    pub fn new() -> Self {
        Self::default()
    }

    /// 層を追加する。**追加した順に優先度が上がる。**
    pub fn push(&mut self, scope: EnvScope, values: IndexMap<String, String>) {
        self.layers.push((scope, values));
    }

    /// dotenv ファイルを層として読む。ファイルが無い場合は何もしない。
    pub fn push_file(&mut self, scope: EnvScope, path: &Path) -> Result<(), EnvError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(EnvError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };

        let values = parse(&text).map_err(|message| EnvError::Parse {
            path: path.to_path_buf(),
            message,
        })?;

        self.push(scope, values);
        Ok(())
    }

    /// 重ねた結果。キーごとに、最も優先度の高い層の値を返す。
    ///
    /// 並びはキー順。表示と比較を安定させるため。
    pub fn resolve(&self) -> Vec<EnvEntry> {
        let mut merged: BTreeMap<String, EnvEntry> = BTreeMap::new();

        for (scope, values) in &self.layers {
            for (key, raw) in values {
                merged.insert(
                    key.clone(),
                    EnvEntry {
                        key: key.clone(),
                        raw: raw.clone(),
                        scope: *scope,
                    },
                );
            }
        }

        merged.into_values().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.layers.iter().all(|(_, values)| values.is_empty())
    }
}

/// 外部から取り出すシークレットの参照。
///
/// 平文をリポジトリに置かないための仕組み。解決は起動時に行い、
/// 結果はディスクに書かない。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SecretRef {
    /// 1Password CLI (`op read`)。
    OnePassword(String),
    /// macOS キーチェーン。`keychain://<service>/<account>`。
    Keychain { service: String, account: String },
    /// daemon プロセスの環境変数。
    Env(String),
}

impl SecretRef {
    pub fn parse(value: &str) -> Option<Self> {
        if value.starts_with("op://") {
            return Some(Self::OnePassword(value.to_string()));
        }

        if let Some(rest) = value.strip_prefix("keychain://") {
            let (service, account) = rest.split_once('/')?;
            if service.is_empty() || account.is_empty() {
                return None;
            }
            return Some(Self::Keychain {
                service: service.to_string(),
                account: account.to_string(),
            });
        }

        if let Some(name) = value.strip_prefix("env://") {
            if name.is_empty() {
                return None;
            }
            return Some(Self::Env(name.to_string()));
        }

        None
    }

    /// 表示用の説明。値そのものは出さない。
    pub fn describe(&self) -> String {
        match self {
            Self::OnePassword(reference) => format!("1Password ({reference})"),
            Self::Keychain { service, account } => {
                format!("キーチェーン ({service}/{account})")
            }
            Self::Env(name) => format!("daemon の環境変数 ({name})"),
        }
    }
}

/// 値を伏せる。ログや一覧に平文を出さないため。
pub fn mask(value: &str) -> String {
    let length = value.chars().count();

    if length == 0 {
        return "(空)".to_string();
    }

    // 短い値は先頭も出さない。1 文字でも漏れると総当たりの手掛かりになる。
    if length <= 4 {
        return "•".repeat(length);
    }

    let head: String = value.chars().take(2).collect();
    format!("{head}{}", "•".repeat(length - 2))
}

#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error("環境変数ファイルを読めません ({path}): {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("環境変数ファイルを書けません ({path}): {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("環境変数ファイルの書式が不正です ({path}): {message}")]
    Parse { path: PathBuf, message: String },

    #[error("環境変数名として使えません: `{0}`")]
    InvalidKey(String),
}

/// 環境変数名として妥当か。
///
/// シェルや Docker に渡せない名前を弾く。
pub fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with(|c: char| c.is_ascii_digit())
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// dotenv 形式を読む。
///
/// - `KEY=VALUE`、`export KEY=VALUE`
/// - `#` から行末までコメント（クォートの中を除く）
/// - `"..."` は `\n` `\t` `\\` `\"` を解釈、`'...'` はそのまま
pub fn parse(text: &str) -> std::result::Result<IndexMap<String, String>, String> {
    let mut values = IndexMap::new();

    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let statement = trimmed.strip_prefix("export ").unwrap_or(trimmed).trim();

        let Some((key, raw_value)) = statement.split_once('=') else {
            return Err(format!("{line_number} 行目: `=` がありません: {trimmed}"));
        };

        let key = key.trim();
        if !is_valid_key(key) {
            return Err(format!(
                "{line_number} 行目: `{key}` は環境変数名として使えません"
            ));
        }

        values.insert(key.to_string(), parse_value(raw_value.trim()));
    }

    Ok(values)
}

fn parse_value(raw: &str) -> String {
    if let Some(inner) = raw.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        return unescape(inner);
    }

    if let Some(inner) = raw.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        // シングルクォートは中身をそのまま使う。
        return inner.to_string();
    }

    // クォートが無い場合、`#` 以降はコメント。
    match raw.split_once(" #") {
        Some((value, _)) => value.trim_end().to_string(),
        None => raw.to_string(),
    }
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();

    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }

        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            // 知らないエスケープはそのまま残す。壊すより素通しの方が安全。
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }

    out
}

/// 既存のファイル内容に `key` を設定する。
///
/// **コメントと行の順序を保つ。** 利用者が手で書いたファイルを
/// 書き換える以上、勝手に整形してはいけない。
pub fn upsert(text: &str, key: &str, value: &str) -> String {
    let rendered = format!("{key}={}", quote(value));
    let mut replaced = false;

    let mut lines: Vec<String> = text
        .lines()
        .map(|line| {
            if replaced || !defines_key(line, key) {
                return line.to_string();
            }
            replaced = true;
            rendered.clone()
        })
        .collect();

    if !replaced {
        lines.push(rendered);
    }

    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// `key` の定義を取り除く。
pub fn remove(text: &str, key: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !defines_key(line, key))
        .collect();

    if lines.is_empty() {
        return String::new();
    }

    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// その行が `key` を定義しているか。
fn defines_key(line: &str, key: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return false;
    }

    let statement = trimmed.strip_prefix("export ").unwrap_or(trimmed).trim();
    statement
        .split_once('=')
        .is_some_and(|(found, _)| found.trim() == key)
}

/// 値を書き出す形にする。必要なときだけクォートする。
fn quote(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '#' | '$' | '`'));

    if !needs_quotes {
        return value.to_string();
    }

    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t");

    format!("\"{escaped}\"")
}

/// ファイルに書き出す。親ディレクトリが無ければ作る。
pub fn write_file(path: &Path, contents: &str) -> Result<(), EnvError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| EnvError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    // 環境変数ファイルにはシークレットの参照が入る。
    // 実体でなくても、どこに何があるかは他人に見せない。
    write_private(path, contents.as_bytes())
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &[u8]) -> Result<(), EnvError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| EnvError::Write {
            path: path.to_path_buf(),
            source,
        })?;

    file.write_all(contents).map_err(|source| EnvError::Write {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &[u8]) -> Result<(), EnvError> {
    std::fs::write(path, contents).map_err(|source| EnvError::Write {
        path: path.to_path_buf(),
        source,
    })
}

/// プロジェクトの環境変数ファイル（`{root}/.minato/env`）。
pub fn project_env_path(root: &Path) -> PathBuf {
    root.join(ENV_DIR).join(PROJECT_ENV_FILE)
}

/// worktree 固有の環境変数ファイル（`{worktree}/.minato/env.local`）。
pub fn workspace_env_path(worktree: &Path) -> PathBuf {
    worktree.join(ENV_DIR).join(WORKSPACE_ENV_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_assignments() {
        let values = parse("FOO=bar\nBAZ=qux\n").expect("読める");

        assert_eq!(values.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(values.get("BAZ").map(String::as_str), Some("qux"));
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let values = parse("# comment\n\nFOO=bar\n   \n# another\n").expect("読める");

        assert_eq!(values.len(), 1);
        assert_eq!(values.get("FOO").map(String::as_str), Some("bar"));
    }

    #[test]
    fn accepts_export_prefix() {
        // シェルからコピーした行をそのまま貼れるようにする。
        let values = parse("export FOO=bar\n").expect("読める");
        assert_eq!(values.get("FOO").map(String::as_str), Some("bar"));
    }

    #[test]
    fn handles_quotes() {
        let values = parse(
            r#"
SPACED="hello world"
RAW='no $interpolation here'
ESCAPED="line1\nline2"
"#,
        )
        .expect("読める");

        assert_eq!(
            values.get("SPACED").map(String::as_str),
            Some("hello world")
        );
        assert_eq!(
            values.get("RAW").map(String::as_str),
            Some("no $interpolation here")
        );
        assert_eq!(
            values.get("ESCAPED").map(String::as_str),
            Some("line1\nline2")
        );
    }

    #[test]
    fn strips_trailing_comments_outside_quotes() {
        let values = parse("FOO=bar # 説明\nURL=\"http://x/#anchor\"\n").expect("読める");

        assert_eq!(values.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(
            values.get("URL").map(String::as_str),
            Some("http://x/#anchor"),
            "クォートの中の # はコメントではない"
        );
    }

    #[test]
    fn empty_values_are_allowed() {
        let values = parse("EMPTY=\n").expect("読める");
        assert_eq!(values.get("EMPTY").map(String::as_str), Some(""));
    }

    #[test]
    fn rejects_lines_without_assignment() {
        let err = parse("JUST_A_WORD\n").unwrap_err();
        assert!(err.contains("1 行目"), "行番号を出す: {err}");
    }

    #[test]
    fn rejects_invalid_keys() {
        assert!(parse("1FOO=bar\n").is_err());
        assert!(parse("FOO-BAR=x\n").is_err());
        assert!(parse("FOO BAR=x\n").is_err());
    }

    #[test]
    fn validates_keys() {
        assert!(is_valid_key("FOO"));
        assert!(is_valid_key("_FOO_BAR1"));
        assert!(!is_valid_key(""));
        assert!(!is_valid_key("1FOO"));
        assert!(!is_valid_key("FOO-BAR"));
    }

    fn layer(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn later_layers_win() {
        let mut layers = EnvLayers::new();
        layers.push(EnvScope::Global, layer(&[("A", "global"), ("B", "global")]));
        layers.push(EnvScope::Project, layer(&[("B", "project")]));
        layers.push(EnvScope::Workspace, layer(&[("B", "workspace")]));

        let resolved = layers.resolve();
        let find = |key: &str| {
            resolved
                .iter()
                .find(|entry| entry.key == key)
                .expect("存在する")
                .clone()
        };

        assert_eq!(find("A").raw, "global");
        assert_eq!(find("A").scope, EnvScope::Global);

        assert_eq!(find("B").raw, "workspace", "最も内側の層が勝つ");
        assert_eq!(
            find("B").scope,
            EnvScope::Workspace,
            "どこで定義されたかも分かる必要がある"
        );
    }

    #[test]
    fn injected_values_can_be_overridden_by_the_user() {
        // 自動注入を先に置き、利用者の指定を後に重ねる。
        let mut layers = EnvLayers::new();
        layers.push(EnvScope::Injected, layer(&[("MINATO_URL_WEB", "auto")]));
        layers.push(EnvScope::Project, layer(&[("MINATO_URL_WEB", "custom")]));

        let resolved = layers.resolve();
        assert_eq!(resolved[0].raw, "custom");
        assert_eq!(resolved[0].scope, EnvScope::Project);
    }

    #[test]
    fn resolve_is_sorted_by_key() {
        let mut layers = EnvLayers::new();
        layers.push(EnvScope::Global, layer(&[("Z", "1"), ("A", "2")]));

        let resolved = layers.resolve();
        let keys: Vec<&str> = resolved.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, vec!["A", "Z"]);
    }

    #[test]
    fn missing_files_are_not_an_error() {
        let mut layers = EnvLayers::new();
        layers
            .push_file(EnvScope::Global, Path::new("/definitely/not/here/env"))
            .expect("無いだけなら失敗させない");

        assert!(layers.is_empty());
    }

    #[test]
    fn reports_the_path_when_a_file_is_malformed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("env");
        std::fs::write(&path, "BROKEN\n").expect("書ける");

        let mut layers = EnvLayers::new();
        let err = layers.push_file(EnvScope::Project, &path).unwrap_err();

        assert!(err.to_string().contains("env"), "どのファイルか示す: {err}");
    }

    #[test]
    fn recognises_secret_references() {
        assert_eq!(
            SecretRef::parse("op://Development/myapp/password"),
            Some(SecretRef::OnePassword(
                "op://Development/myapp/password".into()
            ))
        );
        assert_eq!(
            SecretRef::parse("keychain://minato/api-key"),
            Some(SecretRef::Keychain {
                service: "minato".into(),
                account: "api-key".into()
            })
        );
        assert_eq!(
            SecretRef::parse("env://STRIPE_KEY"),
            Some(SecretRef::Env("STRIPE_KEY".into()))
        );
    }

    #[test]
    fn plain_values_are_not_secret_references() {
        assert_eq!(SecretRef::parse("just a value"), None);
        assert_eq!(SecretRef::parse("https://example.com"), None);
        assert_eq!(SecretRef::parse(""), None);
    }

    #[test]
    fn malformed_references_are_not_treated_as_secrets() {
        // 中途半端な参照を「解決できないシークレット」にすると、
        // ただの文字列を渡したい場合に詰む。
        assert_eq!(SecretRef::parse("keychain://only-service"), None);
        assert_eq!(SecretRef::parse("keychain:///no-service"), None);
        assert_eq!(SecretRef::parse("env://"), None);
    }

    #[test]
    fn descriptions_never_contain_the_value() {
        let reference = SecretRef::Keychain {
            service: "minato".into(),
            account: "api-key".into(),
        };
        let description = reference.describe();

        assert!(description.contains("キーチェーン"));
        assert!(description.contains("minato/api-key"));
    }

    #[test]
    fn masks_hide_the_value() {
        assert_eq!(mask(""), "(空)");
        assert_eq!(mask("abc"), "•••", "短い値は先頭も見せない");
        assert_eq!(mask("secret-value"), "se••••••••••");

        // 長さ以外の情報が漏れないこと。
        assert!(!mask("password123").contains("ssword"));
    }

    #[test]
    fn upsert_replaces_in_place_and_keeps_comments() {
        // 利用者が手で書いたファイルを整形し直してはいけない。
        let original = "# 説明\nFOO=old\n\n# 別の説明\nBAR=keep\n";
        let updated = upsert(original, "FOO", "new");

        assert_eq!(updated, "# 説明\nFOO=new\n\n# 別の説明\nBAR=keep\n");
    }

    #[test]
    fn upsert_appends_when_absent() {
        let updated = upsert("FOO=1\n", "BAR", "2");
        assert_eq!(updated, "FOO=1\nBAR=2\n");
    }

    #[test]
    fn upsert_into_empty_file() {
        assert_eq!(upsert("", "FOO", "1"), "FOO=1\n");
    }

    #[test]
    fn upsert_replaces_only_the_first_definition() {
        // 重複定義があっても増やさない。
        let updated = upsert("FOO=1\nFOO=2\n", "FOO", "3");
        assert_eq!(updated.matches("FOO=").count(), 2);
        assert!(updated.starts_with("FOO=3\n"));
    }

    #[test]
    fn upsert_quotes_only_when_needed() {
        assert_eq!(upsert("", "A", "plain"), "A=plain\n");
        assert_eq!(upsert("", "A", "has space"), "A=\"has space\"\n");
        assert_eq!(upsert("", "A", ""), "A=\"\"\n");
        assert_eq!(upsert("", "A", "with\"quote"), "A=\"with\\\"quote\"\n");
    }

    #[test]
    fn quoted_values_survive_a_roundtrip() {
        for value in [
            "plain",
            "has space",
            "",
            "with\"quote",
            "with#hash",
            "line1\nline2",
            "$SHELL",
        ] {
            let text = upsert("", "A", value);
            let parsed = parse(&text).expect("読める");

            assert_eq!(
                parsed.get("A").map(String::as_str),
                Some(value),
                "書いて読んだら元に戻る必要がある: {value:?}"
            );
        }
    }

    #[test]
    fn remove_deletes_the_definition_only() {
        let original = "# 説明\nFOO=1\nBAR=2\n";
        assert_eq!(remove(original, "FOO"), "# 説明\nBAR=2\n");
    }

    #[test]
    fn remove_of_a_missing_key_changes_nothing() {
        assert_eq!(remove("FOO=1\n", "NOPE"), "FOO=1\n");
    }

    #[test]
    fn remove_can_empty_the_file() {
        assert_eq!(remove("FOO=1\n", "FOO"), "");
    }

    #[test]
    fn does_not_match_keys_inside_comments() {
        let original = "# FOO=commented\nFOO=real\n";
        assert_eq!(remove(original, "FOO"), "# FOO=commented\n");
    }

    #[test]
    fn writes_files_only_readable_by_the_owner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("env");

        write_file(&path, "FOO=bar\n").expect("書ける");
        assert_eq!(std::fs::read_to_string(&path).expect("読める"), "FOO=bar\n");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "シークレットの在り処を他人に見せない");
        }
    }

    #[test]
    fn scope_parsing_lists_the_options() {
        assert_eq!("project".parse::<EnvScope>(), Ok(EnvScope::Project));

        let err = "nope".parse::<EnvScope>().unwrap_err();
        assert!(err.contains("global"), "選べるものを示す: {err}");
    }

    #[test]
    fn injected_scope_is_not_writable() {
        // 自動注入はファイルに書けない。書けると実体と食い違う。
        assert!(!EnvScope::Injected.is_writable());
        assert!(EnvScope::Project.is_writable());
    }

    #[test]
    fn paths_follow_the_convention() {
        assert_eq!(
            project_env_path(Path::new("/repo")),
            PathBuf::from("/repo/.minato/env")
        );
        assert_eq!(
            workspace_env_path(Path::new("/repo/wt/feat-1")),
            PathBuf::from("/repo/wt/feat-1/.minato/env.local")
        );
    }
}

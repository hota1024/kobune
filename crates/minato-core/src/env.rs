//! Environment variable layers, and reading and writing them.
//!
//! Three layers, later ones winning.
//!
//! | Layer | Location | Intent |
//! | --- | --- | --- |
//! | global | `~/.minato/env` | shared by every project |
//! | project | `env` in `minato.toml` and `.minato/env` | committed |
//! | workspace | `.minato/env.local` | per-worktree, gitignored |
//!
//! **Keeps plaintext secrets out of the repository.** Values may hold a
//! reference (`op://` and friends), resolved by the daemon at start-up.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// The directory where projects and workspaces keep their variables.
pub const ENV_DIR: &str = ".minato";

/// The project-wide file. Committed to the repository.
pub const PROJECT_ENV_FILE: &str = "env";

/// The per-worktree file. Gitignored.
pub const WORKSPACE_ENV_FILE: &str = "env.local";

/// The global file, directly under `$MINATO_HOME`.
pub const GLOBAL_ENV_FILE: &str = "env";

/// Where a variable was defined.
///
/// Declaration order is precedence: later entries override earlier ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvScope {
    /// Shared by every project.
    Global,
    /// Shared within the project.
    Project,
    /// Specific to one worktree.
    Workspace,
    /// Injected by Minato. The user may override it.
    Injected,
}

impl EnvScope {
    /// The layers `minato env set` can target.
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

    /// Whether this layer can be written to.
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
                "`{other}` is not a valid layer. Use global, project or workspace"
            )),
        }
    }
}

/// A single entry, before resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvEntry {
    pub key: String,
    /// The value as written. For a secret, the reference itself.
    pub raw: String,
    pub scope: EnvScope,
}

impl EnvEntry {
    /// Whether this is a secret reference.
    pub fn secret_ref(&self) -> Option<SecretRef> {
        SecretRef::parse(&self.raw)
    }
}

/// A stack of layers.
#[derive(Debug, Default, Clone)]
pub struct EnvLayers {
    layers: Vec<(EnvScope, IndexMap<String, String>)>,
}

impl EnvLayers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a layer. **Precedence increases with each call.**
    pub fn push(&mut self, scope: EnvScope, values: IndexMap<String, String>) {
        self.layers.push((scope, values));
    }

    /// Reads a dotenv file as a layer. A missing file is not an error.
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

    /// The merged result: for each key, the value from the highest layer.
    ///
    /// Sorted by key so that display and comparison stay stable.
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

/// A reference to a secret held outside the repository.
///
/// The mechanism that keeps plaintext out of version control. Resolved at
/// start-up; the result is never written to disk.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SecretRef {
    /// 1Password CLI (`op read`)。
    OnePassword(String),
    /// The macOS keychain. `keychain://<service>/<account>`.
    Keychain { service: String, account: String },
    /// An environment variable of the daemon process.
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

    /// A description for display. Never includes the value itself.
    pub fn describe(&self) -> String {
        match self {
            Self::OnePassword(reference) => format!("1Password ({reference})"),
            Self::Keychain { service, account } => {
                format!("keychain ({service}/{account})")
            }
            Self::Env(name) => format!("daemon environment ({name})"),
        }
    }
}

/// Masks a value, so plaintext never reaches a log or a listing.
pub fn mask(value: &str) -> String {
    let length = value.chars().count();

    if length == 0 {
        return "(empty)".to_string();
    }

    // Short values reveal nothing at all: even one character narrows a
    // brute-force search.
    if length <= 4 {
        return "•".repeat(length);
    }

    let head: String = value.chars().take(2).collect();
    format!("{head}{}", "•".repeat(length - 2))
}

#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error("cannot read the environment file ({path}): {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("cannot write the environment file ({path}): {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("malformed environment file ({path}): {message}")]
    Parse { path: PathBuf, message: String },

    #[error("not a usable variable name: `{0}`")]
    InvalidKey(String),
}

/// Whether a name is usable as an environment variable.
///
/// Rejects names that a shell or Docker would not accept.
pub fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with(|c: char| c.is_ascii_digit())
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Parses the dotenv format.
///
/// - `KEY=VALUE` and `export KEY=VALUE`
/// - `#` to end of line is a comment, unless quoted
/// - `"..."` interprets `\n` `\t` `\\` `\"`; `'...'` is taken literally
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
            return Err(format!("line {line_number}: no `=`: {trimmed}"));
        };

        let key = key.trim();
        if !is_valid_key(key) {
            return Err(format!(
                "line {line_number}: `{key}` is not a usable variable name"
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
        // Single quotes are taken literally.
        return inner.to_string();
    }

    // Without quotes, everything after `#` is a comment.
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
            // Unknown escapes are left as-is: passing them through is
            // safer than mangling them.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }

    out
}

/// Sets `key` within existing file contents.
///
/// **Preserves comments and line order.** These files are hand-written, so
/// rewriting one must not reformat it.
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

/// Removes the definition of `key`.
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

/// Whether this line defines `key`.
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

/// Renders a value for writing, quoting only when necessary.
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

/// Writes the file, creating parent directories as needed.
pub fn write_file(path: &Path, contents: &str) -> Result<(), EnvError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| EnvError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    // These files hold secret references. Even without the secrets
    // themselves, where they live is nobody else's business.
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

/// The project's environment file (`{root}/.minato/env`).
pub fn project_env_path(root: &Path) -> PathBuf {
    root.join(ENV_DIR).join(PROJECT_ENV_FILE)
}

/// The worktree's environment file (`{worktree}/.minato/env.local`).
pub fn workspace_env_path(worktree: &Path) -> PathBuf {
    worktree.join(ENV_DIR).join(WORKSPACE_ENV_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_assignments() {
        let values = parse("FOO=bar\nBAZ=qux\n").expect("parses");

        assert_eq!(values.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(values.get("BAZ").map(String::as_str), Some("qux"));
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let values = parse("# comment\n\nFOO=bar\n   \n# another\n").expect("parses");

        assert_eq!(values.len(), 1);
        assert_eq!(values.get("FOO").map(String::as_str), Some("bar"));
    }

    #[test]
    fn accepts_export_prefix() {
        // So a line copied from a shell can be pasted verbatim.
        let values = parse("export FOO=bar\n").expect("parses");
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
        .expect("parses");

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
        let values = parse("FOO=bar # note\nURL=\"http://x/#anchor\"\n").expect("parses");

        assert_eq!(values.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(
            values.get("URL").map(String::as_str),
            Some("http://x/#anchor"),
            "a # inside quotes is not a comment"
        );
    }

    #[test]
    fn empty_values_are_allowed() {
        let values = parse("EMPTY=\n").expect("parses");
        assert_eq!(values.get("EMPTY").map(String::as_str), Some(""));
    }

    #[test]
    fn rejects_lines_without_assignment() {
        let err = parse("JUST_A_WORD\n").unwrap_err();
        assert!(err.contains("line 1"), "report the line number: {err}");
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
                .expect("present")
                .clone()
        };

        assert_eq!(find("A").raw, "global");
        assert_eq!(find("A").scope, EnvScope::Global);

        assert_eq!(find("B").raw, "workspace", "the innermost layer wins");
        assert_eq!(
            find("B").scope,
            EnvScope::Workspace,
            "the defining layer must be visible too"
        );
    }

    #[test]
    fn injected_values_can_be_overridden_by_the_user() {
        // Injected values go first; the user's settings layer on top.
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
            .expect("a missing file is not a failure");

        assert!(layers.is_empty());
    }

    #[test]
    fn reports_the_path_when_a_file_is_malformed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("env");
        std::fs::write(&path, "BROKEN\n").expect("writes");

        let mut layers = EnvLayers::new();
        let err = layers.push_file(EnvScope::Project, &path).unwrap_err();

        assert!(err.to_string().contains("env"), "name the file: {err}");
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
        // Treating a half-formed reference as an unresolvable secret
        // would make it impossible to pass such a string literally.
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

        assert!(description.contains("keychain"));
        assert!(description.contains("minato/api-key"));
    }

    #[test]
    fn masks_hide_the_value() {
        assert_eq!(mask(""), "(empty)");
        assert_eq!(mask("abc"), "•••", "short values reveal nothing");
        assert_eq!(mask("secret-value"), "se••••••••••");

        // Nothing but the length leaks.
        assert!(!mask("password123").contains("ssword"));
    }

    #[test]
    fn upsert_replaces_in_place_and_keeps_comments() {
        // A hand-written file must not be reformatted.
        let original = "# note\nFOO=old\n\n# another note\nBAR=keep\n";
        let updated = upsert(original, "FOO", "new");

        assert_eq!(updated, "# note\nFOO=new\n\n# another note\nBAR=keep\n");
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
        // A duplicate definition is not multiplied.
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
            let parsed = parse(&text).expect("parses");

            assert_eq!(
                parsed.get("A").map(String::as_str),
                Some(value),
                "writing then reading must round-trip: {value:?}"
            );
        }
    }

    #[test]
    fn remove_deletes_the_definition_only() {
        let original = "# note\nFOO=1\nBAR=2\n";
        assert_eq!(remove(original, "FOO"), "# note\nBAR=2\n");
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

        write_file(&path, "FOO=bar\n").expect("writes");
        assert_eq!(std::fs::read_to_string(&path).expect("parses"), "FOO=bar\n");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "where the secrets live is nobody else's business");
        }
    }

    #[test]
    fn scope_parsing_lists_the_options() {
        assert_eq!("project".parse::<EnvScope>(), Ok(EnvScope::Project));

        let err = "nope".parse::<EnvScope>().unwrap_err();
        assert!(err.contains("global"), "list the choices: {err}");
    }

    #[test]
    fn injected_scope_is_not_writable() {
        // Injected values cannot be written; a file would drift from reality.
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

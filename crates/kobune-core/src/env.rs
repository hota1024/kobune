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
//!
//! A value may also hold `${ANOTHER_KEY}`, expanded when the layers are
//! resolved. It is what lets a per-worktree URL reach a name the
//! application already reads: `NEXT_PUBLIC_API_URL = "${MINATO_URL_API}"`.

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
    /// A service's own `env` in `minato.toml`.
    ///
    /// Between the project and the worktree: more specific than what the
    /// whole project sets, less than what this worktree does. It has its
    /// own name because `project` would send someone editing
    /// `.minato/env` to change a value that a service overrides — with the
    /// listing having told them they were looking at the right layer.
    Service,
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
            Self::Service => "service",
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
            // `service` is not here on purpose: it is written in
            // `minato.toml` under the service, not through `env set`.
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
    /// The value with `${...}` expanded. For a secret, the reference
    /// itself — that one is resolved later, and only in memory.
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

    /// The merged result: for each key, the value from the highest layer,
    /// with every `${...}` reference expanded.
    ///
    /// Sorted by key so that display and comparison stay stable.
    ///
    /// **Expansion happens here rather than at each caller** so that what
    /// `minato env ls` shows is what the container is given. A listing of
    /// unexpanded values would be a listing of something nothing ever runs
    /// with.
    ///
    /// **Every value or none.** Starting a service with one of them left
    /// as `${...}` is the "set, but broken" this exists to avoid.
    pub fn resolve(&self) -> Result<Vec<EnvEntry>, EnvError> {
        let settled = self.settle();

        match settled.unsettled.into_iter().next() {
            Some(first) => Err(first.error),
            None => Ok(settled.entries),
        }
    }

    /// The same, but with the ones that will not settle marked rather than
    /// refused.
    ///
    /// **A value that settles still settles.** Failing the lot over one
    /// bad reference would leave a listing where nothing can be told apart
    /// — which of thirty values is the one at fault, and which merely look
    /// unexpanded because everything does.
    pub fn settle(&self) -> Settled {
        settle_all(self.merge())
    }

    /// The merged result with every value still as it was written.
    ///
    /// **For saying something about how a value was written.** Expansion
    /// has by then turned `$$NAME` into `$NAME` and pasted one value into
    /// another, so a message built from the settled values would object to
    /// a deliberate escape, and blame the value that referred to the
    /// mistake rather than the one that made it.
    pub fn unexpanded(&self) -> Vec<EnvEntry> {
        self.merge().into_values().collect()
    }

    /// The merge alone, `${...}` still as written.
    fn merge(&self) -> BTreeMap<String, EnvEntry> {
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

        merged
    }

    pub fn is_empty(&self) -> bool {
        self.layers.iter().all(|(_, values)| values.is_empty())
    }
}

/// A merged set with its `${...}` expanded as far as they go.
#[derive(Debug)]
pub struct Settled {
    /// Every key, expanded where it could be, as written where it could
    /// not.
    pub entries: Vec<EnvEntry>,
    /// The ones that could not be, in key order.
    pub unsettled: Vec<Unsettled>,
}

impl Settled {
    /// Why `key` did not settle, if it did not.
    pub fn reason_for(&self, key: &str) -> Option<&EnvError> {
        self.unsettled
            .iter()
            .find(|failure| failure.key == key)
            .map(|failure| &failure.error)
    }
}

/// One value that could not be expanded, and why.
#[derive(Debug)]
pub struct Unsettled {
    pub key: String,
    pub error: EnvError,
}

/// Expands `${...}` throughout the merged set.
///
/// **What a reference resolves to is the value the container will see**,
/// not the one from the layer below the reference. A worktree that
/// overrides `MINATO_URL_API` overrides it for everything built out of it
/// too; the other way round, the override would apply everywhere except
/// where it was being used.
///
/// A key that cannot be expanded keeps its value as written and is
/// recorded. So does one that refers to it: the reason travels, naming the
/// value that actually went wrong rather than the one that trusted it.
fn settle_all(merged: BTreeMap<String, EnvEntry>) -> Settled {
    let mut expanded: BTreeMap<String, String> = BTreeMap::new();
    let mut unsettled = Vec::new();

    for key in merged.keys() {
        if let Err(error) = expand_key(key, &merged, &mut expanded, &mut Vec::new()) {
            unsettled.push(Unsettled {
                key: key.clone(),
                error,
            });
        }
    }

    let entries = merged
        .into_values()
        .map(|entry| EnvEntry {
            raw: expanded
                .remove(&entry.key)
                .unwrap_or_else(|| entry.raw.clone()),
            ..entry
        })
        .collect();

    Settled { entries, unsettled }
}

/// Expands one key, and whatever it refers to, first.
///
/// `chain` is the path taken to get here, so a cycle can be reported as
/// the loop it is rather than as a stack overflow.
fn expand_key(
    key: &str,
    merged: &BTreeMap<String, EnvEntry>,
    expanded: &mut BTreeMap<String, String>,
    chain: &mut Vec<String>,
) -> Result<String, EnvError> {
    if let Some(done) = expanded.get(key) {
        return Ok(done.clone());
    }

    if let Some(start) = chain.iter().position(|seen| seen == key) {
        let mut loop_ = chain[start..].to_vec();
        loop_.push(key.to_string());
        return Err(EnvError::CyclicReference { chain: loop_ });
    }

    let Some(entry) = merged.get(key) else {
        // Only reached through a reference, which checks first.
        return Ok(String::new());
    };

    // A secret is a reference to be resolved at start-up, not a template.
    // Expanding one would mean reading `op://` as text.
    if SecretRef::parse(&entry.raw).is_some() {
        expanded.insert(key.to_string(), entry.raw.clone());
        return Ok(entry.raw.clone());
    }

    chain.push(key.to_string());
    let value = expand_value(key, &entry.raw, merged, expanded, chain)?;
    chain.pop();

    expanded.insert(key.to_string(), value.clone());
    Ok(value)
}

/// The substitution itself.
///
/// - `${NAME}` is a reference
/// - `$$` is a literal `$`
/// - everything else is left alone, `$NAME` included
///
/// **A bare `$NAME` stays literal** because these values have always been
/// passed through as written, and quietly expanding them would change what
/// existing configurations mean. `${...}` is new syntax and can only mean
/// this.
fn expand_value(
    key: &str,
    raw: &str,
    merged: &BTreeMap<String, EnvEntry>,
    expanded: &mut BTreeMap<String, String>,
    chain: &mut Vec<String>,
) -> Result<String, EnvError> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;

    while let Some(dollar) = rest.find('$') {
        out.push_str(&rest[..dollar]);
        let after = &rest[dollar + 1..];

        if let Some(tail) = after.strip_prefix('$') {
            out.push('$');
            rest = tail;
            continue;
        }

        let Some(name) = reference_at(after) else {
            out.push('$');
            rest = after;
            continue;
        };

        let Some(target) = merged.get(name) else {
            return Err(EnvError::UndefinedReference {
                key: key.to_string(),
                name: name.to_string(),
            });
        };

        // A secret resolves in memory when the container starts, so there
        // is nothing here to paste in. Pasting the reference itself would
        // hand the container the string `op://…`, and expanding it would
        // put the secret into `minato env ls` and into any value that
        // gets written out.
        if SecretRef::parse(&target.raw).is_some() {
            return Err(EnvError::SecretReference {
                key: key.to_string(),
                name: name.to_string(),
            });
        }

        out.push_str(&expand_key(name, merged, expanded, chain)?);
        rest = &after[name.len() + 2..];
    }

    out.push_str(rest);
    Ok(out)
}

/// The name in `{NAME}...`, when there is one.
///
/// Anything that is not a usable variable name is not a reference:
/// `${VAR:-default}` is shell syntax someone meant to pass through, and
/// refusing it would leave no way to write it.
fn reference_at(text: &str) -> Option<&str> {
    let inner = text.strip_prefix('{')?;
    let end = inner.find('}')?;
    let name = &inner[..end];

    is_valid_key(name).then_some(name)
}

/// The names written as `$NAME`, which is not a reference.
///
/// **So the trap can be answered instead of sprung.** `$MINATO_CACHE_DIR`
/// is what anyone reaches for first, and a value passed through as written
/// fails somewhere else entirely — a directory called `$MINATO_CACHE_DIR`
/// appears in the worktree and nothing says why. The caller warns when one
/// of these names is a variable that exists.
pub fn bare_references(value: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = value;

    while let Some(dollar) = rest.find('$') {
        let after = &rest[dollar + 1..];

        if let Some(tail) = after.strip_prefix('$') {
            rest = tail;
            continue;
        }

        let name: &str = after
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .next()
            .unwrap_or_default();

        if is_valid_key(name) {
            names.push(name);
        }

        rest = &after[name.len()..];
    }

    names
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

    #[error("{key} refers to ${{{name}}}, which nothing sets")]
    UndefinedReference { key: String, name: String },

    #[error(
        "{key} refers to ${{{name}}}, which is a secret reference. Minato resolves those when the container starts, so one cannot be built into another value"
    )]
    SecretReference { key: String, name: String },

    #[error("these refer to each other and cannot be settled: {}", chain.join(" -> "))]
    CyclicReference { chain: Vec<String> },
}

/// Whether a name is usable as an environment variable.
///
/// Rejects names that a shell or Docker would not accept.
pub fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with(|c: char| c.is_ascii_digit())
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The first line of a file Minato writes.
///
/// **The marker that says this file may be replaced.** Anything without it
/// is somebody's own work, and gets left alone.
pub const GENERATED_MARKER: &str = "# generated by minato";

/// Renders settled values as a dotenv file, for the tools that read one.
///
/// `note` says which service and worktree it came from, so a file found
/// later says what wrote it and why.
///
/// **Secret references are left out**, named but not written. A resolved
/// secret lives in the daemon's memory and never touches disk; a file that
/// broke that would break it everywhere, since this one is read by
/// whatever the service hands it to.
pub fn render(entries: &[EnvEntry], note: &str) -> String {
    let mut out = format!("{GENERATED_MARKER} — do not edit, and do not commit\n# {note}\n");

    let secrets: Vec<&str> = entries
        .iter()
        .filter(|entry| entry.secret_ref().is_some())
        .map(|entry| entry.key.as_str())
        .collect();

    if !secrets.is_empty() {
        out.push_str(&format!(
            "# resolved in memory when the container starts, so not written here: {}\n",
            secrets.join(", ")
        ));
    }

    out.push('\n');

    for entry in entries.iter().filter(|entry| entry.secret_ref().is_none()) {
        out.push_str(&format!("{}={}\n", entry.key, quote(&entry.raw)));
    }

    out
}

/// Whether this text is a file Minato wrote.
pub fn is_generated(text: &str) -> bool {
    text.starts_with(GENERATED_MARKER)
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

        let resolved = layers.resolve().expect("resolves");
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

        let resolved = layers.resolve().expect("resolves");
        assert_eq!(resolved[0].raw, "custom");
        assert_eq!(resolved[0].scope, EnvScope::Project);
    }

    #[test]
    fn resolve_is_sorted_by_key() {
        let mut layers = EnvLayers::new();
        layers.push(EnvScope::Global, layer(&[("Z", "1"), ("A", "2")]));

        let resolved = layers.resolve().expect("resolves");
        let keys: Vec<&str> = resolved.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, vec!["A", "Z"]);
    }

    /// The value of `key` once the layers are settled.
    fn settled(layers: &EnvLayers, key: &str) -> String {
        layers
            .resolve()
            .expect("resolves")
            .into_iter()
            .find(|entry| entry.key == key)
            .unwrap_or_else(|| panic!("{key} is present"))
            .raw
    }

    #[test]
    fn expands_a_reference_to_another_variable() {
        // The point of the whole thing: a URL that differs per worktree,
        // reaching the name the application already reads.
        let mut layers = EnvLayers::new();
        layers.push(
            EnvScope::Injected,
            layer(&[("MINATO_URL_API", "https://api.feat-1.myapp.localhost")]),
        );
        layers.push(
            EnvScope::Service,
            layer(&[
                ("NEXT_PUBLIC_API_URL", "${MINATO_URL_API}"),
                ("FILE_BASE_URL", "${MINATO_URL_API}/dev/r2"),
            ]),
        );

        assert_eq!(
            settled(&layers, "NEXT_PUBLIC_API_URL"),
            "https://api.feat-1.myapp.localhost"
        );
        assert_eq!(
            settled(&layers, "FILE_BASE_URL"),
            "https://api.feat-1.myapp.localhost/dev/r2",
            "a reference is a part of the value, not the whole of it"
        );
    }

    #[test]
    fn a_reference_sees_the_value_that_won() {
        // Not the layer below the reference: an override that applied
        // everywhere except where it was being used would be a trap.
        let mut layers = EnvLayers::new();
        layers.push(EnvScope::Injected, layer(&[("MINATO_URL_API", "auto")]));
        layers.push(
            EnvScope::Service,
            layer(&[("API_URL", "${MINATO_URL_API}")]),
        );
        layers.push(
            EnvScope::Workspace,
            layer(&[("MINATO_URL_API", "http://localhost:8080")]),
        );

        assert_eq!(settled(&layers, "API_URL"), "http://localhost:8080");
    }

    #[test]
    fn references_can_be_chained() {
        let mut layers = EnvLayers::new();
        layers.push(
            EnvScope::Project,
            layer(&[("A", "one"), ("B", "${A}-two"), ("C", "${B}-three")]),
        );

        assert_eq!(settled(&layers, "C"), "one-two-three");
    }

    #[test]
    fn an_undefined_reference_is_an_error() {
        // Not an empty string. "Set, but empty" is the state that is
        // hardest to trace back to its cause.
        let mut layers = EnvLayers::new();
        layers.push(EnvScope::Project, layer(&[("API_URL", "${NOT_SET}/v1")]));

        let err = layers.resolve().unwrap_err().to_string();
        assert!(err.contains("API_URL"), "name the value: {err}");
        assert!(err.contains("NOT_SET"), "name the reference: {err}");
    }

    #[test]
    fn settling_keeps_the_values_that_settle() {
        // One bad reference used to take every other value with it, which
        // left a listing where nothing could be told apart.
        let mut layers = EnvLayers::new();
        layers.push(
            EnvScope::Project,
            layer(&[
                ("BASE", "https://api"),
                ("GOOD", "${BASE}/v1"),
                ("BAD", "${NOWHERE}"),
            ]),
        );

        let settled = layers.settle();
        let value = |key: &str| {
            settled
                .entries
                .iter()
                .find(|entry| entry.key == key)
                .expect("present")
                .raw
                .clone()
        };

        assert_eq!(value("GOOD"), "https://api/v1", "this one was fine");
        assert_eq!(value("BAD"), "${NOWHERE}", "and this one is as written");

        assert!(settled.reason_for("GOOD").is_none());
        assert!(settled.reason_for("BAD").is_some());
    }

    #[test]
    fn a_value_built_on_one_that_will_not_settle_says_so_too() {
        // Both are unusable, and the reason names the one that actually
        // went wrong rather than the one that trusted it.
        let mut layers = EnvLayers::new();
        layers.push(
            EnvScope::Project,
            layer(&[("BAD", "${NOWHERE}"), ("DERIVED", "${BAD}/x")]),
        );

        let settled = layers.settle();

        assert_eq!(settled.unsettled.len(), 2, "{:?}", settled.unsettled);
        assert!(
            settled
                .reason_for("DERIVED")
                .is_some_and(|err| err.to_string().contains("NOWHERE")),
            "point at the root: {:?}",
            settled.reason_for("DERIVED")
        );
    }

    #[test]
    fn starting_still_wants_all_of_them() {
        // A container given one value still holding `${...}` is the "set,
        // but broken" the whole thing exists to avoid.
        let mut layers = EnvLayers::new();
        layers.push(
            EnvScope::Project,
            layer(&[("GOOD", "1"), ("BAD", "${NOWHERE}")]),
        );

        assert!(layers.resolve().is_err(), "every value or none");
    }

    #[test]
    fn a_cycle_is_reported_as_the_loop_it_is() {
        let mut layers = EnvLayers::new();
        layers.push(EnvScope::Project, layer(&[("A", "${B}"), ("B", "${A}")]));

        let err = layers.resolve().unwrap_err().to_string();
        assert!(err.contains("A -> B -> A"), "show the loop: {err}");
    }

    #[test]
    fn a_secret_cannot_be_built_into_another_value() {
        // Expanding one would put the secret in `minato env ls` and in
        // anything written out of it; pasting the reference in would hand
        // the container the string `op://…`.
        let mut layers = EnvLayers::new();
        layers.push(
            EnvScope::Project,
            layer(&[
                ("PASSWORD", "op://Development/myapp/password"),
                ("DATABASE_URL", "postgres://user:${PASSWORD}@db/app"),
            ]),
        );

        let err = layers.resolve().unwrap_err().to_string();
        assert!(err.contains("PASSWORD"), "name the secret: {err}");
        assert!(err.contains("secret"), "say why: {err}");
    }

    #[test]
    fn what_was_written_survives_for_anything_that_has_to_talk_about_it() {
        // A warning about how a value was written cannot be read off the
        // settled ones: `$$A` has become `$A` by then, and `B` is carrying
        // a copy of the mistake `C` made.
        let mut layers = EnvLayers::new();
        layers.push(
            EnvScope::Project,
            layer(&[("A", "1"), ("B", "$${A}"), ("C", "$A"), ("D", "${C}")]),
        );

        let written = layers.unexpanded();
        let find = |key: &str| {
            written
                .iter()
                .find(|entry| entry.key == key)
                .expect("present")
                .raw
                .clone()
        };

        assert_eq!(find("B"), "$${A}", "the escape is still an escape");
        assert_eq!(find("D"), "${C}", "the mistake stays with C");
        assert!(bare_references(&find("B")).is_empty());
        assert!(bare_references(&find("D")).is_empty());
        assert_eq!(bare_references(&find("C")), vec!["A"]);
    }

    #[test]
    fn a_secret_reference_is_left_alone() {
        let mut layers = EnvLayers::new();
        layers.push(
            EnvScope::Project,
            layer(&[("PASSWORD", "op://Development/myapp/password")]),
        );

        assert_eq!(
            settled(&layers, "PASSWORD"),
            "op://Development/myapp/password"
        );
    }

    #[test]
    fn a_bare_dollar_is_left_as_written() {
        // These values have always been passed through verbatim. Expanding
        // them now would change what existing configurations mean.
        let mut layers = EnvLayers::new();
        layers.push(
            EnvScope::Project,
            layer(&[
                ("MINATO_CACHE_DIR", "/var/cache/minato"),
                ("STORE", "$MINATO_CACHE_DIR/pnpm"),
                ("COST", "$5"),
            ]),
        );

        assert_eq!(settled(&layers, "STORE"), "$MINATO_CACHE_DIR/pnpm");
        assert_eq!(settled(&layers, "COST"), "$5");
    }

    #[test]
    fn double_dollar_escapes() {
        let mut layers = EnvLayers::new();
        layers.push(
            EnvScope::Project,
            layer(&[("A", "one"), ("LITERAL", "$${A} costs $$5")]),
        );

        assert_eq!(settled(&layers, "LITERAL"), "${A} costs $5");
    }

    #[test]
    fn what_is_not_a_variable_name_is_not_a_reference() {
        // Shell syntax someone meant to pass to a shell. Refusing it would
        // leave no way to write it.
        let mut layers = EnvLayers::new();
        layers.push(
            EnvScope::Project,
            layer(&[
                ("SHELL_DEFAULT", "${PORT:-3000}"),
                ("JQ", "${.name}"),
                ("UNCLOSED", "${OPEN"),
            ]),
        );

        assert_eq!(settled(&layers, "SHELL_DEFAULT"), "${PORT:-3000}");
        assert_eq!(settled(&layers, "JQ"), "${.name}");
        assert_eq!(settled(&layers, "UNCLOSED"), "${OPEN");
    }

    #[test]
    fn bare_references_are_found_so_they_can_be_warned_about() {
        assert_eq!(
            bare_references("$MINATO_CACHE_DIR/pnpm"),
            vec!["MINATO_CACHE_DIR"]
        );
        assert_eq!(bare_references("$A:$B"), vec!["A", "B"]);

        assert!(
            bare_references("${A} $$B $5 $").is_empty(),
            "a reference, an escape, and two things that are neither"
        );
    }

    #[test]
    fn renders_a_file_the_tools_that_read_one_can_use() {
        let mut layers = EnvLayers::new();
        layers.push(
            EnvScope::Injected,
            layer(&[("MINATO_URL_API", "https://api")]),
        );
        layers.push(
            EnvScope::Service,
            layer(&[("API_URL", "${MINATO_URL_API}"), ("NOTE", "has space")]),
        );

        let rendered = render(&layers.resolve().expect("resolves"), "service: api");

        assert!(is_generated(&rendered), "says who wrote it: {rendered}");
        assert!(rendered.contains("# service: api"));
        assert!(rendered.contains("\nAPI_URL=https://api\n"), "{rendered}");
        assert!(
            rendered.contains("\nNOTE=\"has space\"\n"),
            "quoted the way the parser expects: {rendered}"
        );

        let read_back = parse(&rendered).expect("what is written can be read");
        assert_eq!(
            read_back.get("API_URL").map(String::as_str),
            Some("https://api")
        );
    }

    #[test]
    fn a_secret_is_named_but_not_written() {
        // A resolved secret never touches disk. A file that broke that
        // would break it everywhere, since this one is handed on.
        let mut layers = EnvLayers::new();
        layers.push(
            EnvScope::Project,
            layer(&[
                ("API_KEY", "op://Development/myapp/key"),
                ("LOG_LEVEL", "debug"),
            ]),
        );

        let rendered = render(&layers.resolve().expect("resolves"), "service: api");

        assert!(
            !rendered.contains("op://"),
            "no reference either: {rendered}"
        );
        assert!(
            rendered.contains("# resolved in memory"),
            "say where it went: {rendered}"
        );
        assert!(
            rendered.contains("API_KEY") && !rendered.contains("\nAPI_KEY="),
            "named, not written: {rendered}"
        );
        assert!(rendered.contains("\nLOG_LEVEL=debug\n"));
    }

    #[test]
    fn only_what_minato_wrote_carries_the_marker() {
        assert!(is_generated(&render(&[], "service: api")));
        assert!(!is_generated("FOO=bar\n"));
        assert!(!is_generated("# my own notes\nFOO=bar\n"));
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
            assert_eq!(
                mode & 0o777,
                0o600,
                "where the secrets live is nobody else's business"
            );
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

//! The schema and validation of `kobune.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::env;
use crate::error::{Error, Result};
use crate::naming;

/// The name of the configuration file.
pub const CONFIG_FILE: &str = "kobune.toml";

/// The machine-wide configuration, directly under `$KOBUNE_HOME`.
///
/// For what is true of this computer rather than of the project: which
/// container runtime is installed on it, most of all.
pub const GLOBAL_CONFIG_FILE: &str = "config.toml";

/// The per-clone override, beside [`CONFIG_FILE`]. Gitignored.
pub const LOCAL_CONFIG_FILE: &str = "kobune.local.toml";

/// Where the worktree's source is mounted inside the container.
pub const MOUNT_TARGET: &str = "/workspace";

/// Where a service can write things worth keeping but not committing.
///
/// **Deliberately outside [`MOUNT_TARGET`].** Anywhere under the worktree
/// is the host's disk, inside the repository — which is how a package
/// store ends up as a gigabyte of untracked files in someone's checkout.
/// Handed to every service as `KOBUNE_CACHE_DIR`.
pub const CACHE_TARGET: &str = "/var/cache/kobune";

/// The name of the volume behind [`CACHE_TARGET`].
///
/// **Deliberately not a valid volume name.** [`naming::is_valid_label`]
/// allows only lowercase letters, digits and hyphens, and `VolumeMount`
/// checks every declared name against it — so no `volumes` entry can reach
/// this one however it is spelled.
///
/// Calling it `cache` would have collided with anyone already using that
/// name: their storage would quietly have become the cache, which is the
/// sort of migration nobody notices until the data looks gone.
pub const CACHE_VOLUME: &str = "_cache";

/// Where Kobune's own CA certificate is mounted, read-only.
///
/// **The browser trusts it and a container does not.** `kobune setup`
/// puts the CA in the host's keychain, which is what makes
/// `https://api.myapp.localhost` load without a warning — but a container
/// carries its own trust store, so the same URL called from inside one
/// fails to verify. Mounting the certificate is what lets a service call
/// the URL it was handed instead of turning verification off.
///
/// Not under [`MOUNT_TARGET`]: it is not the worktree's, and a file that
/// appeared in the repository would be committed by somebody. Handed to
/// every service as `KOBUNE_CA_FILE`.
pub const CA_TARGET: &str = "/etc/kobune/ca.crt";

/// The paths Kobune mounts itself, and what to say when one is taken.
///
/// A table rather than a branch each: the next `KOBUNE_*` path should cost
/// a line here, not another eight-line copy of the same check.
const RESERVED_MOUNTS: [(&str, &str, &str); 2] = [
    (
        CACHE_TARGET,
        "KOBUNE_CACHE_DIR",
        "Write under $KOBUNE_CACHE_DIR, or mount yours somewhere else",
    ),
    (CA_TARGET, "KOBUNE_CA_FILE", "Mount yours somewhere else"),
];

/// The default when `idle_timeout` is omitted.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Where a piece of configuration was read from.
///
/// Declaration order is precedence: later layers override earlier ones,
/// the same way [`crate::EnvScope`] works.
///
/// **[`Self::Local`] is not [`crate::EnvScope::Workspace`].** The
/// environment's innermost layer is per-worktree, because `.kobune/env.local`
/// is written into each one. This one is per-*clone*: `kobune.local.toml`
/// lives in the main worktree and every worktree of that checkout reads it.
/// Worktrees of one repository share a container runtime whether they like
/// it or not, so there is nothing here for a worktree to differ about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigLayer {
    /// `config.toml` under `$KOBUNE_HOME`. This machine, every project.
    Global,
    /// `kobune.toml` at the repository root. Committed, so everyone gets it.
    Project,
    /// `kobune.local.toml` beside it. Gitignored, so only this clone.
    Local,
}

impl ConfigLayer {
    /// The layers in the order they are applied.
    pub const ORDER: [ConfigLayer; 3] = [Self::Global, Self::Project, Self::Local];

    pub fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
            Self::Local => "local",
        }
    }

    /// The file this layer is read from.
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Global => GLOBAL_CONFIG_FILE,
            Self::Project => CONFIG_FILE,
            Self::Local => LOCAL_CONFIG_FILE,
        }
    }
}

/// One layer, and whether there was a file to read.
#[derive(Debug, Clone)]
pub struct ConfigSource {
    pub layer: ConfigLayer,
    pub path: PathBuf,
    /// Whether the file was there.
    ///
    /// **Absent layers are reported rather than dropped.** "My override is
    /// not applying" is nearly always "the file is not where I think it
    /// is", and a listing that showed only what was read could not answer
    /// that question.
    pub loaded: bool,
}

/// Which layer settled one key, and what it beat.
#[derive(Debug, Clone)]
pub struct ConfigOrigin {
    /// The layer whose value won.
    pub layer: ConfigLayer,
    /// The layers it overrode, in the order they were applied. Empty when
    /// only one layer had an opinion.
    pub overridden: Vec<ConfigLayer>,
    /// The winning value, rendered for display.
    pub value: String,
}

/// What the layers are, and what they came to.
///
/// **Separate from the configuration itself, and obtainable without one.**
/// The moment this is most worth asking for is the moment the merge does
/// not load at all, so [`KobuneConfig::inspect`] reports that as
/// [`Self::problem`] rather than as an error — a command that explains a
/// configuration is no use if a broken configuration is what stops it.
#[derive(Debug, Clone)]
pub struct ConfigReport {
    /// Every layer, in the order applied, whether or not it was read.
    pub sources: Vec<ConfigSource>,

    /// Which layer settled each key, by dotted path (`runtime.default`).
    ///
    /// Filled in either way: it comes from the merge, which happens before
    /// anything asks whether the result is a configuration.
    pub origins: BTreeMap<String, ConfigOrigin>,

    /// Why the merge is not a usable configuration, when it is not.
    pub problem: Option<String>,
}

impl ConfigReport {
    /// The files that were actually read.
    pub fn loaded(&self) -> impl Iterator<Item = &ConfigSource> {
        self.sources.iter().filter(|source| source.loaded)
    }

    /// The keys some layer took from another.
    ///
    /// What `kobune config show` leads with: a key only one layer sets is
    /// not a question anybody has.
    pub fn overrides(&self) -> impl Iterator<Item = (&String, &ConfigOrigin)> {
        self.origins
            .iter()
            .filter(|(_, origin)| !origin.overridden.is_empty())
    }
}

/// The three files, merged, before anyone asks what they mean.
struct Layered {
    sources: Vec<ConfigSource>,
    origins: BTreeMap<String, ConfigOrigin>,
    merged: toml::Table,
    /// Where the search for `kobune.toml` began.
    ///
    /// Kept for the error when it found none, which names the directory
    /// rather than the file — there is no file to name.
    start: PathBuf,
    /// The one loaded layer's text, when exactly one was loaded.
    ///
    /// For its span. See [`Self::settle`].
    only_text: Option<String>,
    /// Whether a `kobune.toml` was found at all.
    ///
    /// **Not an error until something asks what the layers mean.** The
    /// three paths are still worth reporting when the project file is the
    /// missing one: "it is not where you think" is the case `config show`
    /// exists for, and it cannot answer it by failing the same way every
    /// other command already did.
    found_project: bool,
}

impl Layered {
    /// The files that were read, for a message that has to name them.
    fn files(&self) -> Vec<PathBuf> {
        self.sources
            .iter()
            .filter(|source| source.loaded)
            .map(|source| source.path.clone())
            .collect()
    }

    /// The merged document as a configuration, validated.
    ///
    /// **Consumes the merge.** Every daemon operation resolves a
    /// configuration, so a clone here would be one copy of the whole
    /// document per `up`, `status` or `exec`. [`KobuneConfig::inspect`]
    /// takes the layers off it first and hands the rest over.
    fn settle(self) -> Result<KobuneConfig> {
        if !self.found_project {
            return Err(Error::ConfigNotFound(self.start));
        }

        let files = self.files();

        let config: KobuneConfig =
            toml::Value::Table(self.merged)
                .try_into()
                .map_err(|source: toml::de::Error| {
                    // **A merged document has no line numbers**, so this error
                    // says which files it came from and nothing about where.
                    // With one layer there is a file to point at, and that is
                    // very nearly every project: re-reading it through
                    // `from_str` costs a parse on a path that has already
                    // failed, and buys back `at line 7, column 1` with the
                    // offending line under a caret.
                    //
                    // The configuration itself still comes from the merge, so
                    // there is one answer to what a project is — only the
                    // message improves.
                    match &self.only_text {
                        Some(text) => match toml::from_str::<KobuneConfig>(text) {
                            Err(spanned) => Error::ConfigParse {
                                path: files.first().cloned().unwrap_or_default(),
                                source: spanned,
                            },
                            // It parsed alone but not merged, which cannot
                            // happen with one layer. Report what actually
                            // failed rather than inventing a success.
                            Ok(_) => Error::ConfigMerged {
                                files: files.clone(),
                                source: Box::new(source),
                            },
                        },
                        None => Error::ConfigMerged {
                            files: files.clone(),
                            source: Box::new(source),
                        },
                    }
                })?;

        // **Validated once, against the merged result**, which is the only
        // thing that has to make sense: a layer setting `expose = true` is
        // fine on its own and wrong beside a layer that removed the port.
        //
        // Which is also why the files are named. Without them the message
        // describes a line that appears in none of them.
        config.validate().map_err(|err| match err {
            Error::ConfigInvalid(message) if files.len() > 1 => Error::ConfigInvalid(format!(
                "{message}. This is the merge of {}",
                describe_files(&files)
            )),
            other => other,
        })?;

        Ok(config)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KobuneConfig {
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

    /// The URL suffix. Defaults to `{name}.localhost`.
    #[serde(default)]
    pub domain: Option<String>,

    /// Files to copy into a new worktree, relative to the repository root.
    ///
    /// For what git does not carry: an untracked but required `.env`.
    #[serde(default)]
    pub carry: Vec<String>,
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

/// Whether a `volumes` source asks for per-worktree storage.
///
/// **A host path is never scoped**, however it is spelled: there is nothing
/// to namespace about a directory the user already owns, and `@` is legal
/// in a path. Without this, a real directory called `certs@workspace` on a
/// shared service would be refused with a message describing something
/// nobody wrote.
///
/// Mirrors how `VolumeMount::parse` decides the same thing, in
/// `kobune-runtime`. It cannot be called from here — the runtime sits above
/// this crate — so the one rule the two share is the prefix test, kept
/// deliberately trivial so it can be read side by side.
fn is_workspace_scoped(source: &str) -> bool {
    let is_host_path =
        source.starts_with('/') || source.starts_with('.') || source.starts_with('~');

    !is_host_path && source.ends_with("@workspace")
}

/// An `env_file` entry as the file it names.
///
/// **Drops `.` segments**, so that two spellings of one file compare as
/// one. Refusing `.kobune/env.local` while accepting `./.kobune/env.local`
/// would not be much of a refusal, and two services claiming the same file
/// under different spellings would go on overwriting each other.
fn env_file_path(entry: &str) -> PathBuf {
    Path::new(entry)
        .components()
        .filter(|part| !matches!(part, std::path::Component::CurDir))
        .collect()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    /// A prebuilt image. Mutually exclusive with `build`.
    #[serde(default)]
    pub image: Option<String>,

    /// The build context, relative to the repository root. Mutually
    /// exclusive with `image`.
    #[serde(default)]
    pub build: Option<String>,

    /// The Dockerfile, relative to the repository root.
    ///
    /// Defaults to `Dockerfile` inside the build context. Naming it
    /// separately is what lets several services build different images
    /// from one context.
    #[serde(default)]
    pub dockerfile: Option<String>,

    /// `--build-arg` values.
    ///
    /// A `BTreeMap` so the order is stable: these feed the fingerprint that
    /// decides whether a rebuild is needed, and a map that reordered itself
    /// would rebuild at random.
    #[serde(default)]
    pub build_args: BTreeMap<String, String>,

    /// The port the service listens on inside the container.
    #[serde(default)]
    pub port: Option<u16>,

    /// The start command. Falls back to the image's CMD.
    #[serde(default)]
    pub command: Option<String>,

    /// What to run once, before the service first starts.
    ///
    /// **Not once per container.** A stopped container is recreated by the
    /// next `up`, so tying this to container creation would run it on
    /// every `down`/`up` — which is the thing it exists to stop. It is
    /// remembered against the worktree, and runs again when this changes.
    #[serde(default)]
    pub setup: Option<String>,

    /// The working directory inside the container. Defaults to [`MOUNT_TARGET`].
    #[serde(default)]
    pub workdir: Option<String>,

    /// Give the process a terminal, and keep its stdin open.
    ///
    /// **What a program looks for before it draws anything.** Turborepo,
    /// Vitest and the rest ask whether stdout is a terminal, and settle for
    /// plain scrolling text when it is not — which is what a container
    /// without this gives them. With it, `kobune logs -f <service>` becomes
    /// that terminal: colour comes through and keys reach the program.
    ///
    /// Off by default, because a terminal changes what the logs *are*: the
    /// two output streams become one, so nothing separates stderr from
    /// stdout any more, and lines arrive ending `\r\n`. A pipeline that
    /// greps `kobune logs` should not have that happen to it unasked.
    #[serde(default)]
    pub tty: bool,

    /// How readiness is determined. Used by scale-to-zero.
    #[serde(default)]
    pub health: Option<HealthCheck>,

    /// How long without traffic before the service is stopped.
    #[serde(default, with = "humantime_serde::option")]
    pub idle_timeout: Option<Duration>,

    #[serde(default)]
    pub env: BTreeMap<String, String>,

    /// Where to write this service's settled environment, relative to the
    /// worktree.
    ///
    /// **For the tools that read a file rather than their process's
    /// environment.** `wrangler dev --env-file`, dotenvx and Vite all do,
    /// and a variable Kobune injects cannot reach them otherwise.
    ///
    /// Secrets are left out of it. Written before the service starts, and
    /// again whenever it is started.
    #[serde(default)]
    pub env_file: Option<String>,

    #[serde(default)]
    pub depends_on: Vec<String>,

    #[serde(default)]
    pub scope: ServiceScope,

    /// Whether to publish a URL. Defaults to true when `port` is set.
    #[serde(default)]
    pub expose: Option<bool>,

    #[serde(default)]
    pub volumes: Vec<String>,
}

impl ServiceConfig {
    /// The effective value of `expose`.
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

/// Whether a service gets one instance per worktree or one per project.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceScope {
    /// One independent instance per worktree.
    #[default]
    Workspace,
    /// A single instance shared by every worktree of the project.
    Project,
}

/// How readiness is determined.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub enum HealthCheck {
    /// Ready when the response is 2xx or 3xx.
    Http(String),
    /// Ready when a TCP connection can be established.
    Tcp(String),
    /// Runs inside the container; ready on exit code 0.
    Cmd(String),
}

impl FromStr for HealthCheck {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix("cmd:") {
            let cmd = rest.trim();
            if cmd.is_empty() {
                return Err("nothing follows `cmd:`".to_string());
            }
            return Ok(Self::Cmd(cmd.to_string()));
        }
        if s.starts_with("http://") || s.starts_with("https://") {
            return Ok(Self::Http(s.to_string()));
        }
        if let Some(rest) = s.strip_prefix("tcp://") {
            if rest.is_empty() {
                return Err("nothing follows `tcp://`".to_string());
            }
            return Ok(Self::Tcp(rest.to_string()));
        }
        Err(format!(
            "cannot parse `{s}` as a health check. \
             Use `http://...`, `https://...`, `tcp://host:port` or `cmd:...`"
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

/// Searches upwards from `start` for `kobune.toml`.
fn find_file(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(current) = dir {
        let candidate = current.join(CONFIG_FILE);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = current.parent();
    }
    None
}

/// Reads one layer. A file that is not there is not an error.
///
/// The text comes back with the table so that a failure on a single-layer
/// merge can be re-reported against the file it came from — see
/// [`Layered::settle`].
fn read_layer(path: &Path) -> Result<Option<(toml::Table, String)>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error::ConfigRead {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    match toml::from_str(&text) {
        Ok(table) => Ok(Some((table, text))),
        Err(source) => Err(Error::ConfigParse {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Names the files a merged configuration came from.
pub(crate) fn describe_files(files: &[PathBuf]) -> String {
    files
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(" + ")
}

/// A value as it should read in a listing.
///
/// Strings lose their quotes — the column is already labelled as a value,
/// and `"apple"` in a table of values reads as a quoting mistake.
fn render_value(value: &toml::Value) -> String {
    match value {
        toml::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Merges `overlay` onto `base`, recording what `layer` settled.
///
/// **Tables merge, everything else replaces.** A table is a namespace, so
/// merging one is what lets a layer say `[runtime] default` without
/// restating every service below it. An array is a single value: appending
/// would leave no way to remove an entry, and the arrays here (`volumes`,
/// `depends_on`, `carry`) are short enough to restate.
///
/// Done on [`toml::Value`] rather than on `KobuneConfig` itself, for two
/// reasons that both come from the schema. `[project] name` is required, so
/// an overlay could not be deserialized on its own — a file that only sets
/// `[runtime] default` would have to restate the project's name. And most
/// of [`ServiceConfig`] carries `#[serde(default)]`, so "not mentioned" and
/// "set to the default" arrive indistinguishable: an overlay silent about
/// `tty` would deserialize identically to one saying `tty = false`, and
/// would turn a service's terminal off behind its owner's back.
fn merge_into(
    base: &mut toml::Table,
    overlay: toml::Table,
    layer: ConfigLayer,
    prefix: &str,
    origins: &mut BTreeMap<String, ConfigOrigin>,
) {
    for (key, incoming) in overlay {
        let path = match prefix.is_empty() {
            true => key.clone(),
            false => format!("{prefix}.{key}"),
        };

        match (base.get_mut(&key), incoming) {
            (Some(toml::Value::Table(existing)), toml::Value::Table(incoming)) => {
                merge_into(existing, incoming, layer, &path, origins);
            }

            // A table no lower layer had, or one displacing a scalar.
            // Walked all the same, so that a service introduced here is
            // attributed key by key like any other.
            (_, toml::Value::Table(incoming)) => {
                // At `path` as well as under it: what a table displaces
                // here is a scalar, whose own origin is the entry at
                // `path`, and nothing below will overwrite it.
                origins.remove(&path);
                forget(origins, &path);

                let mut fresh = toml::Table::new();
                merge_into(&mut fresh, incoming, layer, &path, origins);
                base.insert(key, toml::Value::Table(fresh));
            }

            (_, incoming) => {
                forget(origins, &path);
                record_origin(origins, path, layer, &incoming);
                base.insert(key, incoming);
            }
        }
    }
}

/// Drops what was recorded at `path`, and anything that was under it.
///
/// **A layer may change a key's shape, not just its value.** A scalar
/// replacing a table takes every leaf beneath it out of the document, and
/// a table replacing a scalar takes that scalar; origins left behind would
/// have `config show` list keys the merge does not contain — in the one
/// command whose job is to say what it does.
///
/// Only what is *under* `path`. The entry at `path` itself belongs to
/// whoever is replacing it: a scalar hands it to [`record_origin`], which
/// needs it to say what the new value overrode, and a table has to drop it
/// outright because nothing below will.
fn forget(origins: &mut BTreeMap<String, ConfigOrigin>, path: &str) {
    let under = format!("{path}.");
    origins.retain(|key, _| !key.starts_with(&under));
}

/// Notes that `layer` settled `path`, keeping whatever it displaced.
fn record_origin(
    origins: &mut BTreeMap<String, ConfigOrigin>,
    path: String,
    layer: ConfigLayer,
    value: &toml::Value,
) {
    let overridden = match origins.remove(&path) {
        Some(previous) => {
            let mut chain = previous.overridden;
            chain.push(previous.layer);
            chain
        }
        None => Vec::new(),
    };

    origins.insert(
        path,
        ConfigOrigin {
            layer,
            overridden,
            value: render_value(value),
        },
    );
}

/// Reads the three layers and merges them.
///
/// `start` is where the search for `kobune.toml` begins, `main_root` is the
/// repository's main worktree, and `home` is `$KOBUNE_HOME`.
///
/// **The local layer is anchored on `main_root`, not beside whichever
/// `kobune.toml` was found.** `kobune.toml` is committed, so every worktree
/// holds a copy and the search finds that one; `kobune.local.toml` is
/// gitignored, so `git worktree add` never brings it along. Looking for it
/// beside the file that was found would mean an override that applied in
/// the main checkout and silently nowhere else — which is the one failure
/// this is most likely to be blamed for.
fn layer_files(start: &Path, main_root: &Path, home: &Path) -> Result<Layered> {
    // The main worktree is tried second so that a worktree created before
    // its branch had a `kobune.toml` still resolves.
    //
    // Finding none is not an error here: the layer is reported as absent,
    // with the path it would have been at, and `settle` is what turns that
    // into `ConfigNotFound`. Reporting where it was looked for is the
    // whole of what `config show` has to say in that case.
    let found = find_file(start).or_else(|| find_file(main_root));
    let found_project = found.is_some();
    let project_path = found.unwrap_or_else(|| main_root.join(CONFIG_FILE));

    let layout = [
        (ConfigLayer::Global, home.join(GLOBAL_CONFIG_FILE)),
        (ConfigLayer::Project, project_path),
        (ConfigLayer::Local, main_root.join(LOCAL_CONFIG_FILE)),
    ];

    let mut merged = toml::Table::new();
    let mut origins = BTreeMap::new();
    let mut sources = Vec::with_capacity(layout.len());

    let mut only_text = None;

    for (layer, path) in layout {
        match read_layer(&path)? {
            Some((table, text)) => {
                merge_into(&mut merged, table, layer, "", &mut origins);

                // Kept only while exactly one layer has been read. Two and
                // the merged document is on no disk, so there is no line
                // to point at and nothing worth holding the text for.
                only_text = match sources.iter().any(|source: &ConfigSource| source.loaded) {
                    true => None,
                    false => Some(text),
                };

                sources.push(ConfigSource {
                    layer,
                    path,
                    loaded: true,
                });
            }
            None => sources.push(ConfigSource {
                layer,
                path,
                loaded: false,
            }),
        }
    }

    Ok(Layered {
        sources,
        origins,
        merged,
        only_text,
        start: start.to_path_buf(),
        found_project,
    })
}

impl KobuneConfig {
    /// The three layers, merged into one configuration.
    ///
    /// See [`layer_files`] for where each of them is looked for.
    pub fn resolve(start: &Path, main_root: &Path, home: &Path) -> Result<Self> {
        layer_files(start, main_root, home)?.settle()
    }

    /// The same, reporting what the layers are rather than what they mean.
    ///
    /// **A merge that is not a configuration comes back as
    /// [`ConfigReport::problem`], not as an error.** This is what answers
    /// "where did that value come from", and the question is at its most
    /// urgent when the answer is "from a combination that does not load" —
    /// so the layers have to survive the failure that prompted the asking.
    ///
    /// A file that is not TOML at all still fails here. That error already
    /// names the one file at fault, which is the whole of what a person
    /// needs; there is no merged document to explain yet.
    pub fn inspect(start: &Path, main_root: &Path, home: &Path) -> Result<ConfigReport> {
        let layered = layer_files(start, main_root, home)?;

        // Taken off before `settle` consumes the rest, so neither is
        // cloned: the layers are what survives the failure, and the merged
        // document is what fails.
        let sources = layered.sources.clone();
        let origins = layered.origins.clone();
        let problem = layered.settle().err().map(|err| err.to_string());

        Ok(ConfigReport {
            sources,
            origins,
            problem,
        })
    }

    /// The URL suffix. `{name}.localhost` when `[project] domain` is unset.
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

    /// Checks that a syntactically valid configuration also makes sense.
    pub fn validate(&self) -> Result<()> {
        if !naming::is_valid_label(&self.project.name) {
            return Err(Error::ConfigInvalid(format!(
                "[project] name = \"{}\" is not a usable DNS label. \
                 Use lowercase letters, digits and hyphens, up to 63 characters",
                self.project.name
            )));
        }

        for entry in &self.project.carry {
            self.validate_carry_entry(entry)?;
        }

        if self.services.is_empty() {
            return Err(Error::ConfigInvalid("no services are defined".to_string()));
        }

        for (name, svc) in &self.services {
            self.validate_service(name, svc)?;
        }

        self.validate_env_files_are_distinct()?;
        self.validate_no_dependency_cycle()?;
        Ok(())
    }

    /// Checks that no two services want the same file.
    ///
    /// **They hold different environments**, so sharing a path means each
    /// start overwrites the other's — whichever service woke last decides
    /// what the file says, and "rewriting it unchanged is not a write"
    /// never holds, so anything watching it restarts every time.
    fn validate_env_files_are_distinct(&self) -> Result<()> {
        let mut claimed: BTreeMap<PathBuf, &str> = BTreeMap::new();

        for (name, svc) in &self.services {
            let Some(entry) = svc.env_file.as_deref() else {
                continue;
            };

            if let Some(other) = claimed.insert(env_file_path(entry), name) {
                return Err(Error::ConfigInvalid(format!(
                    "services `{other}` and `{name}` both write env_file \
                     `{entry}`. They hold different environments, so each \
                     start would overwrite the other — give them one path each"
                )));
            }
        }

        Ok(())
    }

    /// Checks one `carry` entry before anything is copied.
    ///
    /// **These name files Kobune reads on the user's behalf**, and a
    /// `kobune.toml` arrives with a cloned repository as readily as it is
    /// written by hand. Anything reaching outside the repository is refused
    /// here rather than at copy time, so a bad entry is a configuration error
    /// with a clear message instead of a surprise during `kobune new`.
    ///
    /// Syntax only. A symlink inside the repository can still point out of it,
    /// and that is caught where the copy happens, against the resolved path.
    fn validate_carry_entry(&self, entry: &str) -> Result<()> {
        let refuse = |why: &str| {
            Err(Error::ConfigInvalid(format!(
                "[project] carry entry `{entry}` {why}. Use a path relative to \
                 the repository root, like \".env\" or \"apps/api/.env\""
            )))
        };

        if entry.trim().is_empty() {
            return refuse("is empty");
        }

        let path = Path::new(entry);

        // Its own message: `~/x` is not an absolute path, and saying it is
        // sends someone looking for a leading slash they never wrote.
        if entry.starts_with('~') {
            return refuse("starts with ~, which Kobune does not expand");
        }

        if path.is_absolute() {
            return refuse("is an absolute path");
        }

        if path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return refuse("leaves the repository");
        }

        Ok(())
    }

    /// Checks where a service wants its environment written.
    ///
    /// Syntax and scope only. Whether the path is one Kobune may write —
    /// tracked by git, or holding a file it did not write — is decided
    /// against the worktree at start, where those questions can be asked.
    fn validate_env_file(&self, name: &str, entry: &str, scope: ServiceScope) -> Result<()> {
        let refuse = |why: &str| {
            Err(Error::ConfigInvalid(format!(
                "service `{name}`: env_file `{entry}` {why}. Use a path \
                 relative to the worktree, like \".kobune/env.api\" or \
                 \"apps/web/.env.local\""
            )))
        };

        if entry.trim().is_empty() {
            return refuse("is empty");
        }

        // Padding is never what was meant, and joining it produces a file
        // whose name nothing else will ever spell the same way.
        if entry != entry.trim() {
            return refuse("has whitespace around it");
        }

        let path = Path::new(entry);

        if entry.starts_with('~') {
            return refuse("starts with ~, which Kobune does not expand");
        }

        if path.is_absolute() {
            return refuse("is an absolute path");
        }

        if path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return refuse("leaves the worktree");
        }

        // **Kobune reads these two itself**, so writing one would feed the
        // generated file back in as a layer — and the workspace layer is
        // the most specific there is. Last run's `KOBUNE_URL_*` would then
        // outrank the one being injected now, and a value put there with
        // `kobune env set --workspace` would be overwritten at the next
        // start, since the header it keeps still reads as generated.
        let reserved = [
            Path::new(env::ENV_DIR).join(env::PROJECT_ENV_FILE),
            Path::new(env::ENV_DIR).join(env::WORKSPACE_ENV_FILE),
        ];

        // Compared as `./x` and `x` name one file, which a refusal that
        // spelling could walk around would not be much of a refusal.
        if reserved.iter().any(|held| env_file_path(entry) == *held) {
            return refuse(
                "is a file Kobune reads as an environment layer of its own. \
                 Write beside it instead, like \".kobune/env.api\"",
            );
        }

        // A shared service is mounted no worktree, so the file would be
        // written where that container cannot see it — into whichever
        // worktree happened to start it.
        if scope == ServiceScope::Project {
            return refuse(
                "is on a service with scope = \"project\", which is mounted \
                 no worktree to write it into",
            );
        }

        Ok(())
    }

    fn validate_service(&self, name: &str, svc: &ServiceConfig) -> Result<()> {
        if !naming::is_valid_label(name) {
            return Err(Error::ConfigInvalid(format!(
                "the service name `{name}` is not a usable DNS label. \
                 Use lowercase letters, digits and hyphens, up to 63 characters"
            )));
        }

        match (&svc.image, &svc.build) {
            (Some(_), Some(_)) => {
                return Err(Error::ConfigInvalid(format!(
                    "service `{name}`: image and build are mutually exclusive"
                )));
            }
            (None, None) => {
                return Err(Error::ConfigInvalid(format!(
                    "service `{name}`: one of image or build is required"
                )));
            }
            _ => {}
        }

        if svc.dockerfile.is_some() && svc.build.is_none() {
            return Err(Error::ConfigInvalid(format!(
                "service `{name}`: dockerfile needs build to say which \
                 context to build in"
            )));
        }

        if !svc.build_args.is_empty() && svc.build.is_none() {
            return Err(Error::ConfigInvalid(format!(
                "service `{name}`: build_args has no effect without build"
            )));
        }

        if svc.expose == Some(true) && svc.port.is_none() {
            return Err(Error::ConfigInvalid(format!(
                "service `{name}`: expose = true requires a port"
            )));
        }

        if let Some(env_file) = &svc.env_file {
            self.validate_env_file(name, env_file, svc.scope)?;
        }

        for dep in &svc.depends_on {
            let target = self.services.get(dep).ok_or_else(|| {
                Error::ConfigInvalid(format!(
                    "service `{name}`: depends_on refers to undefined service `{dep}`"
                ))
            })?;

            // A shared instance depending on a per-worktree one leaves no
            // way to decide which worktree's instance to connect to.
            if svc.scope == ServiceScope::Project && target.scope == ServiceScope::Workspace {
                return Err(Error::ConfigInvalid(format!(
                    "service `{name}` (scope = \"project\") depends on \
                     `{dep}` (scope = \"workspace\"). A shared service cannot \
                     depend on a per-worktree one"
                )));
            }
        }

        // An empty one splits to no words at all, which the runtime reads
        // as "use the image's default command" — so `setup = ""` would
        // start the service's own entrypoint in the setup container and
        // wait for it to exit, which for a server is never.
        if svc
            .setup
            .as_deref()
            .is_some_and(|setup| setup.trim().is_empty())
        {
            return Err(Error::ConfigInvalid(format!(
                "service `{name}`: setup is empty. Give it a command, or \
                 remove the line"
            )));
        }

        // The names cannot be taken — `_cache` is not a valid volume name,
        // and the CA is not a named volume at all — but the places they
        // are mounted can be. Two mounts on one target is an error from
        // the container engine, several steps away from the line that
        // caused it.
        for volume in &svc.volumes {
            let mut parts = volume.split(':');
            let _source = parts.next();
            let target = parts.next();

            for (reserved, variable, way_out) in RESERVED_MOUNTS {
                if target != Some(reserved) {
                    continue;
                }

                return Err(Error::ConfigInvalid(format!(
                    "service `{name}`: {reserved} is where {variable} is \
                     already mounted, so `{volume}` would be a second mount \
                     on the same path. {way_out}"
                )));
            }
        }

        // Same reason, for storage rather than services: one instance
        // serves every worktree, so there is no worktree whose volume it
        // would be. Caught here rather than at start, where it would come
        // out as a container mounting whichever one it happened to make.
        if svc.scope == ServiceScope::Project {
            for volume in &svc.volumes {
                if volume.split(':').next().is_some_and(is_workspace_scoped) {
                    return Err(Error::ConfigInvalid(format!(
                        "service `{name}` (scope = \"project\") asks for the \
                         workspace-scoped volume `{volume}`. A shared service \
                         has no worktree to keep one per"
                    )));
                }
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

        // Explicit-stack DFS. The path is kept so the cycle can be named.
        for root in self.services.keys() {
            if marks[root.as_str()] != Mark::Unvisited {
                continue;
            }

            let mut path: Vec<&str> = vec![root.as_str()];
            let mut cursor: Vec<usize> = vec![0];
            marks[root.as_str()] = Mark::InProgress;

            while let Some(&node) = path.last() {
                let index = *cursor
                    .last()
                    .expect("cursor and path are pushed in lockstep");
                let deps = &self.services[node].depends_on;

                if index >= deps.len() {
                    marks[node] = Mark::Done;
                    path.pop();
                    cursor.pop();
                    continue;
                }

                *cursor.last_mut().expect("as above") += 1;
                let dep = deps[index].as_str();

                match marks[dep] {
                    Mark::Done => {}
                    Mark::InProgress => {
                        let start = path.iter().position(|n| *n == dep).unwrap_or(0);
                        let mut chain: Vec<&str> = path[start..].to_vec();
                        chain.push(dep);
                        return Err(Error::ConfigInvalid(format!(
                            "depends_on has a cycle: {}",
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

    /// Orders service names so that dependencies come first.
    ///
    /// Every service is returned: [`Self::validate`] has already ruled out
    /// cycles.
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

    /// Groups services so that everything in one wave can start at once.
    ///
    /// Wave 0 is what depends on nothing. Wave *n* is what depends only on
    /// services in earlier waves — so nothing within a wave depends on
    /// anything else in it, which is what makes starting them together
    /// safe.
    ///
    /// **Flattened, this is a valid startup order but not the same one
    /// [`Self::startup_order`] gives.** Bucketing by depth necessarily
    /// pulls every independent service ahead of every service one level
    /// down, and `startup_order`'s depth-first walk does not. For
    /// `web -> {api, cache}`, `api -> db`, `worker -> db`:
    ///
    /// ```text
    /// startup_order  db, api, cache, web, worker
    /// flattened      db, cache, api, worker, web
    /// ```
    ///
    /// Both put every dependency in front of what needs it, which is all
    /// either promises. A caller that cares which of the two it walks —
    /// because something reads the state of whatever is already running —
    /// wants `startup_order`, not this flattened.
    pub fn startup_waves(&self) -> Vec<Vec<&str>> {
        // One pass is enough because `startup_order` has already put every
        // dependency in front of the service that names it: by the time a
        // service is reached, each of its dependencies has a depth.
        let mut depths: IndexMap<&str, usize> = IndexMap::with_capacity(self.services.len());

        for name in self.startup_order() {
            let depth = self.services[name]
                .depends_on
                .iter()
                // Indexed, not looked up with a fallback. A missing
                // dependency cannot happen — `validate` rejects one that
                // names nothing, and `startup_order` puts the rest in
                // front — and reading it as depth 0 would put a service in
                // the same wave as its own dependency, which is the one
                // thing this must never do.
                .map(|dep| depths[dep.as_str()] + 1)
                .max()
                .unwrap_or(0);

            depths.insert(name, depth);
        }

        let mut waves: Vec<Vec<&str>> = Vec::new();
        for (name, depth) in depths {
            if waves.len() <= depth {
                waves.resize_with(depth + 1, Vec::new);
            }
            waves[depth].push(name);
        }

        waves
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<KobuneConfig> {
        let config: KobuneConfig = toml::from_str(text).expect("syntax is assumed valid");
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
            "expose = false hides the service even with a port"
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
        assert!(err.to_string().contains("mutually exclusive"));
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
        assert!(err.to_string().contains("one of image or build"));
    }

    #[test]
    fn accepts_files_to_carry() {
        let config = parse(
            r#"
            [project]
            name = "myapp"
            carry = [".env", "apps/api/.dev.vars"]
            [services.web]
            image = "node:22"
        "#,
        )
        .expect("is valid");

        assert_eq!(config.project.carry, vec![".env", "apps/api/.dev.vars"]);
    }

    #[test]
    fn carry_defaults_to_nothing() {
        let config = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
        "#,
        )
        .expect("is valid");

        assert!(config.project.carry.is_empty());
    }

    #[test]
    fn refuses_carry_entries_that_leave_the_repository() {
        // These name files Kobune reads on someone's behalf, and a
        // kobune.toml arrives with a clone as readily as it is hand-written.
        // Asserted per entry: a message that is true of one of these and
        // not the others is exactly the drift worth catching.
        let cases = [
            ("../.env", "leaves the repository"),
            ("a/../../b", "leaves the repository"),
            ("/etc/passwd", "is an absolute path"),
            ("~/.aws/credentials", "which Kobune does not expand"),
        ];

        for (entry, expected) in cases {
            let err = parse(&format!(
                r#"
                [project]
                name = "myapp"
                carry = ["{entry}"]
                [services.web]
                image = "node:22"
            "#
            ))
            .unwrap_err();

            let message = err.to_string();
            assert!(message.contains("carry"), "{entry}: {message}");
            assert!(message.contains(expected), "{entry}: {message}");
        }
    }

    #[test]
    fn accepts_an_env_file_anywhere_in_the_worktree() {
        // Anywhere, because the tools that need this read a path of their
        // own choosing: `.env.local` beside the app, not `.kobune/`.
        let config = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            env_file = "apps/web/.env.local"
        "#,
        )
        .expect("is valid");

        assert_eq!(
            config.services["web"].env_file.as_deref(),
            Some("apps/web/.env.local")
        );
    }

    #[test]
    fn refuses_an_env_file_that_leaves_the_worktree() {
        let cases = [
            ("../.env", "leaves the worktree"),
            ("/etc/environment", "is an absolute path"),
            ("~/.env", "which Kobune does not expand"),
            ("  ", "is empty"),
            (" .env ", "has whitespace around it"),
        ];

        for (entry, expected) in cases {
            let err = parse(&format!(
                r#"
                [project]
                name = "myapp"
                [services.web]
                image = "node:22"
                env_file = "{entry}"
            "#
            ))
            .unwrap_err();

            let message = err.to_string();
            assert!(message.contains("env_file"), "{entry}: {message}");
            assert!(message.contains(expected), "{entry}: {message}");
        }
    }

    #[test]
    fn refuses_an_env_file_kobune_reads_as_a_layer_of_its_own() {
        // Writing one feeds the generated file back in as input, and the
        // workspace layer is the most specific there is: last run's
        // KOBUNE_URL_* would outrank the one being injected now, and a
        // `kobune env set --workspace` value would be overwritten.
        for entry in [".kobune/env", ".kobune/env.local", "./.kobune/env.local"] {
            let err = parse(&format!(
                r#"
                [project]
                name = "myapp"
                [services.web]
                image = "node:22"
                env_file = "{entry}"
            "#
            ))
            .unwrap_err();

            let message = err.to_string();
            assert!(message.contains("environment layer"), "{entry}: {message}");
        }
    }

    #[test]
    fn refuses_two_services_writing_the_same_env_file() {
        // They hold different environments, so each start would overwrite
        // the other's — and nothing watching the file would ever settle.
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            env_file = ".kobune/env.shared"
            [services.api]
            image = "node:22"
            env_file = ".kobune/env.shared"
        "#,
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("env_file"), "{message}");
        assert!(
            message.contains("web") && message.contains("api"),
            "{message}"
        );
    }

    #[test]
    fn refuses_an_env_file_on_a_shared_service() {
        // A shared service is mounted no worktree, so the file would land
        // where that container cannot see it.
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.db]
            image = "postgres:16"
            scope = "project"
            env_file = ".kobune/env.db"
        "#,
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("env_file"), "{message}");
        assert!(message.contains("scope"), "{message}");
    }

    #[test]
    fn refuses_an_empty_carry_entry() {
        let err = parse(
            r#"
            [project]
            name = "myapp"
            carry = ["  "]
            [services.web]
            image = "node:22"
        "#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("is empty"), "{err}");
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
        assert!(err.to_string().contains("requires a port"));
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
        assert!(err.to_string().contains("undefined service"));
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
        assert!(err.to_string().contains("A shared service cannot"));
    }

    #[test]
    fn refuses_a_workspace_volume_on_a_shared_service() {
        // One instance serves every worktree, so there is no worktree whose
        // volume it would be.
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.db]
            image = "postgres:16"
            scope = "project"
            volumes = ["pgdata@workspace:/var/lib/postgresql/data"]
        "#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("no worktree"), "{err}");
    }

    #[test]
    fn a_host_path_ending_in_workspace_is_not_a_scope() {
        // A real directory named `certs@workspace`. Refusing this would
        // fail the whole project with a message about something the user
        // did not write.
        parse(
            r#"
            [project]
            name = "myapp"
            [services.db]
            image = "postgres:16"
            scope = "project"
            volumes = ["./certs@workspace:/certs:ro"]
        "#,
        )
        .expect("a host path is never scoped, however it is spelled");
    }

    #[test]
    fn an_empty_setup_is_refused() {
        // It splits to no words, which the runtime reads as "use the
        // image's command" — so the setup container would start the
        // service itself and be waited on for ever.
        for setup in ["", "   "] {
            let err = parse(&format!(
                r#"
                [project]
                name = "myapp"
                [services.web]
                image = "node:22"
                setup = "{setup}"
            "#
            ))
            .unwrap_err();

            assert!(err.to_string().contains("setup is empty"), "{err}");
        }
    }

    #[test]
    fn the_cache_volume_cannot_be_named() {
        // Not a reservation to remember — `_cache` is not a valid volume
        // name, so no spelling of `volumes` can reach it. Anyone already
        // using a volume called `cache` keeps it, and keeps its contents.
        assert!(!naming::is_valid_label(CACHE_VOLUME));

        parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            volumes = ["cache:/tmp/cache"]
        "#,
        )
        .expect("`cache` is an ordinary name, and stays one");
    }

    #[test]
    fn a_second_mount_on_the_cache_path_is_refused() {
        // Two mounts on one target is an error from the container engine,
        // a long way from the line that caused it.
        let err = parse(&format!(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            volumes = ["mine:{CACHE_TARGET}"]
        "#
        ))
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("KOBUNE_CACHE_DIR"), "{message}");
        assert!(!message.contains("  "), "run-together spacing: {message}");
    }

    #[test]
    fn a_second_mount_on_the_certificate_path_is_refused() {
        // Same reason as the cache path, and the same failure without it:
        // the engine refuses the container and names neither the file nor
        // the line that asked for it.
        let err = parse(&format!(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            volumes = ["mine:{CA_TARGET}"]
        "#
        ))
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("KOBUNE_CA_FILE"), "{message}");
    }

    #[test]
    fn mounting_below_the_cache_path_is_allowed() {
        parse(&format!(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            volumes = ["mine:{CACHE_TARGET}/mine"]
        "#
        ))
        .expect("only the exact path is taken");
    }

    #[test]
    fn a_workspace_volume_is_fine_on_a_per_worktree_service() {
        parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            volumes = ["node-modules@workspace:/workspace/node_modules"]
        "#,
        )
        .expect("this is the case the scope exists for");
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
        assert!(err.to_string().contains("has a cycle"), "got: {err}");
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
        assert!(err.to_string().contains("has a cycle"), "got: {err}");
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
        assert!(err.to_string().contains("not a usable DNS label"));
    }

    #[test]
    fn rejects_unknown_field() {
        let result: std::result::Result<KobuneConfig, _> = toml::from_str(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            porrt = 3000
        "#,
        );
        assert!(result.is_err(), "a typo must be caught");
    }

    /// `web` -> `api` -> `db`: nothing can overlap.
    const CHAIN: &str = r#"
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
        "#;

    /// `web` over both `api` and `worker`, which share `db`. The two
    /// middles are independent of each other.
    const DIAMOND: &str = r#"
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
        "#;

    #[test]
    fn startup_order_respects_dependencies() {
        let config = parse(CHAIN).expect("valid");

        let order = config.startup_order();
        assert_eq!(order.len(), 3);

        let pos = |name: &str| order.iter().position(|s| *s == name).expect("present");
        assert!(pos("db") < pos("api"));
        assert!(pos("api") < pos("web"));
    }

    #[test]
    fn startup_order_handles_diamond() {
        let config = parse(DIAMOND).expect("valid");

        let order = config.startup_order();
        assert_eq!(
            order.len(),
            4,
            "every service appears exactly once: {order:?}"
        );

        let pos = |name: &str| order.iter().position(|s| *s == name).expect("present");
        assert!(pos("db") < pos("api"));
        assert!(pos("db") < pos("worker"));
        assert!(pos("api") < pos("web"));
        assert!(pos("worker") < pos("web"));
    }

    #[test]
    fn services_that_need_nothing_share_the_first_wave() {
        // The point of the whole thing: three services that know nothing
        // of each other are one wave, not three.
        let config = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "x"
            [services.api]
            image = "x"
            [services.db]
            image = "x"
        "#,
        )
        .expect("valid");

        assert_eq!(config.startup_waves(), vec![vec!["web", "api", "db"]]);
    }

    #[test]
    fn a_chain_gets_a_wave_each() {
        let config = parse(CHAIN).expect("valid");

        assert_eq!(
            config.startup_waves(),
            vec![vec!["db"], vec!["api"], vec!["web"]]
        );
    }

    #[test]
    fn a_diamond_puts_the_two_middles_together() {
        let config = parse(DIAMOND).expect("valid");

        assert_eq!(
            config.startup_waves(),
            vec![vec!["db"], vec!["api", "worker"], vec!["web"]]
        );
    }

    #[test]
    fn a_wave_waits_for_the_furthest_of_its_dependencies() {
        // `web` depends on `db` directly as well as through `api`. Put in
        // wave 1 it would start alongside `api`, which is the one thing
        // `depends_on` promises it will not do.
        let config = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "x"
            depends_on = ["db", "api"]
            [services.api]
            image = "x"
            depends_on = ["db"]
            [services.db]
            image = "x"
        "#,
        )
        .expect("valid");

        assert_eq!(
            config.startup_waves(),
            vec![vec!["db"], vec!["api"], vec!["web"]]
        );
    }

    /// `web` over `api` and `cache`; `api` and `worker` over `db`. The
    /// point of it is that `cache` and `worker` are independent of the
    /// chain through `api`, which is where the two orders come apart.
    const FORK: &str = r#"
            [project]
            name = "myapp"
            [services.web]
            image = "x"
            depends_on = ["api", "cache"]
            [services.api]
            image = "x"
            depends_on = ["db"]
            [services.worker]
            image = "x"
            depends_on = ["db"]
            [services.cache]
            image = "x"
            [services.db]
            image = "x"
        "#;

    #[test]
    fn every_dependency_still_comes_first_when_the_waves_are_flattened() {
        let config = parse(FORK).expect("valid");

        let flattened: Vec<&str> = config.startup_waves().concat();
        let pos = |name: &str| flattened.iter().position(|s| *s == name).expect("present");

        assert_eq!(flattened.len(), config.services.len());
        assert!(pos("db") < pos("api"));
        assert!(pos("db") < pos("worker"));
        assert!(pos("api") < pos("web"));
        assert!(pos("cache") < pos("web"));
    }

    #[test]
    fn flattening_the_waves_is_not_the_same_list_as_startup_order() {
        // Pinned because it is easy to assume otherwise, and something
        // did: the two are both valid, and a caller that reads the state
        // of whatever is already running can tell them apart. See the
        // note on `startup_waves`, and `supervisor::waves`, which keeps
        // sequential backends on `startup_order` for this reason.
        let config = parse(FORK).expect("valid");

        assert_eq!(
            config.startup_order(),
            vec!["db", "api", "cache", "web", "worker"]
        );
        assert_eq!(
            config.startup_waves().concat(),
            vec!["db", "cache", "api", "worker", "web"]
        );
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

    /// A repository and a `$KOBUNE_HOME`, each layer written or not.
    ///
    /// The main worktree and the directory the search starts from are
    /// separate arguments because that difference is the whole point of
    /// the local layer: a worktree is not under the main checkout.
    struct Layout {
        home: tempfile::TempDir,
        repo: tempfile::TempDir,
    }

    impl Layout {
        fn new() -> Self {
            Self {
                home: tempfile::tempdir().expect("tempdir"),
                repo: tempfile::tempdir().expect("tempdir"),
            }
        }

        fn write(self, layer: ConfigLayer, text: &str) -> Self {
            let path = match layer {
                ConfigLayer::Global => self.home.path().join(GLOBAL_CONFIG_FILE),
                ConfigLayer::Project => self.repo.path().join(CONFIG_FILE),
                ConfigLayer::Local => self.repo.path().join(LOCAL_CONFIG_FILE),
            };

            std::fs::write(path, text).expect("writes");
            self
        }

        /// Resolves as the main checkout would.
        fn resolve(&self) -> Result<KobuneConfig> {
            KobuneConfig::resolve(self.repo.path(), self.repo.path(), self.home.path())
        }

        /// The layers, which survive a merge that will not load.
        fn inspect(&self) -> Result<ConfigReport> {
            KobuneConfig::inspect(self.repo.path(), self.repo.path(), self.home.path())
        }
    }

    /// The committed layer every case starts from.
    const COMMITTED: &str = r#"
        [project]
        name = "myapp"

        [runtime]
        default = "docker"

        [services.web]
        image = "node:22"
        port = 3000
        tty = true
        volumes = ["node-modules@workspace:/workspace/node_modules"]
    "#;

    #[test]
    fn a_project_on_its_own_still_resolves() {
        // The two other layers are meant to be missing most of the time.
        let layout = Layout::new().write(ConfigLayer::Project, COMMITTED);
        let config = layout.resolve().expect("is valid");
        let report = layout.inspect().expect("is valid");

        assert_eq!(config.runtime.default, "docker");
        assert_eq!(report.loaded().count(), 1);
        assert_eq!(report.overrides().count(), 0, "nothing was contested");
        assert_eq!(report.problem, None);

        let absent: Vec<ConfigLayer> = report
            .sources
            .iter()
            .filter(|source| !source.loaded)
            .map(|source| source.layer)
            .collect();

        assert_eq!(
            absent,
            vec![ConfigLayer::Global, ConfigLayer::Local],
            "a layer with no file is still reported, with the path it looked at"
        );
    }

    #[test]
    fn the_machine_layer_sets_a_runtime_without_restating_the_project() {
        // The case the whole thing exists for: this computer runs Apple
        // Container, and says so once for every project on it.
        let layout = Layout::new()
            .write(ConfigLayer::Global, "[runtime]\ndefault = \"apple\"\n")
            .write(ConfigLayer::Project, COMMITTED);

        let config = layout.resolve().expect("is valid");

        assert_eq!(
            config.runtime.default, "docker",
            "the committed file is more specific than the machine"
        );
        assert_eq!(config.project.name, "myapp");
    }

    #[test]
    fn the_local_layer_wins_and_says_what_it_beat() {
        let layout = Layout::new()
            .write(ConfigLayer::Global, "[runtime]\ndefault = \"apple\"\n")
            .write(ConfigLayer::Project, COMMITTED)
            .write(ConfigLayer::Local, "[runtime]\ndefault = \"apple\"\n");

        let config = layout.resolve().expect("is valid");
        assert_eq!(config.runtime.default, "apple");

        let report = layout.inspect().expect("is valid");
        let origin = &report.origins["runtime.default"];
        assert_eq!(origin.layer, ConfigLayer::Local);
        assert_eq!(
            origin.overridden,
            vec![ConfigLayer::Global, ConfigLayer::Project],
            "both losers are named, in the order they were applied"
        );
        assert_eq!(origin.value, "apple", "rendered without its quotes");
    }

    #[test]
    fn a_layer_reaches_one_key_without_disturbing_its_neighbours() {
        // Tables merge. Without that, naming `services.web` to change its
        // port would drop the image, the terminal and the volume with it —
        // which is the failure that makes an overlay worse than useless.
        let layout = Layout::new().write(ConfigLayer::Project, COMMITTED).write(
            ConfigLayer::Local,
            "[services.web]\nport = 4000\n[services.db]\nimage = \"postgres:16\"\n",
        );

        let config = layout.resolve().expect("is valid");
        let web = config.service("web").expect("survives the merge");

        assert_eq!(web.port, Some(4000));
        assert_eq!(web.image.as_deref(), Some("node:22"));
        assert!(web.tty, "a bool the overlay never mentioned stays as set");
        assert_eq!(
            web.volumes,
            vec!["node-modules@workspace:/workspace/node_modules"],
            "and so does an array"
        );

        assert!(
            config.services.contains_key("db"),
            "a service the overlay introduces is added, not rejected"
        );
        assert_eq!(
            layout.inspect().expect("is valid").origins["services.db.image"].layer,
            ConfigLayer::Local,
            "and is attributed key by key like any other"
        );
    }

    #[test]
    fn an_array_replaces_rather_than_appends() {
        // Appending would leave no way to remove an entry, and these
        // arrays are short enough to restate.
        let layout = Layout::new().write(ConfigLayer::Project, COMMITTED).write(
            ConfigLayer::Local,
            "[services.web]\nvolumes = [\"./certs:/certs:ro\"]\n",
        );

        let config = layout.resolve().expect("is valid");

        assert_eq!(config.services["web"].volumes, vec!["./certs:/certs:ro"]);
    }

    #[test]
    fn the_merged_result_is_what_gets_validated() {
        // Each file is valid TOML and neither is wrong on its own. Only
        // the merge is, and the message has to name both files, because
        // the document it describes is on neither of them.
        let layout = Layout::new()
            .write(ConfigLayer::Project, COMMITTED)
            .write(ConfigLayer::Local, "[services.web]\nbuild = \"./web\"\n");

        let err = layout.resolve().unwrap_err().to_string();

        assert!(err.contains("mutually exclusive"), "{err}");
        assert!(err.contains(CONFIG_FILE), "name the committed file: {err}");
        assert!(err.contains(LOCAL_CONFIG_FILE), "and the local one: {err}");
    }

    #[test]
    fn a_typo_in_a_layer_is_still_caught() {
        // `deny_unknown_fields` has to survive the merge, or the overlay
        // becomes the one place a misspelling goes unnoticed.
        let layout = Layout::new()
            .write(ConfigLayer::Project, COMMITTED)
            .write(ConfigLayer::Local, "[runtime]\ndefalut = \"apple\"\n");

        let err = layout.resolve().unwrap_err().to_string();

        assert!(err.contains("defalut"), "name the key: {err}");
        assert!(
            err.contains(LOCAL_CONFIG_FILE),
            "and the file it came from: {err}"
        );
    }

    #[test]
    fn a_typo_in_the_only_file_still_says_which_line() {
        // The merged document has no line numbers, and nearly every
        // project has one layer. Losing `at line N` there would mean
        // searching a long `kobune.toml` by eye for a misspelled key.
        let layout = Layout::new().write(
            ConfigLayer::Project,
            "[project]\nname = \"myapp\"\n\n[services.web]\nimage = \"x\"\ndefalut = \"oops\"\n",
        );

        let err = layout.resolve().unwrap_err().to_string();

        assert!(err.contains("defalut"), "{err}");
        assert!(err.contains("line 6"), "point at the line: {err}");
    }

    #[test]
    fn a_typo_across_layers_names_the_files_instead() {
        // Two layers and the document is on no disk, so there is no line
        // to point at — the files it merged are what there is to say.
        let layout = Layout::new()
            .write(ConfigLayer::Project, COMMITTED)
            .write(ConfigLayer::Local, "[runtime]\ndefalut = \"apple\"\n");

        let err = layout.resolve().unwrap_err().to_string();

        assert!(err.contains("defalut"), "{err}");
        assert!(err.contains(CONFIG_FILE), "{err}");
        assert!(err.contains(LOCAL_CONFIG_FILE), "{err}");
    }

    #[test]
    fn malformed_toml_names_the_layer_it_is_in() {
        let layout = Layout::new()
            .write(ConfigLayer::Project, COMMITTED)
            .write(ConfigLayer::Local, "this is not toml\n");

        let err = layout.resolve().unwrap_err().to_string();
        assert!(err.contains(LOCAL_CONFIG_FILE), "{err}");
    }

    #[test]
    fn the_local_layer_is_anchored_on_the_main_worktree() {
        // The one that decides whether any of this works. `kobune.toml` is
        // committed, so a worktree holds a copy and the upward search stops
        // there; `kobune.local.toml` is gitignored, so `git worktree add`
        // never brings one. Looked for beside the file that was found, the
        // override would apply in the main checkout and silently nowhere
        // else.
        let layout = Layout::new()
            .write(ConfigLayer::Project, COMMITTED)
            .write(ConfigLayer::Local, "[runtime]\ndefault = \"apple\"\n");

        // A worktree as `kobune new` places one: a sibling of the main
        // checkout, carrying its own committed copy and nothing untracked.
        let worktree = tempfile::tempdir().expect("tempdir");
        std::fs::write(worktree.path().join(CONFIG_FILE), COMMITTED).expect("writes");

        let config = KobuneConfig::resolve(worktree.path(), layout.repo.path(), layout.home.path())
            .expect("is valid");

        assert_eq!(
            config.runtime.default, "apple",
            "the worktree reads the override its main checkout holds"
        );
    }

    #[test]
    fn a_worktree_without_a_committed_file_falls_back_to_the_main_one() {
        // Right after `git worktree add` on a branch that predates the
        // project's kobune.toml.
        let layout = Layout::new().write(ConfigLayer::Project, COMMITTED);
        let worktree = tempfile::tempdir().expect("tempdir");

        let config = KobuneConfig::resolve(worktree.path(), layout.repo.path(), layout.home.path())
            .expect("is valid");

        assert_eq!(config.project.name, "myapp");
    }

    #[test]
    fn no_project_file_anywhere_is_the_error_it_always_was() {
        let layout = Layout::new().write(ConfigLayer::Global, "[runtime]\ndefault = \"apple\"\n");

        assert!(
            matches!(layout.resolve(), Err(Error::ConfigNotFound(_))),
            "a machine layer alone is not a project"
        );
    }

    #[test]
    fn merging_keeps_the_order_the_services_were_declared_in() {
        // **`toml::Table` is a `BTreeMap` unless `preserve_order` is on**,
        // and merging through one sorted every project's services
        // alphabetically — with no overlay file present, because the merge
        // is now how the single committed file is read too. `services` is
        // an `IndexMap` precisely so declaration order survives:
        // `startup_order` walks it, and `KOBUNE_SERVICE` is "the first one
        // declared". Deliberately not alphabetical, so a regression shows.
        let layout = Layout::new().write(
            ConfigLayer::Project,
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "x"
            [services.api]
            image = "x"
            [services.db]
            image = "x"
        "#,
        );

        let config = layout.resolve().expect("is valid");

        assert_eq!(
            config.services.keys().collect::<Vec<_>>(),
            vec!["web", "api", "db"]
        );
        assert_eq!(config.startup_order(), vec!["web", "api", "db"]);
    }

    #[test]
    fn an_overlay_does_not_reorder_what_it_does_not_mention() {
        // Adding a service puts it last, rather than wherever its name
        // happens to sort.
        let layout = Layout::new()
            .write(
                ConfigLayer::Project,
                r#"
                [project]
                name = "myapp"
                [services.web]
                image = "x"
                [services.api]
                image = "x"
            "#,
            )
            .write(
                ConfigLayer::Local,
                "[services.web]
port = 4000
[services.aaa]
image = \"x\"
",
            );

        let config = layout.resolve().expect("is valid");

        assert_eq!(
            config.services.keys().collect::<Vec<_>>(),
            vec!["web", "api", "aaa"]
        );
    }

    #[test]
    fn a_layer_that_changes_a_keys_shape_takes_the_old_origins_with_it() {
        // A layer may replace a table with a scalar, or the other way
        // round. Origins left behind would have `config show` list keys
        // the merged document does not contain — in the one command whose
        // job is to say what it does contain.
        let layout = Layout::new()
            .write(ConfigLayer::Global, "[runtime]\ndefault = \"apple\"\n")
            .write(ConfigLayer::Project, COMMITTED)
            // Mistyped: `runtime` as a string, not a table.
            .write(ConfigLayer::Local, "runtime = \"apple\"\n");

        let report = layout.inspect().expect("the layers are still readable");

        assert!(
            !report.origins.contains_key("runtime.default"),
            "the table it replaced is gone from the document, so from the report: {:?}",
            report.origins.keys().collect::<Vec<_>>()
        );
        assert_eq!(report.origins["runtime"].layer, ConfigLayer::Local);
    }

    #[test]
    fn a_table_displacing_a_scalar_forgets_that_scalar() {
        // The same the other way round, which nothing below the table
        // would otherwise overwrite.
        let layout = Layout::new()
            .write(ConfigLayer::Global, "runtime = \"apple\"\n")
            .write(ConfigLayer::Project, COMMITTED);

        let report = layout.inspect().expect("the layers are still readable");

        assert!(
            !report.origins.contains_key("runtime"),
            "the scalar is not in the merged document: {:?}",
            report.origins.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            report.origins["runtime.default"].layer,
            ConfigLayer::Project
        );
    }

    #[test]
    fn the_layers_survive_a_merge_that_will_not_load() {
        // The whole reason `inspect` is not `resolve`. Somebody asks where
        // a value came from *because* the configuration stopped loading,
        // and an answer that failed alongside it would leave them with the
        // same message and nothing to read it against.
        let layout = Layout::new()
            .write(ConfigLayer::Global, "[runtime]\ndefault = \"apple\"\n")
            .write(ConfigLayer::Project, COMMITTED)
            .write(ConfigLayer::Local, "[runtime]\ndefalut = \"apple\"\n");

        assert!(layout.resolve().is_err(), "it really does not load");

        let report = layout.inspect().expect("the layers are still readable");

        assert_eq!(report.loaded().count(), 3, "all three are still named");
        assert!(
            report
                .problem
                .as_deref()
                .is_some_and(|why| why.contains("defalut")),
            "and the reason travels with them: {:?}",
            report.problem
        );
        assert_eq!(
            report.origins["runtime.default"].layer,
            ConfigLayer::Project,
            "what did merge is still attributed"
        );
    }

    #[test]
    fn a_merge_that_only_fails_validation_keeps_its_layers_too() {
        // Both files are valid TOML and each is fine alone. Only the
        // combination is wrong, which is the case with no file to open.
        let layout = Layout::new()
            .write(ConfigLayer::Project, COMMITTED)
            .write(ConfigLayer::Local, "[services.web]\nbuild = \"./web\"\n");

        let report = layout.inspect().expect("the layers are still readable");

        assert!(
            report
                .problem
                .as_deref()
                .is_some_and(|why| why.contains("mutually exclusive")),
            "{:?}",
            report.problem
        );
        assert_eq!(
            report.origins["services.web.build"].layer,
            ConfigLayer::Local,
            "which points straight at the layer that did it"
        );
    }

    #[test]
    fn the_layers_are_reported_when_the_project_file_is_the_missing_one() {
        // The case the command is sold on — "my file is not where I think
        // it is" — and the one it used to answer with the same
        // `ConfigNotFound` every other command already gave.
        let layout = Layout::new().write(ConfigLayer::Global, "[runtime]\ndefault = \"apple\"\n");

        assert!(
            matches!(layout.resolve(), Err(Error::ConfigNotFound(_))),
            "resolving still fails, for every command that needs a config"
        );

        let report = layout.inspect().expect("the layers are still readable");

        let project = report
            .sources
            .iter()
            .find(|source| source.layer == ConfigLayer::Project)
            .expect("the layer is listed even with no file behind it");

        assert!(!project.loaded);
        assert!(
            project.path.ends_with(CONFIG_FILE),
            "with the path it was looked for at: {}",
            project.path.display()
        );
        assert!(
            report
                .problem
                .as_deref()
                .is_some_and(|why| why.contains("no kobune.toml")),
            "{:?}",
            report.problem
        );
    }

    #[test]
    fn a_file_that_is_not_toml_still_fails_inspection() {
        // There is no merged document to explain yet, and the error names
        // the one file at fault, which is the whole of what is needed.
        let layout = Layout::new()
            .write(ConfigLayer::Project, COMMITTED)
            .write(ConfigLayer::Local, "this is not toml\n");

        let err = layout.inspect().unwrap_err().to_string();
        assert!(err.contains(LOCAL_CONFIG_FILE), "{err}");
    }
}

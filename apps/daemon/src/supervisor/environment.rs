//! Settling a service's environment, and writing the file some tools
//! read instead of their own.
//!
//! **Three layers, later winning**: global, project, then the worktree's
//! own. What makes that worth a module of its own is that the answer has
//! to be identical wherever it is asked from — `up`, a wake from
//! scale-to-zero, `exec`, and `env ls` all settle the same values, and
//! two of them disagreeing is a bug nobody can see from either side.

use std::collections::BTreeMap;
use std::path::PathBuf;

use kobune_api::{ApiError, EnvInfo, ErrorCode, Response, Target};
use kobune_core::WorkspaceRecord;
use kobune_core::config::KobuneConfig;
use kobune_runtime::EventSink;

use crate::env;
use crate::secrets;

use super::Supervisor;
use super::lifecycle::validate_service_names;

impl Supervisor {
    /// Shows the environment, layer by layer.
    ///
    /// **Each value says which layer defined it.** With four layers, not
    /// seeing that an unintended one is winning makes the cause impossible
    /// to find.
    pub(super) async fn env_list(
        &self,
        target: Target,
        reveal: bool,
        service: Option<String>,
    ) -> Result<Response, ApiError> {
        let resolved = self.resolve(&target).await?;

        // Named: what that container is given, its own `env` included.
        // Unnamed: only what every service shares.
        //
        // **Not "whichever service came first".** That is what this used to
        // do, and it showed one service's own variables as if they were
        // everyone's.
        if let Some(service) = &service {
            validate_service_names(&resolved.config, std::slice::from_ref(service))?;
        }

        let layers = env::layers_for_service(
            &resolved.config,
            &resolved.project,
            &resolved.workspace,
            &resolved.repo.main_root,
            service.as_deref(),
            &self.paths,
            &self.gateway,
        )
        .map_err(|err| ApiError::new(ErrorCode::InvalidConfig, err.to_string()))?;

        // **A listing that cannot settle still lists.** This is the tool
        // someone reaches for to find the value that will not settle, and
        // one bad `${...}` taking the whole listing with it leaves them
        // with the error alone and nowhere to look. Only the values at
        // fault are marked, so the rest are not left under suspicion.
        let settled = layers.settle();

        let entries = settled
            .entries
            .iter()
            .map(|entry| {
                let secret = entry.secret_ref();

                // Injected values are Kobune's own and hold no secrets.
                // Checking a URL is common, so they stay visible.
                let injected = entry.scope == kobune_core::EnvScope::Injected;

                EnvInfo {
                    key: entry.key.clone(),
                    value: if reveal || injected || secret.is_some() {
                        // A secret stays a reference even under --reveal.
                        // Showing the value would mean resolving it, and
                        // that only happens at start.
                        entry.raw.clone()
                    } else {
                        kobune_core::env::mask(&entry.raw)
                    },
                    scope: entry.scope,
                    secret: secret.is_some(),
                    source: secret.map(|reference| reference.describe()),
                    unsettled: settled
                        .reason_for(&entry.key)
                        .and_then(|err| env::unsettled(err, service.as_deref(), &resolved.config)),
                }
            })
            .collect();

        Ok(Response::Env { entries, service })
    }
    pub(super) async fn env_set(
        &self,
        target: Target,
        scope: kobune_core::EnvScope,
        key: String,
        value: String,
    ) -> Result<Response, ApiError> {
        if !kobune_core::env::is_valid_key(&key) {
            return Err(ApiError::new(
                ErrorCode::InvalidConfig,
                format!("`{key}` is not a valid environment variable name"),
            )
            .with_hint("letters, digits and underscores only, and not starting with a digit"));
        }

        let path = self.env_file_path(&target, scope).await?;
        let current = read_or_empty(&path)?;

        kobune_core::env::write_file(&path, &kobune_core::env::upsert(&current, &key, &value))
            .map_err(|err| ApiError::internal(err.to_string()))?;

        written(key, self.env_list(target, false, None).await)
    }
    pub(super) async fn env_unset(
        &self,
        target: Target,
        scope: kobune_core::EnvScope,
        key: String,
    ) -> Result<Response, ApiError> {
        let path = self.env_file_path(&target, scope).await?;
        let current = read_or_empty(&path)?;

        kobune_core::env::write_file(&path, &kobune_core::env::remove(&current, &key))
            .map_err(|err| ApiError::internal(err.to_string()))?;

        written(key, self.env_list(target, false, None).await)
    }
    /// Where a layer's file lives.
    pub(super) async fn env_file_path(
        &self,
        target: &Target,
        scope: kobune_core::EnvScope,
    ) -> Result<PathBuf, ApiError> {
        if !scope.is_writable() {
            return Err(ApiError::new(
                ErrorCode::InvalidConfig,
                format!("{} values cannot be written to a file", scope.label()),
            ));
        }

        if scope == kobune_core::EnvScope::Global {
            return Ok(self.paths.root().join(kobune_core::env::GLOBAL_ENV_FILE));
        }

        let resolved = self.resolve(target).await?;

        Ok(match scope {
            kobune_core::EnvScope::Project => {
                kobune_core::env::project_env_path(&resolved.repo.main_root)
            }
            _ => kobune_core::env::workspace_env_path(&resolved.workspace.path),
        })
    }
    /// Settles the environment a service receives.
    ///
    /// Stacks the layers and resolves secret references. **A resolved
    /// value never touches disk**, here or anywhere after; it exists only
    /// to be handed to the container.
    ///
    /// **Settling only.** Writing an `env_file` belongs to starting a
    /// service, not to working out what its environment would be — this
    /// is asked about services nobody is starting.
    pub(super) async fn service_env(
        &self,
        config: &KobuneConfig,
        project: &str,
        record: &WorkspaceRecord,
        project_root: &std::path::Path,
        service: &str,
        events: &EventSink,
    ) -> Result<ServiceEnv, ApiError> {
        let layers = env::layers_for_service(
            config,
            project,
            record,
            project_root,
            Some(service),
            &self.paths,
            &self.gateway,
        )
        .map_err(|err| ApiError::new(ErrorCode::InvalidConfig, err.to_string()))?;

        let entries = layers.resolve().map_err(env::resolution_error)?;

        // `$NAME` is passed through as written — right for a value on its
        // way to a shell, a mistake everywhere else. Saying so where the
        // name is one Kobune has costs a line and saves an afternoon:
        // otherwise a directory called `$KOBUNE_CACHE_DIR` appears in the
        // worktree and nothing connects it back to here.
        //
        // **Read from the values as written**, not from the settled ones:
        // by then `$$NAME` has become `$NAME`, and a reference has carried
        // one value's mistake into every value built out of it.
        let written = layers.unexpanded();
        for entry in &written {
            for name in kobune_core::env::bare_references(&entry.raw)
                .into_iter()
                .filter(|name| written.iter().any(|other| other.key == *name))
            {
                let message = format!(
                    "{}: {} contains ${name}, which is not expanded. Write ${{{name}}} to refer to it",
                    service, entry.key
                );
                events.warn(message.clone());
                tracing::warn!("{message}");
            }
        }

        // Split references from plain values.
        let mut values = BTreeMap::new();
        let mut references = Vec::new();

        for entry in &entries {
            match entry.secret_ref() {
                Some(reference) => references.push((entry.key.clone(), reference)),
                None => {
                    values.insert(entry.key.clone(), entry.raw.clone());
                }
            }
        }

        if references.is_empty() {
            return Ok(ServiceEnv { values, entries });
        }

        let resolved = secrets::resolve(&references).await;
        values.extend(resolved.values);

        // What did not resolve is dropped, but never quietly. The worst
        // outcome is nobody noticing and wondering why authentication
        // keeps failing.
        for (key, reason) in resolved.failures {
            events.warn(format!("cannot resolve the secret for {key}: {reason}"));
            tracing::warn!("{service}: cannot resolve the secret for {key}: {reason}");
        }

        Ok(ServiceEnv { values, entries })
    }
    /// The environments for every service in a workspace.
    pub(super) async fn workspace_envs(
        &self,
        config: &KobuneConfig,
        project: &str,
        record: &WorkspaceRecord,
        project_root: &std::path::Path,
        events: &EventSink,
    ) -> Result<BTreeMap<String, ServiceEnv>, ApiError> {
        let mut envs = BTreeMap::new();

        for name in config.services.keys() {
            let settled = self
                .service_env(config, project, record, project_root, name, events)
                .await?;
            envs.insert(name.clone(), settled);
        }

        Ok(envs)
    }
}

/// One service's settled environment.
///
/// Two views of the same thing. `values` is what the container is given,
/// with secrets resolved; `entries` is what it was before that, which is
/// what an `env_file` is written from — **so the file cannot say anything
/// the process was not given**, and a resolved secret still never reaches
/// disk.
pub(super) struct ServiceEnv {
    pub(super) values: BTreeMap<String, String>,
    pub(super) entries: Vec<kobune_core::env::EnvEntry>,
}

/// Writes the `env_file` of each service that is about to run.
///
/// **Only the ones being started.** Every service's file used to be
/// written whenever any environment was settled, so `kobune up web`
/// failed over `api`'s `env_file` pointing somewhere it may not write
/// — a service nobody asked to start — and `kobune exec` left files
/// behind as a side effect of running a command.
///
/// From the same values the container is about to be given, so the
/// file and the process cannot disagree.
pub(super) fn write_env_files(
    config: &KobuneConfig,
    record: &WorkspaceRecord,
    envs: &BTreeMap<String, ServiceEnv>,
    starting: &[String],
) -> Result<(), ApiError> {
    for service in starting {
        if let Some(settled) = envs.get(service) {
            write_env_file_for(config, record, service, settled)?;
        }
    }

    Ok(())
}
/// The same for one service.
pub(super) fn write_env_file_for(
    config: &KobuneConfig,
    record: &WorkspaceRecord,
    service: &str,
    settled: &ServiceEnv,
) -> Result<(), ApiError> {
    let Ok(service_config) = config.service(service) else {
        return Ok(());
    };

    let Some(relative) = &service_config.env_file else {
        return Ok(());
    };

    // Everything the container is given except what only means something
    // in there — a container path handed to a tool running on the host is
    // a warning on every start about a file that was never missing.
    let written: Vec<_> = settled
        .entries
        .iter()
        .filter(|entry| !env::container_only(entry))
        .cloned()
        .collect();

    let note = format!("service: {service}  workspace: {}", record.label);
    let contents = kobune_core::env::render(&written, &note);

    if let Some(path) = env::write_env_file(&record.path, relative, &contents)? {
        tracing::debug!("{service}: wrote {}", path.display());
    }

    Ok(())
}
/// Just the values, for building a spec.
pub(super) fn env_values(
    envs: &BTreeMap<String, ServiceEnv>,
) -> BTreeMap<String, BTreeMap<String, String>> {
    envs.iter()
        .map(|(name, settled)| (name.clone(), settled.values.clone()))
        .collect()
}
/// Says the value was written, whatever the listing that follows does.
///
/// **`env set` and `env unset` answer with a listing, and settling the
/// layers can fail** — a `${...}` somewhere refers to a name nothing sets.
/// The value is on disk by then, so an error that only described the
/// listing would read as though nothing had been written, and invite the
/// same command again.
pub(super) fn written(
    key: String,
    listing: Result<Response, ApiError>,
) -> Result<Response, ApiError> {
    listing.map_err(|err| ApiError {
        message: format!("{key} was written. {}", err.message),
        ..err
    })
}
/// Reads a file, or an empty string when there is none.
pub(super) fn read_or_empty(path: &std::path::Path) -> Result<String, ApiError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(ApiError::internal(format!(
            "cannot read {}: {err}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::tests::{SAMPLE, config, record_at};

    const TWO_ENV_FILES: &str = r#"
        [project]
        name = "myapp"
        [services.web]
        image = "node:22"
        port = 3000
        env_file = ".kobune/env.web"
        [services.api]
        image = "node:22"
        port = 8080
        env_file = ".kobune/env.api"
    "#;

    fn settled(pairs: &[(&str, &str)]) -> ServiceEnv {
        ServiceEnv {
            values: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            entries: pairs
                .iter()
                .map(|(k, v)| kobune_core::env::EnvEntry {
                    key: k.to_string(),
                    raw: v.to_string(),
                    scope: kobune_core::EnvScope::Service,
                })
                .collect(),
        }
    }
    #[test]
    fn only_the_services_being_started_get_their_file() {
        // `kobune up web` has no business writing into a path `api` alone
        // was pointed at — nor failing over one.
        let dir = tempfile::tempdir().expect("tempdir");
        let record = record_at(dir.path());

        let envs = BTreeMap::from([
            ("web".to_string(), settled(&[("A", "1")])),
            ("api".to_string(), settled(&[("B", "2")])),
        ]);

        write_env_files(&config(TWO_ENV_FILES), &record, &envs, &["web".to_string()])
            .expect("writes");

        assert!(dir.path().join(".kobune/env.web").exists());
        assert!(
            !dir.path().join(".kobune/env.api").exists(),
            "api was not asked to start"
        );
    }
    #[test]
    fn the_env_file_leaves_out_what_only_a_container_can_read() {
        // This file is read on the *host*, and Node reads
        // NODE_EXTRA_CA_CERTS by name: left in, it is a warning on every
        // start about a certificate file that was never missing.
        let dir = tempfile::tempdir().expect("tempdir");
        let record = record_at(dir.path());

        let entry = |key: &str, raw: &str, scope| kobune_core::env::EnvEntry {
            key: key.to_string(),
            raw: raw.to_string(),
            scope,
        };

        let web = ServiceEnv {
            values: BTreeMap::from([(
                "NODE_EXTRA_CA_CERTS".to_string(),
                kobune_core::config::CA_TARGET.to_string(),
            )]),
            entries: vec![
                entry(
                    "NODE_EXTRA_CA_CERTS",
                    kobune_core::config::CA_TARGET,
                    kobune_core::EnvScope::Injected,
                ),
                entry(
                    "KOBUNE_CA_FILE",
                    kobune_core::config::CA_TARGET,
                    kobune_core::EnvScope::Injected,
                ),
                entry(
                    "API_URL",
                    "https://api.myapp.localhost",
                    kobune_core::EnvScope::Service,
                ),
            ],
        };

        let envs = BTreeMap::from([("web".to_string(), web)]);

        write_env_files(&config(TWO_ENV_FILES), &record, &envs, &["web".to_string()])
            .expect("writes");

        let written = std::fs::read_to_string(dir.path().join(".kobune/env.web")).expect("reads");

        assert!(
            !written.contains("NODE_EXTRA_CA_CERTS"),
            "the container has it from its environment; the host cannot use it: {written}"
        );
        assert!(
            written.contains("KOBUNE_CA_FILE"),
            "a path nothing reads by name is the project's to use: {written}"
        );
        assert!(written.contains("API_URL="), "{written}");
    }
    #[test]
    fn another_services_env_file_cannot_fail_this_start() {
        // The whole point: `api` pointing at a file Kobune may not write
        // used to take `kobune up web` down with it.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".kobune")).expect("creates");
        std::fs::write(dir.path().join(".kobune/env.api"), "MINE=1\n").expect("writes");

        let record = record_at(dir.path());
        let envs = BTreeMap::from([
            ("web".to_string(), settled(&[("A", "1")])),
            ("api".to_string(), settled(&[("B", "2")])),
        ]);

        let config = config(TWO_ENV_FILES);

        write_env_files(&config, &record, &envs, &["web".to_string()]).expect("web still starts");

        assert_eq!(
            std::fs::read_to_string(dir.path().join(".kobune/env.api")).expect("reads"),
            "MINE=1\n",
            "and api's file is left exactly as it was"
        );

        // Starting `api` itself is a different matter: that one is asked
        // for, so the refusal belongs to it.
        assert!(
            write_env_files(&config, &record, &envs, &["api".to_string()]).is_err(),
            "the service that was asked for still answers for its own file"
        );
    }
    #[test]
    fn a_service_without_env_file_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let record = record_at(dir.path());
        let envs = BTreeMap::from([("web".to_string(), settled(&[("A", "1")]))]);

        write_env_files(&config(SAMPLE), &record, &envs, &["web".to_string()]).expect("writes");

        assert!(
            !dir.path().join(".kobune").exists(),
            "nothing asked for, nothing made"
        );
    }
}

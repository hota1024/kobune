//! Saying which configuration layers were read, and what each of them set.
//!
//! **Three layers, later winning**: the machine's `config.toml`, the
//! project's committed `kobune.toml`, and the clone's gitignored
//! `kobune.local.toml`. The merged document exists nowhere on disk, so
//! without this there is no file anybody can open to find out why a value
//! is what it is.

use kobune_api::{ApiError, ConfigInfo, ConfigLayerInfo, ConfigValueInfo, Response, Target};
use kobune_core::config::ConfigOrigin;
use kobune_core::{KobuneConfig, Repository};

use super::Supervisor;

impl Supervisor {
    /// What `kobune config show` reports.
    ///
    /// **Deliberately does not resolve the project first.** Every other
    /// operation starts by reading the configuration, which is exactly the
    /// step that fails when somebody needs this — so it goes straight from
    /// the git repository to [`KobuneConfig::inspect`], which reports a
    /// merge that will not load rather than failing on it. Nothing here
    /// registers the project either: a read that explains a broken
    /// configuration should not be writing to the state store.
    pub(super) async fn config_show(
        &self,
        target: Target,
        all: bool,
    ) -> Result<Response, ApiError> {
        let repo = Repository::discover(&target.cwd).map_err(ApiError::from)?;

        let report = KobuneConfig::inspect(&repo.root, &repo.main_root, self.paths.root())
            .map_err(ApiError::from)?;

        let layers = report
            .sources
            .iter()
            .map(|source| ConfigLayerInfo {
                layer: source.layer,
                path: source.path.clone(),
                loaded: source.loaded,
            })
            .collect();

        let overrides = report
            .overrides()
            .map(|(key, origin)| value_info(key, origin))
            .collect();

        // Only when asked. A configuration of any size has hundreds of
        // leaves, and burying the four contested ones among them would
        // undo the point of showing them at all.
        let values = match all {
            true => report
                .origins
                .iter()
                .map(|(key, origin)| value_info(key, origin))
                .collect(),
            false => Vec::new(),
        };

        Ok(Response::Config(ConfigInfo {
            layers,
            overrides,
            values,
            all,
            problem: report.problem,
        }))
    }
}

fn value_info(key: &str, origin: &ConfigOrigin) -> ConfigValueInfo {
    ConfigValueInfo {
        key: key.to_string(),
        value: match holds_a_secret(key) {
            true => kobune_core::env::mask(&origin.value),
            false => origin.value.clone(),
        },
        layer: origin.layer,
        overridden: origin.overridden.clone(),
    }
}

/// Whether a key's value is the sort of thing that must not be printed.
///
/// **This command answers "which layer", not "what value".** The layer is
/// the whole point and a mask does not obscure it, so the two keys under
/// `kobune.toml` that people put tokens in are masked here with no way to
/// lift it — `kobune env ls --service <name> --reveal` is where a value is
/// asked for by name, and it says so out loud.
///
/// Without this the new command would be more revealing than the old one
/// over the same data: `env ls` masks these unless `--reveal`, and
/// `kobune.local.toml` is gitignored precisely so it can hold a real
/// token. `config show` output is the sort of thing pasted into an issue.
fn holds_a_secret(key: &str) -> bool {
    let mut parts = key.split('.');

    // `services.<name>.env.<KEY>` and `services.<name>.build_args.<KEY>`.
    // A `<KEY>` of its own may contain dots, so the tail is not counted.
    parts.next() == Some("services")
        && parts.next().is_some()
        && matches!(parts.next(), Some("env" | "build_args"))
        && parts.next().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_env_value_is_never_printed() {
        // `env ls` masks these unless `--reveal`, and this command has no
        // such gate — so a new surface onto the same data must not be the
        // more revealing of the two.
        for key in [
            "services.web.env.API_TOKEN",
            "services.web.build_args.NPM_TOKEN",
            "services.web.env.WITH.DOTS",
        ] {
            assert!(holds_a_secret(key), "{key}");
        }
    }

    #[test]
    fn the_keys_worth_reading_are_left_alone() {
        // Masking the runtime or an image would defeat the command.
        for key in [
            "runtime.default",
            "project.name",
            "services.web.image",
            "services.web.port",
            // The tables themselves, before a key under them.
            "services.web.env",
            "services.web.build_args",
        ] {
            assert!(!holds_a_secret(key), "{key}");
        }
    }
}

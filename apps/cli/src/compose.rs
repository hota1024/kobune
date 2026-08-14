//! Turning a `docker-compose.yml` into a `kobune.toml`.
//!
//! **Not a complete conversion, and it does not pretend to be.** Compose
//! is enormous and half of it means nothing here — there is no `restart`
//! policy when the daemon owns the lifecycle, no `networks` when Kobune
//! wires them, no `deploy` at all. Converting what maps and saying
//! nothing about the rest would produce a file that looks finished and is
//! not, which is worse than no conversion: the failure arrives later,
//! somewhere else, as a service that behaves differently from the one the
//! author was running yesterday.
//!
//! So everything lands in one of three places, and every key is in
//! exactly one of them:
//!
//! - **converted** — written into `kobune.toml`
//! - **left to you** — what compose cannot say, written into the file as
//!   a `TODO` comment beside the service it belongs to
//! - **dropped** — named in the report, per service, never in silence
//!
//! The point is to turn trying Kobune from *rewriting a working file*
//! into *reviewing a generated one*.

use std::collections::BTreeMap;

use yaml_rust2::{Yaml, YamlLoader};

/// The names compose is found under, in the order compose itself looks.
pub const CANDIDATES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

/// What a conversion produced.
#[derive(Debug)]
pub struct Converted {
    /// The `kobune.toml` to write.
    pub toml: String,
    /// Keys that had no equivalent, per service. Reported, never silent.
    pub dropped: Vec<Dropped>,
    /// Files compose read environments from, which become `carry`.
    pub carried: Vec<String>,
}

/// A key that did not survive, and the service it was on.
#[derive(Debug)]
pub struct Dropped {
    pub service: String,
    pub key: String,
}

/// Keys Kobune has an answer for. Everything else is reported.
const CONVERTED_KEYS: [&str; 12] = [
    "image",
    "build",
    "ports",
    "expose",
    "command",
    "environment",
    "env_file",
    "depends_on",
    "volumes",
    "healthcheck",
    "working_dir",
    "tty",
];

/// Keys worth saying nothing about, because they carry no information a
/// reader would miss.
///
/// `container_name` is the clearest: Kobune names containers itself, from
/// the project and the worktree, and it has to — two worktrees of one
/// repository cannot both be `myapp_web`.
const QUIETLY_IGNORED: [&str; 4] = ["container_name", "hostname", "stdin_open", "platform"];

#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    #[error("{path} is not valid YAML: {source}")]
    Parse {
        path: String,
        #[source]
        source: yaml_rust2::ScanError,
    },

    #[error("{path} has no `services` block, so there is nothing to convert")]
    NoServices { path: String },

    #[error("{path} defines no services")]
    Empty { path: String },
}

/// Converts the compose document in `yaml`, for a project named `project`.
pub fn convert(project: &str, path: &str, yaml: &str) -> Result<Converted, ComposeError> {
    let documents = YamlLoader::load_from_str(yaml).map_err(|source| ComposeError::Parse {
        path: path.to_string(),
        source,
    })?;

    let root = documents.first().ok_or_else(|| ComposeError::Empty {
        path: path.to_string(),
    })?;

    let services = root["services"]
        .as_hash()
        .ok_or_else(|| ComposeError::NoServices {
            path: path.to_string(),
        })?;

    if services.is_empty() {
        return Err(ComposeError::Empty {
            path: path.to_string(),
        });
    }

    let mut dropped = Vec::new();
    let mut carried = Vec::new();
    let mut blocks = Vec::new();

    // Read once up front: rewriting `http://api:8080` into
    // `${KOBUNE_URL_API}` needs to know which names are services here and
    // which are somebody's real hostname.
    let names: Vec<String> = services
        .keys()
        .filter_map(|name| name.as_str().map(str::to_string))
        .collect();

    for (name, body) in services {
        let Some(name) = name.as_str() else { continue };
        let mut service = Service::read(name, body, &mut dropped, &mut carried);
        service.point_urls_at_kobune(&names);
        blocks.push(service.render());
    }

    carried.sort();
    carried.dedup();

    Ok(Converted {
        toml: render(project, &carried, &blocks),
        dropped,
        carried,
    })
}

/// One service, as far as Kobune can describe it.
#[derive(Default)]
struct Service {
    name: String,
    image: Option<String>,
    build: Option<String>,
    dockerfile: Option<String>,
    build_args: BTreeMap<String, String>,
    port: Option<u16>,
    /// Compose published nothing, so nothing here should have a URL.
    unexposed: bool,
    command: Option<String>,
    env: BTreeMap<String, String>,
    depends_on: Vec<String>,
    volumes: Vec<String>,
    health: Option<String>,
    workdir: Option<String>,
    tty: bool,
    /// What the reader has to decide, written beside the service.
    todos: Vec<String>,
}

impl Service {
    fn read(
        name: &str,
        body: &Yaml,
        dropped: &mut Vec<Dropped>,
        carried: &mut Vec<String>,
    ) -> Self {
        let mut service = Self {
            name: name.to_string(),
            ..Self::default()
        };

        if let Some(hash) = body.as_hash() {
            for (key, value) in hash {
                let Some(key) = key.as_str() else { continue };

                if QUIETLY_IGNORED.contains(&key) {
                    continue;
                }

                if !CONVERTED_KEYS.contains(&key) {
                    dropped.push(Dropped {
                        service: name.to_string(),
                        key: key.to_string(),
                    });
                    continue;
                }

                service.take(key, value, carried);
            }
        }

        service.finish();
        service
    }

    fn take(&mut self, key: &str, value: &Yaml, carried: &mut Vec<String>) {
        match key {
            "image" => self.image = value.as_str().map(str::to_string),
            "build" => self.take_build(value),
            "ports" => self.port = first_container_port(value),
            // Compose's `expose` is "open to other services, not to the
            // host", which is what `expose = false` means here.
            "expose" => {
                if self.port.is_none() {
                    self.port = first_port(value);
                }
                self.unexposed = true;
            }
            "command" => self.command = command_of(value),
            "environment" => self.env = pairs_of(value),
            // **The one that would have destroyed data.** Compose reads
            // this file into the environment; Kobune writes the settled
            // environment out to it. Mapped across, `up` would overwrite
            // the user's `.env`. What it actually means here is a file the
            // worktree needs that git does not carry.
            "env_file" => carried.extend(strings_of(value)),
            "depends_on" => self.depends_on = depends_on_of(value),
            "volumes" => self.volumes = strings_of(value),
            "healthcheck" => self.health = health_of(value),
            "working_dir" => self.workdir = value.as_str().map(str::to_string),
            "tty" => self.tty = value.as_bool().unwrap_or(false),
            _ => {}
        }
    }

    fn take_build(&mut self, value: &Yaml) {
        if let Some(context) = value.as_str() {
            self.build = Some(context.to_string());
            return;
        }

        let Some(hash) = value.as_hash() else { return };

        for (key, inner) in hash {
            match key.as_str() {
                Some("context") => self.build = inner.as_str().map(str::to_string),
                Some("dockerfile") => self.dockerfile = inner.as_str().map(str::to_string),
                Some("args") => self.build_args = pairs_of(inner),
                _ => {}
            }
        }
    }

    /// The decisions compose has no way to express.
    fn finish(&mut self) {
        if self.port.is_none() {
            self.todos.push(
                "no port was published, so this service gets no URL. Add `port` \
                 if something should reach it"
                    .to_string(),
            );
        }

        if self.health.is_none() && self.port.is_some() {
            self.todos.push(
                "add `health` so `ready` means serving rather than started — \
                 scale-to-zero waits on it"
                    .to_string(),
            );
        }

        // Not inferable, and the one that bites: a database shared across
        // worktrees is `scope = \"project\"`, and one per worktree is the
        // default. Compose has no worktrees, so it says nothing either way.
        if self.looks_like_a_datastore() {
            self.todos.push(
                "one of these per worktree, or one shared by the project? \
                 `scope = \"project\"` shares it"
                    .to_string(),
            );
        }
    }

    /// Turns compose's way of reaching a sibling into Kobune's.
    ///
    /// **`http://api:8080` is faithful and wrong.** It is how compose
    /// names another service, and here it bypasses the proxy, hands the
    /// application a different URL from the one the browser uses — which
    /// is what `KOBUNE_URL_*` exists to prevent — and does not resolve at
    /// all under Apple Container, which has no container-to-container
    /// DNS.
    ///
    /// Only rewritten when the host is a service in this same file. A
    /// value pointing at something outside it is somebody's real
    /// hostname and is left alone.
    ///
    /// Found by an agent that had never seen this codebase: writing the
    /// configuration by hand from the Skill, it got this right, and the
    /// converter meant to save that work got it wrong.
    fn point_urls_at_kobune(&mut self, services: &[String]) {
        for value in self.env.values_mut() {
            let Some(rest) = value
                .strip_prefix("http://")
                .or_else(|| value.strip_prefix("https://"))
            else {
                continue;
            };

            // `api:8080/v1` — the name is everything before the port or
            // the path, whichever comes first.
            let host = rest
                .split(['/', ':'])
                .next()
                .unwrap_or_default()
                .to_string();

            if host.is_empty() || !services.contains(&host) {
                continue;
            }

            let variable = host.to_uppercase().replace('-', "_");
            let path = rest[host.len()..]
                .split_once('/')
                .map(|(_, path)| format!("/{path}"))
                .unwrap_or_default();

            *value = format!("${{KOBUNE_URL_{variable}}}{path}");
        }
    }

    /// A guess, and offered as a question rather than an answer.
    fn looks_like_a_datastore(&self) -> bool {
        let image = self.image.as_deref().unwrap_or_default();

        ["postgres", "mysql", "mariadb", "redis", "mongo", "valkey"]
            .iter()
            .any(|known| image.starts_with(known) || image.contains(&format!("/{known}")))
    }

    fn render(&self) -> String {
        let mut out = String::new();

        for todo in &self.todos {
            out.push_str(&format!("# TODO: {todo}\n"));
        }

        out.push_str(&format!("[services.{}]\n", self.name));

        if let Some(image) = &self.image {
            out.push_str(&format!("image = {}\n", quote(image)));
        }
        if let Some(build) = &self.build {
            out.push_str(&format!("build = {}\n", quote(build)));
        }
        if let Some(dockerfile) = &self.dockerfile {
            out.push_str(&format!("dockerfile = {}\n", quote(dockerfile)));
        }
        if let Some(port) = self.port {
            out.push_str(&format!("port = {port}\n"));
        }
        if self.unexposed {
            out.push_str("expose = false\n");
        }
        if let Some(command) = &self.command {
            out.push_str(&format!("command = {}\n", quote(command)));
        }
        if let Some(workdir) = &self.workdir {
            out.push_str(&format!("workdir = {}\n", quote(workdir)));
        }
        if let Some(health) = &self.health {
            out.push_str(&format!("health = {}\n", quote(health)));
        }
        if self.tty {
            out.push_str("tty = true\n");
        }
        if !self.depends_on.is_empty() {
            out.push_str(&format!("depends_on = {}\n", array(&self.depends_on)));
        }
        if !self.volumes.is_empty() {
            out.push_str(&format!("volumes = {}\n", array(&self.volumes)));
        }
        if !self.build_args.is_empty() {
            out.push('\n');
            out.push_str(&format!("[services.{}.build_args]\n", self.name));
            for (key, value) in &self.build_args {
                out.push_str(&format!("{key} = {}\n", quote(value)));
            }
        }
        if !self.env.is_empty() {
            out.push('\n');
            out.push_str(&format!("[services.{}.env]\n", self.name));
            for (key, value) in &self.env {
                out.push_str(&format!("{key} = {}\n", quote(value)));
            }
        }

        out
    }
}

fn render(project: &str, carried: &[String], blocks: &[String]) -> String {
    let mut out = String::new();

    out.push_str("# Converted from compose by `kobune init --from-compose`.\n");
    out.push_str("# Every key: https://minato.1024.works/reference/kobune-toml\n");
    out.push_str("#\n");
    out.push_str("# Read the TODOs before the first `kobune up`. They are the\n");
    out.push_str("# decisions compose had no way to express.\n\n");

    out.push_str("[project]\n");
    out.push_str(&format!("name = {}\n", quote(project)));

    if !carried.is_empty() {
        out.push_str(
            "# From compose's `env_file`. **Not `env_file` here** — that one\n\
             # writes rather than reads, and would overwrite these. `carry`\n\
             # copies them into each new worktree, which is what they were for.\n",
        );
        out.push_str(&format!("carry = {}\n", array(carried)));
    }

    out.push_str("\n[runtime]\ndefault = \"docker\"\n\n");
    out.push_str(&blocks.join("\n"));

    out
}

/// The container side of `"3000:3000"`, or of a bare `3000`.
fn first_container_port(value: &Yaml) -> Option<u16> {
    let entries = value.as_vec()?;

    entries.iter().find_map(|entry| {
        if let Some(port) = entry.as_i64() {
            return u16::try_from(port).ok();
        }

        let text = entry.as_str()?;
        // `8080:80`, `127.0.0.1:8080:80`, `80`, and any of them with
        // `/tcp` on the end. The container's port is the last number.
        let without_protocol = text.split('/').next().unwrap_or(text);

        without_protocol
            .rsplit(':')
            .next()
            .and_then(|port| port.parse().ok())
    })
}

/// A bare port list, as `expose` takes.
fn first_port(value: &Yaml) -> Option<u16> {
    let entries = value.as_vec()?;

    entries.iter().find_map(|entry| {
        entry
            .as_i64()
            .and_then(|port| u16::try_from(port).ok())
            .or_else(|| entry.as_str().and_then(|text| text.parse().ok()))
    })
}

/// `command` is a string or a list. Kobune takes a string and splits it
/// shell-style, so a list is joined back.
fn command_of(value: &Yaml) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }

    let parts = strings_of(value);
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// `environment` is a map or a list of `KEY=value`.
fn pairs_of(value: &Yaml) -> BTreeMap<String, String> {
    let mut pairs = BTreeMap::new();

    if let Some(hash) = value.as_hash() {
        for (key, inner) in hash {
            if let Some(key) = key.as_str() {
                pairs.insert(key.to_string(), scalar(inner));
            }
        }
        return pairs;
    }

    for entry in strings_of(value) {
        match entry.split_once('=') {
            Some((key, value)) => {
                pairs.insert(key.to_string(), value.to_string());
            }
            // `- KEY` with no value takes it from the daemon's own
            // environment, which is `env://` here.
            None => {
                pairs.insert(entry.clone(), format!("env://{entry}"));
            }
        }
    }

    pairs
}

/// `depends_on` is a list, or a map keyed by service with conditions.
fn depends_on_of(value: &Yaml) -> Vec<String> {
    if let Some(hash) = value.as_hash() {
        return hash
            .iter()
            .filter_map(|(key, _)| key.as_str().map(str::to_string))
            .collect();
    }

    strings_of(value)
}

/// `healthcheck.test`, as Kobune's `cmd:` form.
///
/// Only `cmd:` — a compose health check is a command, and the `http://`
/// form Kobune also takes is a different thing that cannot be derived
/// from one.
fn health_of(value: &Yaml) -> Option<String> {
    let test = &value["test"];

    if let Some(text) = test.as_str() {
        return Some(format!("cmd:{text}"));
    }

    let mut parts = strings_of(test);
    if parts.is_empty() {
        return None;
    }

    // `["CMD", "curl", "-f", "…"]` and `["CMD-SHELL", "…"]`.
    match parts.first().map(String::as_str) {
        Some("CMD") => {
            parts.remove(0);
        }
        Some("CMD-SHELL") => {
            parts.remove(0);
            return Some(format!("cmd:sh -c {}", shell_quote(&parts.join(" "))));
        }
        Some("NONE") => return None,
        _ => {}
    }

    Some(format!("cmd:{}", parts.join(" ")))
}

/// Every string in a scalar or a sequence.
fn strings_of(value: &Yaml) -> Vec<String> {
    if let Some(text) = value.as_str() {
        return vec![text.to_string()];
    }

    value
        .as_vec()
        .map(|entries| entries.iter().map(scalar).collect())
        .unwrap_or_default()
}

/// A YAML scalar as the string a TOML value wants.
fn scalar(value: &Yaml) -> String {
    match value {
        Yaml::String(text) => text.clone(),
        Yaml::Integer(number) => number.to_string(),
        Yaml::Boolean(flag) => flag.to_string(),
        Yaml::Real(number) => number.clone(),
        _ => String::new(),
    }
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn array(values: &[String]) -> String {
    let quoted: Vec<String> = values.iter().map(|value| quote(value)).collect();
    format!("[{}]", quoted.join(", "))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert_ok(yaml: &str) -> Converted {
        convert("myapp", "compose.yaml", yaml).expect("converts")
    }

    #[test]
    fn converts_the_shape_most_projects_have() {
        let out = convert_ok(
            r#"
services:
  web:
    image: node:22
    ports: ["3000:3000"]
    command: npm run dev
    depends_on: [api]
    environment:
      NODE_ENV: development
"#,
        );

        assert!(out.toml.contains("[services.web]"), "{}", out.toml);
        assert!(out.toml.contains(r#"image = "node:22""#));
        assert!(out.toml.contains("port = 3000"));
        assert!(out.toml.contains(r#"command = "npm run dev""#));
        assert!(out.toml.contains(r#"depends_on = ["api"]"#));
        assert!(out.toml.contains(r#"NODE_ENV = "development""#));
    }

    #[test]
    fn env_file_becomes_carry_and_never_env_file() {
        // **The one that would have destroyed data.** Compose reads this
        // file; Kobune's `env_file` writes it. Mapped across, the first
        // `up` would overwrite the user's `.env`.
        let out = convert_ok(
            r#"
services:
  api:
    image: node:22
    env_file: .env
"#,
        );

        // Checked on the parsed configuration rather than the text: the
        // comment beside `carry` says the words "env_file" on purpose,
        // and a test that could not tell the two apart would be pinning
        // the prose instead of the behaviour.
        let config: kobune_core::config::KobuneConfig =
            toml::from_str(&out.toml).unwrap_or_else(|err| panic!("{err}\n{}", out.toml));

        assert!(
            config
                .services
                .values()
                .all(|service| service.env_file.is_none()),
            "compose's env_file must not become Kobune's: {}",
            out.toml
        );
        assert_eq!(config.project.carry, vec![".env".to_string()]);
        assert_eq!(out.carried, vec![".env".to_string()]);
    }

    #[test]
    fn the_container_side_of_a_port_is_the_one_that_matters() {
        // Kobune publishes on a port it chooses; what it needs is the one
        // the app listens on inside.
        for (ports, expected) in [
            ("[\"3000:3000\"]", 3000),
            ("[\"8080:80\"]", 80),
            ("[\"127.0.0.1:8080:80\"]", 80),
            ("[\"8080:80/tcp\"]", 80),
            ("[3000]", 3000),
        ] {
            let out = convert_ok(&format!(
                "services:\n  web:\n    image: nginx\n    ports: {ports}\n"
            ));
            assert!(
                out.toml.contains(&format!("port = {expected}")),
                "{ports} should give {expected}: {}",
                out.toml
            );
        }
    }

    #[test]
    fn compose_expose_means_no_url() {
        // Compose's `expose` is "reachable by other services, not by the
        // host", which is exactly what `expose = false` says here.
        let out = convert_ok(
            r#"
services:
  db:
    image: postgres:16
    expose: [5432]
"#,
        );

        assert!(out.toml.contains("port = 5432"), "{}", out.toml);
        assert!(out.toml.contains("expose = false"), "{}", out.toml);
    }

    #[test]
    fn every_shape_compose_allows_for_the_same_key() {
        // environment as a list, depends_on as a map, command as a list.
        let out = convert_ok(
            r#"
services:
  api:
    image: node:22
    command: ["node", "server.js"]
    environment:
      - NODE_ENV=production
      - FROM_THE_DAEMON
    depends_on:
      db:
        condition: service_healthy
"#,
        );

        assert!(
            out.toml.contains(r#"command = "node server.js""#),
            "{}",
            out.toml
        );
        assert!(out.toml.contains(r#"NODE_ENV = "production""#));
        assert!(
            out.toml
                .contains(r#"FROM_THE_DAEMON = "env://FROM_THE_DAEMON""#),
            "a bare name takes it from the daemon's environment: {}",
            out.toml
        );
        assert!(out.toml.contains(r#"depends_on = ["db"]"#));
    }

    #[test]
    fn a_build_block_becomes_its_three_keys() {
        let out = convert_ok(
            r#"
services:
  web:
    build:
      context: ./web
      dockerfile: Dockerfile.dev
      args:
        NODE_VERSION: "22"
"#,
        );

        assert!(out.toml.contains(r#"build = "./web""#), "{}", out.toml);
        assert!(out.toml.contains(r#"dockerfile = "Dockerfile.dev""#));
        assert!(out.toml.contains("[services.web.build_args]"));
        assert!(out.toml.contains(r#"NODE_VERSION = "22""#));
    }

    #[test]
    fn a_health_check_keeps_its_shape() {
        let cases = [
            (
                r#"test: ["CMD", "pg_isready", "-U", "postgres"]"#,
                "cmd:pg_isready -U postgres",
            ),
            (
                r#"test: ["CMD-SHELL", "curl -f http://localhost || exit 1"]"#,
                "cmd:sh -c 'curl -f http://localhost || exit 1'",
            ),
        ];

        for (test, expected) in cases {
            let out = convert_ok(&format!(
                "services:\n  db:\n    image: postgres:16\n    ports: [5432]\n    healthcheck:\n      {test}\n"
            ));
            assert!(
                out.toml.contains(expected),
                "{test} should give {expected}: {}",
                out.toml
            );
        }
    }

    #[test]
    fn a_url_pointing_at_a_sibling_becomes_the_one_kobune_issues() {
        // Compose reaches a sibling by service name. Carried across
        // verbatim that bypasses the proxy, hands the app a different URL
        // from the browser's, and does not resolve at all under Apple
        // Container. An agent writing this by hand from the Skill got it
        // right; the converter meant to save that work did not.
        let out = convert_ok(
            r#"
services:
  web:
    image: node:22
    environment:
      ROOMS_API: http://api:8080
      WITH_A_PATH: http://api:8080/v1
      SOMEBODY_ELSES: https://api.stripe.com/v1
      NOT_A_SERVICE: http://elsewhere:9000
  api:
    image: node:22
"#,
        );

        assert!(
            out.toml.contains(r#"ROOMS_API = "${KOBUNE_URL_API}""#),
            "{}",
            out.toml
        );
        assert!(
            out.toml.contains(r#"WITH_A_PATH = "${KOBUNE_URL_API}/v1""#),
            "the path has to survive: {}",
            out.toml
        );
        assert!(
            out.toml
                .contains(r#"SOMEBODY_ELSES = "https://api.stripe.com/v1""#),
            "a real hostname that merely starts with a service name is not ours: {}",
            out.toml
        );
        assert!(
            out.toml
                .contains(r#"NOT_A_SERVICE = "http://elsewhere:9000""#),
            "only names that are services in this file: {}",
            out.toml
        );
    }

    #[test]
    fn a_hyphenated_service_becomes_a_legal_variable_name() {
        // `KOBUNE_URL_` names cannot carry a hyphen, and Kobune's own
        // injection replaces it the same way.
        let out = convert_ok(
            r#"
services:
  web:
    image: node:22
    environment:
      API: http://api-server:8080
  api-server:
    image: node:22
"#,
        );

        assert!(
            out.toml.contains(r#"API = "${KOBUNE_URL_API_SERVER}""#),
            "{}",
            out.toml
        );
    }

    #[test]
    fn what_has_no_equivalent_is_named_rather_than_dropped_quietly() {
        // A generated file that looks finished and is not costs more than
        // no conversion at all.
        let out = convert_ok(
            r#"
services:
  web:
    image: nginx
    restart: unless-stopped
    networks: [frontend]
    deploy:
      replicas: 3
    logging:
      driver: json-file
"#,
        );

        let dropped: Vec<&str> = out.dropped.iter().map(|d| d.key.as_str()).collect();
        assert!(dropped.contains(&"restart"), "{dropped:?}");
        assert!(dropped.contains(&"networks"), "{dropped:?}");
        assert!(dropped.contains(&"deploy"), "{dropped:?}");
        assert!(dropped.contains(&"logging"), "{dropped:?}");
        assert!(out.dropped.iter().all(|d| d.service == "web"));
    }

    #[test]
    fn what_kobune_names_itself_is_not_reported_as_lost() {
        // `container_name` cannot survive and nobody should be asked about
        // it: two worktrees of one repository cannot share a name.
        let out = convert_ok(
            r#"
services:
  web:
    image: nginx
    container_name: myapp_web
"#,
        );

        assert!(
            out.dropped.is_empty(),
            "reporting this would be noise: {:?}",
            out.dropped.iter().map(|d| &d.key).collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_decisions_compose_cannot_express_are_left_where_they_belong() {
        let out = convert_ok(
            r#"
services:
  db:
    image: postgres:16
    ports: ["5432:5432"]
"#,
        );

        assert!(
            out.toml.contains("# TODO:") && out.toml.contains("scope"),
            "a datastore has to be asked about, not guessed: {}",
            out.toml
        );
        assert!(
            out.toml.contains("health"),
            "and readiness is what scale-to-zero waits on: {}",
            out.toml
        );

        // Beside the service, not in a paragraph at the top.
        let todo = out.toml.find("# TODO:").expect("has one");
        let block = out.toml.find("[services.db]").expect("has the service");
        assert!(todo < block, "the TODO belongs above its service");
    }

    #[test]
    fn what_it_writes_is_a_configuration_kobune_reads() {
        // The whole point: reviewing rather than rewriting. A file that
        // does not parse is neither.
        let out = convert_ok(
            r#"
services:
  web:
    build: ./web
    ports: ["3000:3000"]
    depends_on: [db]
    environment:
      DATABASE_URL: postgres://db:5432/app
  db:
    image: postgres:16
    expose: [5432]
    volumes: ["pgdata:/var/lib/postgresql/data"]
"#,
        );

        let config: kobune_core::config::KobuneConfig =
            toml::from_str(&out.toml).unwrap_or_else(|err| panic!("{err}\n---\n{}", out.toml));

        config
            .validate()
            .unwrap_or_else(|err| panic!("{err}\n---\n{}", out.toml));

        assert_eq!(config.services.len(), 2);
        assert_eq!(config.service("web").expect("web").port, Some(3000));
        assert!(!config.service("db").expect("db").exposed());
    }

    #[test]
    fn a_file_with_nothing_in_it_says_so() {
        let err = convert("myapp", "compose.yaml", "version: '3'\n").unwrap_err();
        assert!(err.to_string().contains("no `services`"), "got: {err}");
    }
}

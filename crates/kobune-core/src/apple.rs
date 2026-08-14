//! Reading Apple Container's network layout.
//!
//! A container on Apple Container's network reaches the host at that
//! network's gateway, and nowhere else — there is no `host.docker.internal`
//! and nothing forwards the host's loopback. So the gateway address is what
//! a service hostname has to point at from inside a container, and what the
//! proxy has to be listening on for that to arrive anywhere.
//!
//! Only the reading lives here. Running `container` is the runtime's
//! business — and, for the launchd socket that holds `:443` on that
//! address, the CLI's. What they share is the shape of the answer.

use std::net::Ipv4Addr;

use serde::Deserialize;

/// The Apple Container CLI.
pub const PROGRAM: &str = "container";

/// Overrides which binary is run.
///
/// For a `container` installed somewhere [`crate::program`] does not think
/// to look, and for exercising the runtime against a stub.
pub const PROGRAM_ENV: &str = "MINATO_CONTAINER";

/// The command to run, honouring [`PROGRAM_ENV`].
///
/// The installer puts `container` in `/usr/local/bin`, which is not on the
/// `PATH` a launchd daemon is handed — so the daemon has to look it up
/// rather than spawn the bare name.
pub fn program() -> String {
    crate::program::resolve_with(std::env::var(PROGRAM_ENV).ok().as_deref(), PROGRAM)
}

/// The arguments that print the networks as JSON.
pub const LIST_ARGS: [&str; 4] = ["network", "list", "--format", "json"];

/// The network Minato's services share.
///
/// Apple Container attaches a container to one network only, so everything
/// goes on the default one — see `AppleContainerRuntime::ensure_network`.
pub const DEFAULT_NETWORK: &str = "default";

/// Picks one network's gateway out of what [`LIST_ARGS`] prints.
///
/// `None` for output that is not what was expected. The CLI's JSON is
/// undocumented, and no address at all is a state every caller already has
/// to handle — Apple Container is often simply not installed.
pub fn parse_gateway(json: &str, network: &str) -> Option<Ipv4Addr> {
    let records: Vec<NetworkRecord> = serde_json::from_str(json).ok()?;

    records
        .iter()
        .find(|record| record.is(network))
        .and_then(|record| record.status.as_ref())
        .and_then(|status| status.ipv4_gateway.as_deref())
        .and_then(parse_address)
}

/// Reads an address that may carry a prefix length.
///
/// `ipv4Gateway` is a bare address today, while `ipv4Address` on a
/// container is `192.168.64.3/24`. Accepting both costs one `split` and
/// saves this from breaking if the two ever agree on a shape.
fn parse_address(raw: &str) -> Option<Ipv4Addr> {
    raw.split('/').next()?.trim().parse().ok()
}

#[derive(Debug, Deserialize)]
struct NetworkRecord {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    configuration: Option<NetworkConfiguration>,
    #[serde(default)]
    status: Option<NetworkStatus>,
}

impl NetworkRecord {
    /// Whether this is the network being asked about.
    ///
    /// The name is checked as well as the id: they hold the same string for
    /// the networks Minato uses, and neither is documented to be the one
    /// that always does.
    fn is(&self, network: &str) -> bool {
        self.id.as_deref() == Some(network)
            || self
                .configuration
                .as_ref()
                .and_then(|configuration| configuration.name.as_deref())
                == Some(network)
    }
}

#[derive(Debug, Deserialize)]
struct NetworkConfiguration {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NetworkStatus {
    #[serde(default, rename = "ipv4Gateway")]
    ipv4_gateway: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `container network list --format json` prints, trimmed.
    const LIST: &str = r#"[
      {
        "configuration": {"mode": "nat", "name": "default"},
        "id": "default",
        "status": {"ipv4Gateway": "192.168.64.1", "ipv4Subnet": "192.168.64.0/24"}
      }
    ]"#;

    #[test]
    fn reads_the_gateway_of_the_default_network() {
        assert_eq!(
            parse_gateway(LIST, DEFAULT_NETWORK),
            Some(Ipv4Addr::new(192, 168, 64, 1))
        );
    }

    #[test]
    fn a_network_that_is_not_there_has_no_gateway() {
        assert_eq!(parse_gateway(LIST, "myapp-feat-1"), None);
    }

    #[test]
    fn survives_output_that_is_not_what_was_expected() {
        for json in ["", "{}", "[]", "not json", r#"[{"id": "default"}]"#] {
            assert_eq!(parse_gateway(json, DEFAULT_NETWORK), None, "{json}");
        }
    }

    #[test]
    fn accepts_an_address_with_a_prefix_length() {
        let json = r#"[{"id": "default", "status": {"ipv4Gateway": "192.168.64.1/24"}}]"#;

        assert_eq!(
            parse_gateway(json, DEFAULT_NETWORK),
            Some(Ipv4Addr::new(192, 168, 64, 1))
        );
    }
}

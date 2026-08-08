//! Diagnosing the host-side setup, and how to fix it.
//!
//! Installing `/etc/resolver` and trusting the local CA both need root.
//! **Never run sudo unasked.** An agent doing so hangs at the password
//! prompt, and from a person's side it looks like a silent privilege
//! escalation. This module diagnoses and prints commands; running them is
//! the user's call.

use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};

use minato_api::Check;

/// Where macOS looks for resolver configuration.
pub const RESOLVER_DIR: &str = "/etc/resolver";

/// Diagnoses the system-side setup.
///
/// `dns_port` and `ca_path` are the real values, as reported by the
/// daemon.
pub fn check_system(suffix: &str, dns_port: Option<u16>, ca_path: Option<&Path>) -> Vec<Check> {
    let mut checks = vec![check_resolver(suffix, dns_port)];

    if let Some(path) = ca_path {
        checks.push(check_ca_trust(path));
    }

    // Whether a name actually resolves is the final word. A configuration
    // file can be there without having taken effect.
    checks.push(check_resolution(suffix));

    checks
}

/// Whether `/etc/resolver/{suffix}` points at Minato's DNS.
fn check_resolver(suffix: &str, dns_port: Option<u16>) -> Check {
    let path = resolver_path(suffix);
    let title = format!("DNS resolver ({})", path.display());

    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Check::fail("resolver", title, "not installed".to_string())
            .with_fix(resolver_fix(suffix, dns_port));
    };

    if !contents.contains("127.0.0.1") {
        return Check::fail("resolver", title, "does not point at 127.0.0.1".to_string())
            .with_fix(resolver_fix(suffix, dns_port));
    }

    // On an unprivileged port, a missing `port` line sends the query to
    // 53 instead.
    if let Some(port) = dns_port {
        if port != 53 && !contents.contains(&format!("port {port}")) {
            return Check::fail(
                "resolver",
                title,
                format!("DNS listens on :{port}, but there is no port line"),
            )
            .with_fix(resolver_fix(suffix, dns_port));
        }
    }

    Check::ok("resolver", title, "installed".to_string())
}

/// Whether the system trusts the CA.
fn check_ca_trust(ca_path: &Path) -> Check {
    let title = "local CA trust".to_string();

    if !ca_path.is_file() {
        return Check::warn(
            "ca-trust",
            title,
            "the CA has not been generated yet".to_string(),
        );
    }

    if !cfg!(target_os = "macos") {
        return Check::warn(
            "ca-trust",
            title,
            "cannot be checked automatically on this OS; check by hand".to_string(),
        )
        .with_fix(trust_fix(ca_path));
    }

    // verify-cert is the surest answer. A self-signed CA verifies exactly
    // when it is trusted.
    let verified = std::process::Command::new("security")
        .args(["verify-cert", "-c"])
        .arg(ca_path)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);

    if verified {
        Check::ok("ca-trust", title, "trusted".to_string())
    } else {
        Check::warn(
            "ca-trust",
            title,
            "not trusted; browsers and curl will warn over HTTPS".to_string(),
        )
        .with_fix(trust_fix(ca_path))
    }
}

/// Whether a name really resolves. Pass this and curl passes too.
///
/// **It insists on 127.0.0.1.** macOS resolves `*.localhost` to `::1` even
/// with no resolver installed, and the proxy only listens on IPv4, so that
/// does not connect. Accepting "it resolved" would let the check pass on
/// a setup that cannot actually reach anything.
fn check_resolution(suffix: &str) -> Check {
    let probe = format!("minato-doctor-probe.{suffix}");
    let title = format!("resolving {probe}");

    let addresses: Vec<std::net::IpAddr> = match (probe.as_str(), 80u16).to_socket_addrs() {
        Ok(addrs) => addrs.map(|addr| addr.ip()).collect(),
        Err(err) => {
            return Check::fail("resolution", title, err.to_string()).with_fix(format!(
                "follow `minato setup` to install /etc/resolver/{suffix}"
            ));
        }
    };

    if addresses.is_empty() {
        return Check::fail("resolution", title, "resolved to nothing".to_string()).with_fix(
            format!("follow `minato setup` to install /etc/resolver/{suffix}"),
        );
    }

    let loopback_v4 = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    if addresses.contains(&loopback_v4) {
        return Check::ok("resolution", title, "resolves to 127.0.0.1".to_string());
    }

    let rendered: Vec<String> = addresses.iter().map(|addr| addr.to_string()).collect();
    Check::fail(
        "resolution",
        title,
        format!(
            "resolved to {}. The proxy listens on 127.0.0.1, so nothing connects",
            rendered.join(", ")
        ),
    )
    .with_fix(format!(
        "follow `minato setup` to install /etc/resolver/{suffix}"
    ))
}

pub fn resolver_path(suffix: &str) -> PathBuf {
    Path::new(RESOLVER_DIR).join(suffix)
}

/// What goes in the resolver file.
pub fn resolver_contents(dns_port: Option<u16>) -> String {
    let port = dns_port.unwrap_or(53);

    if port == 53 {
        "nameserver 127.0.0.1\n".to_string()
    } else {
        // On an unprivileged port, say so explicitly. This line is what
        // keeps DNS out of root's hands.
        format!("nameserver 127.0.0.1\nport {port}\n")
    }
}

/// The command that installs the resolver file.
pub fn resolver_command(suffix: &str, dns_port: u16) -> String {
    resolver_fix(suffix, Some(dns_port))
}

fn resolver_fix(suffix: &str, dns_port: Option<u16>) -> String {
    let path = resolver_path(suffix);
    let contents = resolver_contents(dns_port);

    format!(
        "sudo mkdir -p {RESOLVER_DIR} && printf '{}' | sudo tee {} >/dev/null",
        contents.replace('\n', "\\n"),
        path.display()
    )
}

/// The command that trusts the CA.
pub fn trust_command(ca_path: &Path) -> String {
    trust_fix(ca_path)
}

fn trust_fix(ca_path: &Path) -> String {
    if cfg!(target_os = "macos") {
        format!(
            "sudo security add-trusted-cert -d -r trustRoot \
             -k /Library/Keychains/System.keychain {}",
            ca_path.display()
        )
    } else {
        format!(
            "sudo cp {} /usr/local/share/ca-certificates/minato-ca.crt \
             && sudo update-ca-certificates",
            ca_path.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minato_api::CheckStatus;

    #[test]
    fn resolver_contents_include_port_when_non_standard() {
        // The port line is what lets DNS run unprivileged.
        assert_eq!(resolver_contents(Some(53)), "nameserver 127.0.0.1\n");
        assert_eq!(
            resolver_contents(Some(15353)),
            "nameserver 127.0.0.1\nport 15353\n"
        );
    }

    #[test]
    fn resolver_path_is_per_suffix() {
        assert_eq!(
            resolver_path("localhost"),
            PathBuf::from("/etc/resolver/localhost")
        );
    }

    #[test]
    fn missing_resolver_is_a_failure_with_a_command() {
        let check = check_resolver("definitely-not-a-real-suffix", Some(15353));

        assert_eq!(check.status, CheckStatus::Fail);
        let fix = check.fix.expect("needs a fix");
        assert!(fix.contains("sudo"), "got: {fix}");
        assert!(
            fix.contains("port 15353") || fix.contains("port\\n"),
            "got: {fix}"
        );
    }

    #[test]
    fn resolution_to_ipv6_only_is_a_failure() {
        // macOS answers *.localhost with ::1 even with no resolver
        // installed. Accepting that would pass a setup that cannot
        // connect.
        let check = check_resolution("localhost");

        if check.status == CheckStatus::Ok {
            assert!(
                check.detail.contains("127.0.0.1"),
                "only an IPv4 loopback answer counts as OK: {}",
                check.detail
            );
        } else {
            assert!(check.fix.is_some(), "a fix comes with it");
        }
    }

    #[test]
    fn resolver_command_targets_the_effective_port() {
        // After launchd it is :53. Writing the earlier port stops it
        // resolving.
        let command = resolver_command("localhost", 53);
        assert!(command.contains("nameserver 127.0.0.1"));
        assert!(
            !command.contains("port "),
            "no port line needed for 53: {command}"
        );

        let command = resolver_command("localhost", 15353);
        assert!(command.contains("port 15353"), "got: {command}");
    }

    #[test]
    fn trust_command_targets_the_system_keychain_on_macos() {
        let fix = trust_fix(Path::new("/tmp/minato-ca.crt"));

        if cfg!(target_os = "macos") {
            assert!(fix.contains("add-trusted-cert"), "got: {fix}");
            assert!(fix.contains("/tmp/minato-ca.crt"), "got: {fix}");
        } else {
            assert!(fix.contains("update-ca-certificates"), "got: {fix}");
        }
    }
}

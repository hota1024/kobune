//! Generating the launchd plist.
//!
//! Ports 80, 443 and 53 cannot be bound without root. Letting launchd bind
//! them as root and hand over nothing but the file descriptors keeps **the
//! daemon itself unprivileged**. `UserName` is what makes that work:
//! launchd opens the sockets as root, and the process runs as the user.
//!
//! Writing the plist needs no privileges, so that happens here. Installing
//! it into `/Library/LaunchDaemons` needs sudo, so all that comes back is
//! the commands to run.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

// The daemon and the client have to agree on where the plist lives and what
// the job is called, so those live in `minato-core`. Generating the plist is
// this side's job alone.
pub use minato_core::launchd::{INSTALL_DIR, LABEL};

/// The shape of the plist this build writes.
///
/// **Bump it whenever the generated plist gains something an installed one
/// would not have** — a socket, a key launchd reads, a different program
/// to run. It goes into the file as a comment, and comparing it with what
/// is installed is how a build that has just landed works out that
/// `minato setup` has something new to do (see [`crate::followup`]).
///
/// Leaving it alone when the shape does move costs nothing but the notice:
/// the plist already there keeps working exactly as well as it did.
pub const PLIST_REVISION: u32 = 1;

/// How the revision appears in the file.
const REVISION_MARKER: &str = "minato plist revision";

/// The revision an installed plist was written by.
///
/// `None` for a plist from before the marker existed, and for one edited
/// into a shape this cannot read. Both are left alone deliberately: an
/// installation that works is not worth sending anyone to a privileged,
/// interactive command over a number that was never written down.
pub fn revision_of(plist: &str) -> Option<u32> {
    let (_, rest) = plist.split_once(REVISION_MARKER)?;
    rest.split_whitespace().next()?.parse().ok()
}

pub struct LaunchdPlan {
    /// Where the generated plist went. No privileges needed.
    pub source: PathBuf,
    /// Where it gets installed. Also appears in the commands.
    #[allow(
        dead_code,
        reason = "the destination is baked into the command strings, but tests and future output read it"
    )]
    pub destination: PathBuf,
    pub commands: Vec<String>,
}

/// Writes the plist into `minato_home` and returns the install steps.
pub fn prepare(
    program: &Path,
    minato_home: &Path,
    user: &str,
    ports: Ports,
) -> anyhow::Result<LaunchdPlan> {
    let source = minato_home.join(format!("{LABEL}.plist"));
    let destination = Path::new(INSTALL_DIR).join(format!("{LABEL}.plist"));

    std::fs::create_dir_all(minato_home)?;
    std::fs::write(
        &source,
        plist(program, minato_home, user, ports, container_gateway()),
    )?;

    let commands = vec![
        format!("sudo cp {} {}", source.display(), destination.display()),
        format!("sudo chown root:wheel {}", destination.display()),
        format!("sudo chmod 644 {}", destination.display()),
        format!("sudo launchctl bootstrap system {}", destination.display()),
    ];

    Ok(LaunchdPlan {
        source,
        destination,
        commands,
    })
}

/// The steps that hand a job launchd already has its sockets back, for
/// when [`minato_core::launchd::is_loaded`] says installing is not what is
/// wanted.
///
/// Stopping the daemon comes first because it is usually the reason the job
/// is not running: a daemon started any other way owns the socket, launchd's
/// job finds it taken and stands down, and a clean exit is not restarted
/// (`KeepAlive { SuccessfulExit: false }`). Kickstarting around it would
/// only repeat that.
pub fn wake_commands() -> Vec<String> {
    vec![
        "minato daemon stop".to_string(),
        minato_core::launchd::kickstart_command(),
    ]
}

/// The steps that undo the installation.
pub fn uninstall_commands() -> Vec<String> {
    let destination = Path::new(INSTALL_DIR).join(format!("{LABEL}.plist"));

    vec![
        format!("sudo launchctl bootout system/{LABEL}"),
        format!("sudo rm {}", destination.display()),
    ]
}

/// Where Apple Container's containers reach the host, if it can be asked.
///
/// **The proxy has to be listening there, and only launchd can put it
/// there.** A container on that network cannot reach the host's loopback,
/// and :443 is privileged, so the socket has to be in the plist — which is
/// written here, once, by `minato setup`.
///
/// `None` when Apple Container is not installed or not running. Running the
/// CLI is duplicated from the daemon rather than shared: `minato-core` is
/// where the two agree on the shape of the answer, and it stays clear of
/// running container tooling itself.
fn container_gateway() -> Option<Ipv4Addr> {
    let output = std::process::Command::new(minato_core::apple::PROGRAM)
        .args(minato_core::apple::LIST_ARGS)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    minato_core::apple::parse_gateway(
        &String::from_utf8_lossy(&output.stdout),
        minato_core::apple::DEFAULT_NETWORK,
    )
}

#[derive(Debug, Clone, Copy)]
pub struct Ports {
    pub http: u16,
    pub https: u16,
    pub dns: u16,
}

impl Default for Ports {
    fn default() -> Self {
        Self {
            http: 80,
            https: 443,
            dns: 53,
        }
    }
}

fn plist(
    program: &Path,
    minato_home: &Path,
    user: &str,
    ports: Ports,
    container_gateway: Option<Ipv4Addr>,
) -> String {
    // `localhost` resolves to both ::1 and 127.0.0.1, so launchd opens two
    // sockets and hands over both descriptors. Clients that prefer IPv6
    // are covered too.
    let mut nodes = vec!["localhost".to_string()];
    nodes.extend(container_gateway.map(|gateway| gateway.to_string()));

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<!-- {REVISION_MARKER} {PLIST_REVISION} -->
<dict>
  <key>Label</key>
  <string>{LABEL}</string>

  <key>ProgramArguments</key>
  <array>
    <string>{program}</string>
  </array>

  <!-- launchd opens the sockets as root; the process runs as this user. -->
  <key>UserName</key>
  <string>{user}</string>

  <key>EnvironmentVariables</key>
  <dict>
    <key>MINATO_HOME</key>
    <string>{home}</string>
  </dict>

  <key>RunAtLoad</key>
  <true/>

  <!-- A clean exit via `minato daemon stop` is not restarted. -->
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>

  <key>StandardErrorPath</key>
  <string>{home}/logs/launchd.log</string>

  <key>Sockets</key>
  <dict>
{http_sockets}
{https_sockets}

    <key>dns-udp</key>
    <dict>
      <key>SockNodeName</key>
      <string>127.0.0.1</string>
      <key>SockServiceName</key>
      <string>{dns}</string>
      <key>SockType</key>
      <string>dgram</string>
    </dict>

    <key>dns-tcp</key>
    <dict>
      <key>SockNodeName</key>
      <string>127.0.0.1</string>
      <key>SockServiceName</key>
      <string>{dns}</string>
      <key>SockType</key>
      <string>stream</string>
    </dict>
  </dict>
</dict>
</plist>
"#,
        program = escape_xml(&program.to_string_lossy()),
        user = escape_xml(user),
        home = escape_xml(&minato_home.to_string_lossy()),
        http_sockets = stream_sockets("http", &nodes, ports.http),
        https_sockets = stream_sockets("https", &nodes, ports.https),
        dns = ports.dns,
    )
}

/// One socket entry, listening on `port` at every address in `nodes`.
///
/// **An array even for one address**, which launchd accepts and which
/// keeps the second one from being a different shape of entry. An address
/// that does not exist at boot — Apple Container's gateway, on a machine
/// where its network has not come up — costs that socket and nothing else:
/// the job still loads and its other sockets still listen.
fn stream_sockets(key: &str, nodes: &[String], port: u16) -> String {
    let entries: Vec<String> = nodes
        .iter()
        .map(|node| {
            format!(
                "      <dict>
        <key>SockNodeName</key>
        <string>{node}</string>
        <key>SockServiceName</key>
        <string>{port}</string>
        <key>SockType</key>
        <string>stream</string>
      </dict>",
                node = escape_xml(node),
            )
        })
        .collect();

    format!(
        "    <key>{key}</key>
    <array>
{}
    </array>",
        entries.join("\n")
    )
}

/// Keeps an `&` or a `<` in a path from breaking the plist.
fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        plist(
            Path::new("/usr/local/bin/minatod"),
            Path::new("/Users/someone/.minato"),
            "someone",
            Ports::default(),
            None,
        )
    }

    #[test]
    fn declares_every_socket_the_daemon_expects() {
        let xml = sample();

        // launch_activate_socket returns nothing unless the names match.
        for key in ["http", "https", "dns-udp", "dns-tcp"] {
            assert!(xml.contains(&format!("<key>{key}</key>")), "missing: {key}");
        }
    }

    #[test]
    fn runs_as_the_user_not_root() {
        let xml = sample();

        assert!(
            xml.contains("<key>UserName</key>"),
            "running as root would leave containers and files owned by the wrong user"
        );
        assert!(xml.contains("<string>someone</string>"));
    }

    #[test]
    fn binds_privileged_ports() {
        let xml = sample();
        assert!(xml.contains("<string>80</string>"));
        assert!(xml.contains("<string>443</string>"));
        assert!(xml.contains("<string>53</string>"));
    }

    #[test]
    fn uses_localhost_so_both_families_are_bound() {
        let xml = sample();
        // localhost resolves to both ::1 and 127.0.0.1, so two
        // descriptors come through.
        assert!(xml.contains("<string>localhost</string>"));
    }

    #[test]
    fn holds_the_ports_where_containers_reach_the_host_too() {
        // Apple Container's containers cannot reach the host's loopback,
        // and :443 is privileged — so if this socket is not in the plist,
        // nothing can put the proxy where those containers look for it.
        let xml = plist(
            Path::new("/usr/local/bin/minatod"),
            Path::new("/Users/someone/.minato"),
            "someone",
            Ports::default(),
            Some(Ipv4Addr::new(192, 168, 64, 1)),
        );

        assert!(xml.contains("<string>192.168.64.1</string>"), "{xml}");
        assert!(
            xml.contains("<string>localhost</string>"),
            "not instead of loopback: {xml}"
        );
    }

    #[test]
    fn names_only_loopback_without_apple_container() {
        // Most machines have no such network, and a socket for an address
        // that will never exist is one launchd reports as a failure every
        // time the job loads.
        assert!(!sample().contains("192.168.64"));
    }

    #[test]
    fn does_not_restart_after_a_clean_stop() {
        let xml = sample();

        // An unconditional KeepAlive would make `minato daemon stop`
        // useless.
        assert!(xml.contains("<key>SuccessfulExit</key>"));
        assert!(xml.contains("<false/>"));
    }

    #[test]
    fn says_which_revision_wrote_it() {
        // A build that lands on a machine reads this to work out whether
        // `minato setup` has anything new to do. Without it in the file,
        // it never has anything to compare against.
        assert_eq!(revision_of(&sample()), Some(PLIST_REVISION));
    }

    #[test]
    fn a_plist_from_before_the_marker_has_no_revision() {
        // Every plist installed until now. `None` rather than 0, which
        // would read as "older than revision 1" and send those machines
        // to `minato setup` over a comment.
        assert_eq!(revision_of("<plist version=\"1.0\"><dict/></plist>"), None);
        assert_eq!(revision_of("<!-- minato plist revision -->"), None);
        assert_eq!(revision_of("<!-- minato plist revision x -->"), None);
    }

    #[test]
    fn passes_minato_home_through() {
        let xml = sample();
        assert!(xml.contains("<key>MINATO_HOME</key>"));
        assert!(xml.contains("<string>/Users/someone/.minato</string>"));
    }

    #[test]
    fn escapes_paths_that_would_break_the_xml() {
        let xml = plist(
            Path::new("/tmp/a&b/minatod"),
            Path::new("/tmp/<home>"),
            "some&one",
            Ports::default(),
            None,
        );

        assert!(xml.contains("/tmp/a&amp;b/minatod"));
        assert!(xml.contains("/tmp/&lt;home&gt;"));
        assert!(xml.contains("some&amp;one"));
        assert!(!xml.contains("a&b"), "a bare & breaks the plist");
    }

    #[test]
    fn honours_custom_ports() {
        let xml = plist(
            Path::new("/bin/minatod"),
            Path::new("/tmp/minato"),
            "someone",
            Ports {
                http: 8080,
                https: 8443,
                dns: 15353,
            },
            None,
        );

        assert!(xml.contains("<string>8080</string>"));
        assert!(xml.contains("<string>15353</string>"));
    }

    #[test]
    fn writes_the_plist_and_returns_install_commands() {
        let dir = tempfile::tempdir().expect("tempdir");

        let plan = prepare(
            Path::new("/usr/local/bin/minatod"),
            dir.path(),
            "someone",
            Ports::default(),
        )
        .expect("writes it");

        assert!(
            plan.source.is_file(),
            "writing the plist needs no privileges"
        );
        assert!(plan.destination.starts_with(INSTALL_DIR));

        // Installing needs root, so it only ever comes back as steps.
        assert!(plan.commands.iter().all(|c| c.starts_with("sudo")));
        assert!(plan.commands.iter().any(|c| c.contains("bootstrap")));
        assert!(
            plan.commands.iter().any(|c| c.contains("chown root:wheel")),
            "a LaunchDaemon is ignored unless root owns it"
        );
    }

    #[test]
    fn waking_does_not_bootstrap_again() {
        let commands = wake_commands();

        assert!(
            !commands.iter().any(|c| c.contains("bootstrap")),
            "launchd fails a second bootstrap with EIO: {commands:?}"
        );
        let stop = commands.iter().position(|c| c.contains("daemon stop"));
        let kickstart = commands.iter().position(|c| c.contains("kickstart"));

        assert!(
            matches!((stop, kickstart), (Some(stop), Some(kickstart)) if stop < kickstart),
            "the daemon holding the socket has to go first: {commands:?}"
        );
    }

    #[test]
    fn uninstall_uses_bootout() {
        let commands = uninstall_commands();
        assert!(commands.iter().any(|c| c.contains("bootout")));
        assert!(commands.iter().any(|c| c.contains("rm ")));
    }
}

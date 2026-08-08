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

use std::path::{Path, PathBuf};

// The daemon and the client have to agree on where the plist lives and what
// the job is called, so those live in `minato-core`. Generating the plist is
// this side's job alone.
pub use minato_core::launchd::{INSTALL_DIR, LABEL};

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
    std::fs::write(&source, plist(program, minato_home, user, ports))?;

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

/// The steps that undo the installation.
pub fn uninstall_commands() -> Vec<String> {
    let destination = Path::new(INSTALL_DIR).join(format!("{LABEL}.plist"));

    vec![
        format!("sudo launchctl bootout system/{LABEL}"),
        format!("sudo rm {}", destination.display()),
    ]
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

fn plist(program: &Path, minato_home: &Path, user: &str, ports: Ports) -> String {
    // `localhost` resolves to both ::1 and 127.0.0.1, so launchd opens two
    // sockets and hands over both descriptors. Clients that prefer IPv6
    // are covered too.
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
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
    <key>http</key>
    <dict>
      <key>SockNodeName</key>
      <string>localhost</string>
      <key>SockServiceName</key>
      <string>{http}</string>
      <key>SockType</key>
      <string>stream</string>
    </dict>

    <key>https</key>
    <dict>
      <key>SockNodeName</key>
      <string>localhost</string>
      <key>SockServiceName</key>
      <string>{https}</string>
      <key>SockType</key>
      <string>stream</string>
    </dict>

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
        http = ports.http,
        https = ports.https,
        dns = ports.dns,
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
    fn does_not_restart_after_a_clean_stop() {
        let xml = sample();

        // An unconditional KeepAlive would make `minato daemon stop`
        // useless.
        assert!(xml.contains("<key>SuccessfulExit</key>"));
        assert!(xml.contains("<false/>"));
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
    fn uninstall_uses_bootout() {
        let commands = uninstall_commands();
        assert!(commands.iter().any(|c| c.contains("bootout")));
        assert!(commands.iter().any(|c| c.contains("rm ")));
    }
}

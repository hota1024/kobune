//! launchd の plist 生成。
//!
//! 80 / 443 / 53 は非 root では bind できない。launchd（root）に bind させて
//! ファイルディスクリプタだけ渡してもらえば、**daemon 本体は非 root のまま**
//! でよい。`UserName` を指定することで、launchd が root で socket を開き、
//! プロセス自体は利用者の権限で動く。
//!
//! plist の書き出しまでは権限が要らないのでここで行い、`/Library/LaunchDaemons`
//! への設置は sudo が要るのでコマンドを提示するに留める。

use std::path::{Path, PathBuf};

/// launchd のジョブ名。
pub const LABEL: &str = "dev.minato.daemon";

/// システム全体の LaunchDaemon の置き場。
///
/// ユーザ単位の `LaunchAgents` ではなく `LaunchDaemons` に置く必要がある。
/// 特権ポートを bind できるのは root で動く launchd だけのため。
pub const INSTALL_DIR: &str = "/Library/LaunchDaemons";

pub struct LaunchdPlan {
    /// 生成した plist の場所（権限不要）。
    pub source: PathBuf,
    /// 設置先。設置コマンドにも現れる。
    #[allow(
        dead_code,
        reason = "設置先はコマンド文字列にも埋め込まれるが、テストと将来の表示で参照する"
    )]
    pub destination: PathBuf,
    pub commands: Vec<String>,
}

/// plist を `minato_home` に書き出し、設置手順を返す。
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

/// 設置を取り消す手順。
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
    // `localhost` は ::1 と 127.0.0.1 の両方に解決されるため、
    // launchd はソケットを 2 つ開いて両方の fd を渡してくる。
    // これで IPv6 を優先するクライアントも取りこぼさない。
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

  <!-- launchd は root で socket を開くが、プロセスはこの利用者で動かす。 -->
  <key>UserName</key>
  <string>{user}</string>

  <key>EnvironmentVariables</key>
  <dict>
    <key>MINATO_HOME</key>
    <string>{home}</string>
  </dict>

  <key>RunAtLoad</key>
  <true/>

  <!-- `minato daemon stop` で正常終了したときは再起動しない。 -->
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

/// パスに `&` や `<` が含まれても plist を壊さない。
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

        // 名前が一致していないと launch_activate_socket が fd を返さない。
        for key in ["http", "https", "dns-udp", "dns-tcp"] {
            assert!(xml.contains(&format!("<key>{key}</key>")), "missing: {key}");
        }
    }

    #[test]
    fn runs_as_the_user_not_root() {
        let xml = sample();

        assert!(
            xml.contains("<key>UserName</key>"),
            "root のまま動かすと、作るコンテナやファイルの所有者がずれる"
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
        // localhost は ::1 と 127.0.0.1 の両方に解決され、fd が 2 つ渡る。
        assert!(xml.contains("<string>localhost</string>"));
    }

    #[test]
    fn does_not_restart_after_a_clean_stop() {
        let xml = sample();

        // KeepAlive を無条件 true にすると `minato daemon stop` が効かなくなる。
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
        assert!(!xml.contains("a&b"), "生の & は plist を壊す");
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
        .expect("書き出せる");

        assert!(plan.source.is_file(), "plist の書き出しに権限は要らない");
        assert!(plan.destination.starts_with(INSTALL_DIR));

        // 設置は root が要る。手順としてのみ提示する。
        assert!(plan.commands.iter().all(|c| c.starts_with("sudo")));
        assert!(plan.commands.iter().any(|c| c.contains("bootstrap")));
        assert!(
            plan.commands.iter().any(|c| c.contains("chown root:wheel")),
            "LaunchDaemon は root 所有でないと読み込まれない"
        );
    }

    #[test]
    fn uninstall_uses_bootout() {
        let commands = uninstall_commands();
        assert!(commands.iter().any(|c| c.contains("bootout")));
        assert!(commands.iter().any(|c| c.contains("rm ")));
    }
}

//! ホスト側の設定の診断と、その直し方。
//!
//! `/etc/resolver` の設置とローカル CA の信頼登録には root が要る。
//! **勝手に sudo を走らせない。** エージェントが実行すると password 待ちで
//! 固まり、人間から見れば黙って権限昇格したことになる。
//! ここでは診断とコマンドの提示に徹し、実行は利用者に委ねる。

use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};

use minato_api::{Check, CheckStatus};

/// macOS が参照する resolver 設定の置き場。
pub const RESOLVER_DIR: &str = "/etc/resolver";

/// システム側の設定を診断する。
///
/// `dns_port` と `ca_path` は daemon から得た実際の値を渡す。
pub fn check_system(suffix: &str, dns_port: Option<u16>, ca_path: Option<&Path>) -> Vec<Check> {
    let mut checks = vec![check_resolver(suffix, dns_port)];

    if let Some(path) = ca_path {
        checks.push(check_ca_trust(path));
    }

    // 実際に引けるかどうかが最終的な答え。設定ファイルがあっても
    // 反映されていないことがある。
    checks.push(check_resolution(suffix));

    checks
}

/// `/etc/resolver/{suffix}` が Minato の DNS を指しているか。
fn check_resolver(suffix: &str, dns_port: Option<u16>) -> Check {
    let path = resolver_path(suffix);
    let title = format!("DNS resolver ({})", path.display());

    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Check::fail("resolver", title, "設置されていません".to_string())
            .with_fix(resolver_fix(suffix, dns_port));
    };

    if !contents.contains("127.0.0.1") {
        return Check::fail("resolver", title, "127.0.0.1 を指していません".to_string())
            .with_fix(resolver_fix(suffix, dns_port));
    }

    // 非特権ポートで動かしている場合、port 行が無いと 53 に問い合わせてしまう。
    if let Some(port) = dns_port {
        if port != 53 && !contents.contains(&format!("port {port}")) {
            return Check::fail(
                "resolver",
                title,
                format!("DNS は :{port} で待ち受けていますが、設定に port 行がありません"),
            )
            .with_fix(resolver_fix(suffix, dns_port));
        }
    }

    Check::ok("resolver", title, "設置済み".to_string())
}

/// CA がシステムに信頼されているか。
fn check_ca_trust(ca_path: &Path) -> Check {
    let title = "ローカル CA の信頼".to_string();

    if !ca_path.is_file() {
        return Check::warn("ca-trust", title, "CA がまだ生成されていません".to_string());
    }

    if !cfg!(target_os = "macos") {
        return Check::warn(
            "ca-trust",
            title,
            "この OS では自動判定できません。手動で確認してください".to_string(),
        )
        .with_fix(trust_fix(ca_path));
    }

    // 信頼済みかどうかは verify-cert が最も確実。
    // 自己署名 CA は、信頼されていれば検証が通る。
    let verified = std::process::Command::new("security")
        .args(["verify-cert", "-c"])
        .arg(ca_path)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);

    if verified {
        Check::ok("ca-trust", title, "信頼されています".to_string())
    } else {
        Check::warn(
            "ca-trust",
            title,
            "信頼されていません。HTTPS でブラウザや curl が警告します".to_string(),
        )
        .with_fix(trust_fix(ca_path))
    }
}

/// 実際に名前が引けるか。ここが通れば curl も通る。
///
/// **127.0.0.1 に解決されることまで確かめる。** macOS は resolver が
/// 未設置でも `*.localhost` を `::1` に解決することがあるが、プロキシは
/// IPv4 でしか待ち受けないため接続できない。「引けた」だけで OK にすると、
/// 実際には繋がらないのに診断が通ってしまう。
fn check_resolution(suffix: &str) -> Check {
    let probe = format!("minato-doctor-probe.{suffix}");
    let title = format!("{probe} の名前解決");

    let addresses: Vec<std::net::IpAddr> = match (probe.as_str(), 80u16).to_socket_addrs() {
        Ok(addrs) => addrs.map(|addr| addr.ip()).collect(),
        Err(err) => {
            return Check::fail("resolution", title, err.to_string()).with_fix(format!(
                "`minato setup` の手順で /etc/resolver/{suffix} を設置してください"
            ));
        }
    };

    if addresses.is_empty() {
        return Check::fail("resolution", title, "解決結果が空です".to_string()).with_fix(format!(
            "`minato setup` の手順で /etc/resolver/{suffix} を設置してください"
        ));
    }

    let loopback_v4 = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    if addresses.contains(&loopback_v4) {
        return Check::ok("resolution", title, "127.0.0.1 に解決".to_string());
    }

    let rendered: Vec<String> = addresses.iter().map(|addr| addr.to_string()).collect();
    Check::fail(
        "resolution",
        title,
        format!(
            "{} に解決されました。プロキシは 127.0.0.1 で待ち受けているため接続できません",
            rendered.join(", ")
        ),
    )
    .with_fix(format!(
        "`minato setup` の手順で /etc/resolver/{suffix} を設置してください"
    ))
}

pub fn resolver_path(suffix: &str) -> PathBuf {
    Path::new(RESOLVER_DIR).join(suffix)
}

/// resolver ファイルの中身。
pub fn resolver_contents(dns_port: Option<u16>) -> String {
    let port = dns_port.unwrap_or(53);

    if port == 53 {
        "nameserver 127.0.0.1\n".to_string()
    } else {
        // 非特権ポートで動かす場合はポートを明示する。
        // これがあるおかげで DNS に root が要らない。
        format!("nameserver 127.0.0.1\nport {port}\n")
    }
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

/// `minato setup` が案内する手順。
pub struct SetupPlan {
    pub steps: Vec<SetupStep>,
}

pub struct SetupStep {
    pub description: String,
    pub command: String,
}

impl SetupPlan {
    /// 未完了の項目だけを集める。既に済んでいる作業を促さない。
    pub fn from_checks(checks: &[Check]) -> Self {
        let steps = checks
            .iter()
            .filter(|check| check.status != CheckStatus::Ok)
            .filter_map(|check| {
                check.fix.as_ref().map(|fix| SetupStep {
                    description: check.title.clone(),
                    command: fix.clone(),
                })
            })
            .collect();

        Self { steps }
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolver_contents_include_port_when_non_standard() {
        // port 行があるおかげで DNS を非特権ポートで動かせる。
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
        let fix = check.fix.expect("直し方が要る");
        assert!(fix.contains("sudo"), "got: {fix}");
        assert!(
            fix.contains("port 15353") || fix.contains("port\\n"),
            "got: {fix}"
        );
    }

    #[test]
    fn resolution_to_ipv6_only_is_a_failure() {
        // macOS は resolver 未設置でも *.localhost を ::1 に返すことがある。
        // それを OK にすると、繋がらないのに診断が通ってしまう。
        let check = check_resolution("localhost");

        if check.status == CheckStatus::Ok {
            assert!(
                check.detail.contains("127.0.0.1"),
                "OK にするのは IPv4 ループバックに解決したときだけ: {}",
                check.detail
            );
        } else {
            assert!(check.fix.is_some(), "直し方を添える");
        }
    }

    #[test]
    fn setup_plan_skips_completed_checks() {
        let checks = vec![
            Check::ok("a", "済んでいる", "問題なし").with_fix("やらなくてよい"),
            Check::fail("b", "未完了", "設置されていません").with_fix("sudo something"),
        ];

        let plan = SetupPlan::from_checks(&checks);

        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].command, "sudo something");
    }

    #[test]
    fn setup_plan_is_empty_when_everything_is_done() {
        let checks = vec![Check::ok("a", "済んでいる", "問題なし")];
        assert!(SetupPlan::from_checks(&checks).is_empty());
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

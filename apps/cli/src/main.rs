//! minato — daemon の薄いクライアント。
//!
//! ここにロジックを置かない。判断はすべて daemon 側で行い、CLI は
//! リクエストの組み立てと表示だけを担当する（`docs/DESIGN.md` §3）。

mod init;
mod launchd;
mod output;
mod system;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use minato_api::{Request, Response, Target};

/// `[project] domain` を省略したときに使われる接尾辞。
/// resolver の設置対象もこれになる。
const DEFAULT_DOMAIN_SUFFIX: &str = "localhost";
use minato_client::{Client, ClientError};

#[derive(Parser, Debug)]
#[command(
    name = "minato",
    version,
    about = "AI エージェント向けの開発環境管理ツール",
    long_about = "git worktree ごとにプレビュー環境を管理する。\n\
                  すべてのコマンドが --json に対応しており、終了コードで失敗の種類を判別できる。"
)]
struct Cli {
    /// 応答を JSON で出力する。エージェントはこれを使う。
    #[arg(long, global = true)]
    json: bool,

    /// 対象の workspace。省略時は現在のディレクトリから判定する。
    #[arg(long, short = 'w', global = true)]
    workspace: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// minato.toml のひな形を作る
    Init {
        /// 既存の minato.toml を上書きする
        #[arg(long)]
        force: bool,
    },

    /// daemon への疎通を確認する
    Ping,

    /// 環境を診断し、直し方を示す
    Doctor,

    /// URL を使うために必要な、権限の要る設定を案内する
    Setup,

    /// workspace を一覧する
    Ls {
        /// すべてのプロジェクトを対象にする
        #[arg(long)]
        all_projects: bool,
    },

    /// worktree を作り、環境を起動する
    New {
        /// チェックアウトするブランチ。存在しなければ作成する
        branch: String,

        /// 新規ブランチの分岐元
        #[arg(long)]
        base: Option<String>,

        /// worktree を作るパス
        #[arg(long)]
        path: Option<PathBuf>,

        /// 作成するだけで起動しない
        #[arg(long)]
        no_start: bool,
    },

    /// worktree と環境を破棄する
    Rm {
        /// 未コミットの変更があっても削除する
        #[arg(long, short)]
        force: bool,
    },

    /// サービスを起動する
    Up {
        /// 対象サービス。省略時はすべて
        services: Vec<String>,
    },

    /// サービスを停止する
    Down {
        /// 対象サービス。省略時はこの workspace のすべて
        services: Vec<String>,

        /// プロジェクト内のすべての workspace を停止する
        #[arg(long)]
        all: bool,
    },

    /// 現在の状態を表示する
    Status,

    /// サービスのアクセス先を 1 行で出力する
    Url {
        /// サービス名。省略時は公開されている最初のサービス
        service: Option<String>,
    },

    /// daemon を操作する
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
}

#[derive(Subcommand, Debug)]
enum DaemonCommand {
    /// daemon を起動する
    Start,
    /// daemon を停止する
    Stop,
    /// daemon の状態を表示する
    Status,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match run(&cli).await {
        Ok(code) => code,
        Err(err) => {
            if cli.json {
                if let Some(api_error) = as_api_error(&err) {
                    output::print_error_json(api_error);
                } else {
                    output::print_error_json(&minato_api::ApiError::internal(err.to_string()));
                }
            } else {
                output::print_error(&err.to_string(), hint_for(&err));
            }

            ExitCode::from(exit_code_for(&err) as u8)
        }
    }
}

/// CLI が扱うエラー。daemon 由来のものは終了コードを引き継ぐ。
#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error(transparent)]
    Client(#[from] ClientError),

    #[error("{0}")]
    Local(String),
}

fn as_api_error(err: &CliError) -> Option<&minato_api::ApiError> {
    match err {
        CliError::Client(ClientError::Api(api)) => Some(api),
        _ => None,
    }
}

fn hint_for(err: &CliError) -> Option<&str> {
    match err {
        CliError::Client(client) => client.hint(),
        CliError::Local(_) => None,
    }
}

fn exit_code_for(err: &CliError) -> i32 {
    match err {
        CliError::Client(client) => client.exit_code(),
        CliError::Local(_) => 1,
    }
}

async fn run(cli: &Cli) -> Result<ExitCode, CliError> {
    let cwd = std::env::current_dir()
        .map_err(|err| CliError::Local(format!("作業ディレクトリを取得できません: {err}")))?;

    // init は daemon を必要としない。
    if let Command::Init { force } = &cli.command {
        let outcome = init::run(&cwd, *force).map_err(|err| CliError::Local(err.to_string()))?;

        if cli.json {
            output::print_json(&serde_json::json!({
                "path": outcome.path,
                "project": outcome.project,
            }));
        } else {
            println!("{} を作成しました", outcome.path.display());
            println!("プロジェクト名: {}", outcome.project);
            println!();
            println!("次は `minato up` で環境を起動できます");
        }

        return Ok(ExitCode::SUCCESS);
    }

    let client = Client::from_env()
        .map_err(|err| CliError::Local(format!("設定ディレクトリを解決できません: {err}")))?;

    if let Command::Daemon { command } = &cli.command {
        return handle_daemon(cli, &client, command).await;
    }

    let target = Target::new(cwd).workspace(cli.workspace.clone());
    let request = build_request(cli, target)?;

    let mut connection = client.connect_or_spawn().await?;

    // JSON 出力では途中経過を出さない。1 本の JSON だけを返す。
    let show_progress = !cli.json && request.is_long_running();
    let response = connection
        .call(request, |event| {
            if show_progress {
                output::print_event(&event);
            }
        })
        .await?;

    present(cli, &response)?;
    Ok(ExitCode::SUCCESS)
}

fn build_request(cli: &Cli, target: Target) -> Result<Request, CliError> {
    let request = match &cli.command {
        Command::Ping => Request::Ping,
        Command::Ls { all_projects } => Request::Ls {
            target,
            all_projects: *all_projects,
        },
        Command::New {
            branch,
            base,
            path,
            no_start,
        } => Request::New {
            target,
            branch: branch.clone(),
            base: base.clone(),
            path: path.clone(),
            start: !no_start,
        },
        Command::Rm { force } => Request::Rm {
            target,
            force: *force,
        },
        Command::Up { services } => Request::Up {
            target,
            services: services.clone(),
        },
        Command::Down { services, all } => Request::Down {
            target,
            services: services.clone(),
            all: *all,
        },
        Command::Status | Command::Url { .. } => Request::Status { target },
        Command::Doctor | Command::Setup => Request::Doctor,
        Command::Init { .. } | Command::Daemon { .. } => {
            unreachable!("daemon を使わないコマンドは呼び出し前に処理済み")
        }
    };

    Ok(request)
}

fn present(cli: &Cli, response: &Response) -> Result<(), CliError> {
    // url だけは表示が特殊。パイプで繋げるよう 1 行だけ出す。
    if let Command::Url { service } = &cli.command {
        return present_url(cli, response, service.as_deref());
    }

    // doctor と setup は daemon の診断にホスト側の診断を足して見せる。
    if matches!(cli.command, Command::Doctor | Command::Setup) {
        return present_diagnostics(cli, response);
    }

    if cli.json {
        output::print_json(response);
        return Ok(());
    }

    match response {
        Response::Pong(pong) => {
            println!("minatod {} (protocol {})", pong.version, pong.protocol);
            println!("runtime: {}", pong.runtime);
            println!("uptime: {}秒", pong.uptime_secs);
        }
        Response::Workspaces { workspaces } => output::print_workspaces(workspaces),
        Response::Diagnostics(diagnostics) => output::print_diagnostics(diagnostics),
        Response::Workspace { workspace } => output::print_workspace(workspace),
        Response::Empty => println!("完了しました"),
    }

    Ok(())
}

fn present_url(cli: &Cli, response: &Response, service: Option<&str>) -> Result<(), CliError> {
    let Response::Workspace { workspace } = response else {
        return Err(CliError::Local(
            "workspace の情報を取得できませんでした".to_string(),
        ));
    };

    let target = match service {
        Some(name) => workspace.service(name).ok_or_else(|| {
            let available: Vec<&str> = workspace.services.iter().map(|s| s.name.as_str()).collect();
            CliError::Local(format!(
                "サービス `{name}` は定義されていません。利用できるサービス: {}",
                available.join(", ")
            ))
        })?,
        None => workspace
            .services
            .iter()
            .find(|s| s.access().is_some())
            .ok_or_else(|| {
                CliError::Local(
                    "アクセスできるサービスがありません。`minato up` で起動してください"
                        .to_string(),
                )
            })?,
    };

    let access = target.access().ok_or_else(|| {
        CliError::Local(format!(
            "サービス `{}` はまだアクセスできません（状態: {}）",
            target.name,
            target.state.label()
        ))
    })?;

    if cli.json {
        output::print_json(&serde_json::json!({
            "service": target.name,
            "url": access,
            "state": target.state,
        }));
    } else {
        // パイプで使えるよう、装飾なしで 1 行だけ出す。
        println!("{access}");
    }

    Ok(())
}

/// daemon の診断にホスト側の診断を足して表示する。
///
/// `/etc/resolver` や CA の信頼は daemon からは分からない。CLI が
/// 自分の目で確かめたものを合わせて、1 つの結果として見せる。
fn present_diagnostics(cli: &Cli, response: &Response) -> Result<(), CliError> {
    let Response::Diagnostics(diagnostics) = response else {
        return Err(CliError::Local(
            "診断結果を取得できませんでした".to_string(),
        ));
    };

    let dns_port = find_port(diagnostics, "dns");
    let ca_path = find_detail(diagnostics, "ca").map(PathBuf::from);

    let mut all = diagnostics.checks.clone();
    all.extend(system::check_system(
        DEFAULT_DOMAIN_SUFFIX,
        dns_port,
        ca_path.as_deref(),
    ));

    let combined = minato_api::Diagnostics::new(all);

    if matches!(cli.command, Command::Setup) {
        return present_setup(cli, &combined, dns_port, ca_path.as_deref());
    }

    if cli.json {
        output::print_json(&combined);
    } else {
        output::print_diagnostics(&combined);
    }

    Ok(())
}

/// 権限の要る手順を案内する。**実行はしない。**
///
/// sudo を自動で走らせると、エージェントは password 待ちで固まり、
/// 利用者から見れば黙って権限昇格したことになる。
fn present_setup(
    cli: &Cli,
    diagnostics: &minato_api::Diagnostics,
    dns_port: Option<u16>,
    ca_path: Option<&std::path::Path>,
) -> Result<(), CliError> {
    let pending = |id: &str| {
        diagnostics
            .checks
            .iter()
            .any(|check| check.id == id && check.status != minato_api::CheckStatus::Ok)
    };

    // (説明, 補足, コマンド)
    let mut steps: Vec<(String, Option<String>, Vec<String>)> = Vec::new();
    let launchd_pending = pending("launchd");

    if launchd_pending {
        match prepare_launchd() {
            Ok((source, commands)) => steps.push((
                "launchd に 80/443/53 を確保させる（daemon 自体は非 root のまま動きます）"
                    .to_string(),
                Some(format!("生成した plist: {}", source.display())),
                commands,
            )),
            Err(err) => eprintln!("警告: plist を書き出せませんでした: {err}"),
        }
    }

    // launchd を設置すると DNS は :53 に移る。設置前のポートを書いた
    // resolver を残すと、設置後に名前が引けなくなる。
    let effective_dns_port = if launchd_pending {
        launchd::Ports::default().dns
    } else {
        dns_port.unwrap_or(53)
    };

    if pending("resolver") || launchd_pending {
        steps.push((
            format!("*.{DEFAULT_DOMAIN_SUFFIX} を Minato の DNS に向ける"),
            None,
            vec![system::resolver_command(
                DEFAULT_DOMAIN_SUFFIX,
                effective_dns_port,
            )],
        ));
    }

    if pending("ca-trust") {
        if let Some(path) = ca_path {
            steps.push((
                "ローカル CA を信頼する（HTTPS の警告を消す）".to_string(),
                None,
                vec![system::trust_command(path)],
            ));
        }
    }

    if cli.json {
        output::print_json(&serde_json::json!({
            "steps": steps
                .iter()
                .map(|(description, note, commands)| serde_json::json!({
                    "description": description,
                    "note": note,
                    "commands": commands,
                }))
                .collect::<Vec<_>>(),
        }));
        return Ok(());
    }

    if steps.is_empty() {
        println!("設定は完了しています。`minato doctor` で確認できます");
        return Ok(());
    }

    println!("URL を使うには以下の設定が必要です。");
    println!("root 権限が要るため、内容を確認してから実行してください。");

    for (index, (description, note, commands)) in steps.iter().enumerate() {
        println!();
        println!("{}. {description}", index + 1);
        if let Some(note) = note {
            println!("   ({note})");
        }
        for command in commands {
            println!("   {command}");
        }
    }

    println!();
    println!("実行後に `minato daemon stop` してください（launchd が起動し直します）。");

    if launchd_pending {
        println!();
        println!("取り消すには:");
        for command in launchd::uninstall_commands() {
            println!("   {command}");
        }
    }

    Ok(())
}

/// 診断結果の detail からポート番号を拾う（`127.0.0.1:15353` 形式）。
fn find_port(diagnostics: &minato_api::Diagnostics, id: &str) -> Option<u16> {
    diagnostics
        .checks
        .iter()
        .find(|check| check.id == id)?
        .detail
        .rsplit(':')
        .next()?
        .parse()
        .ok()
}

fn find_detail<'a>(diagnostics: &'a minato_api::Diagnostics, id: &str) -> Option<&'a str> {
    let check = diagnostics.checks.iter().find(|check| check.id == id)?;
    if check.detail.starts_with('/') {
        Some(&check.detail)
    } else {
        None
    }
}

/// launchd の plist を書き出し、設置コマンドを返す。
fn prepare_launchd() -> anyhow::Result<(PathBuf, Vec<String>)> {
    let paths = minato_core::Paths::resolve()?;

    // CLI と daemon は一緒に配布されるので隣にいる。
    let program = std::env::current_exe()?
        .parent()
        .map(|dir| dir.join("minatod"))
        .unwrap_or_else(|| PathBuf::from("minatod"));

    let user = std::env::var("USER").unwrap_or_else(|_| "root".to_string());

    let plan = launchd::prepare(&program, paths.root(), &user, launchd::Ports::default())?;

    Ok((plan.source, plan.commands))
}

async fn handle_daemon(
    cli: &Cli,
    client: &Client,
    command: &DaemonCommand,
) -> Result<ExitCode, CliError> {
    match command {
        DaemonCommand::Start => {
            let mut connection = client.connect_or_spawn().await?;
            let pong = connection.handshake().await?;

            if cli.json {
                output::print_json(&pong);
            } else {
                println!("minatod {} が動いています", pong.version);
            }
            Ok(ExitCode::SUCCESS)
        }
        DaemonCommand::Stop => {
            let mut connection = match client.connect().await {
                Ok(connection) => connection,
                Err(_) => {
                    if !cli.json {
                        println!("daemon は動いていません");
                    }
                    return Ok(ExitCode::SUCCESS);
                }
            };

            connection.request(Request::Shutdown).await?;
            if !cli.json {
                println!("daemon を停止しました");
            }
            Ok(ExitCode::SUCCESS)
        }
        DaemonCommand::Status => match client.connect().await {
            Ok(mut connection) => {
                let pong = connection.handshake().await?;
                if cli.json {
                    output::print_json(&pong);
                } else {
                    println!("running");
                    println!("  version:  {}", pong.version);
                    println!("  protocol: {}", pong.protocol);
                    println!("  runtime:  {}", pong.runtime);
                    println!("  uptime:   {}秒", pong.uptime_secs);
                    println!("  socket:   {}", client.socket_path().display());
                }
                Ok(ExitCode::SUCCESS)
            }
            Err(_) => {
                if cli.json {
                    output::print_json(&serde_json::json!({ "running": false }));
                } else {
                    println!("stopped");
                }
                // 停止していることは異常ではないので成功で返す。
                Ok(ExitCode::SUCCESS)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn json_flag_is_available_on_every_subcommand() {
        // エージェントはどのコマンドでも --json を使える必要がある。
        for args in [
            vec!["minato", "ls", "--json"],
            vec!["minato", "status", "--json"],
            vec!["minato", "up", "--json"],
            vec!["minato", "new", "feature/x", "--json"],
            vec!["minato", "url", "web", "--json"],
            vec!["minato", "daemon", "status", "--json"],
        ] {
            let cli = Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert!(cli.json, "{args:?}");
        }
    }

    #[test]
    fn new_starts_services_by_default() {
        let cli = Cli::try_parse_from(["minato", "new", "feature/x"]).expect("parses");
        let request = build_request(&cli, Target::new(PathBuf::from("/repo"))).expect("builds");

        match request {
            Request::New { start, branch, .. } => {
                assert!(start, "`minato new` は環境まで立ち上げるのが既定");
                assert_eq!(branch, "feature/x");
            }
            other => panic!("想定外: {other:?}"),
        }
    }

    #[test]
    fn no_start_flag_is_respected() {
        let cli =
            Cli::try_parse_from(["minato", "new", "feature/x", "--no-start"]).expect("parses");
        let request = build_request(&cli, Target::new(PathBuf::from("/repo"))).expect("builds");

        match request {
            Request::New { start, .. } => assert!(!start),
            other => panic!("想定外: {other:?}"),
        }
    }

    #[test]
    fn workspace_flag_reaches_the_request() {
        let cli = Cli::try_parse_from(["minato", "up", "--workspace", "feat-1"]).expect("parses");
        let target = Target::new(PathBuf::from("/repo")).workspace(cli.workspace.clone());
        let request = build_request(&cli, target).expect("builds");

        match request {
            Request::Up { target, .. } => {
                assert_eq!(target.workspace.as_deref(), Some("feat-1"));
            }
            other => panic!("想定外: {other:?}"),
        }
    }

    #[test]
    fn url_asks_for_status() {
        // url は専用のリクエストを持たず、status の結果から取り出す。
        let cli = Cli::try_parse_from(["minato", "url", "web"]).expect("parses");
        let request = build_request(&cli, Target::new(PathBuf::from("/repo"))).expect("builds");

        assert!(matches!(request, Request::Status { .. }));
    }

    #[test]
    fn up_collects_service_names() {
        let cli = Cli::try_parse_from(["minato", "up", "web", "api"]).expect("parses");
        let request = build_request(&cli, Target::new(PathBuf::from("/repo"))).expect("builds");

        match request {
            Request::Up { services, .. } => assert_eq!(services, vec!["web", "api"]),
            other => panic!("想定外: {other:?}"),
        }
    }

    #[test]
    fn long_running_commands_show_progress() {
        // 進捗を出すかどうかはリクエストの種類で決まる。
        assert!(
            Request::Up {
                target: Target::new(PathBuf::from("/repo")),
                services: vec![]
            }
            .is_long_running()
        );

        assert!(
            !Request::Status {
                target: Target::new(PathBuf::from("/repo"))
            }
            .is_long_running()
        );
    }
}

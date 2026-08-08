//! minato — a thin client for the daemon.
//!
//! No logic lives here. Every decision is the daemon's; the CLI builds
//! requests and prints results (`docs/DESIGN.md` §3).

mod init;
mod launchd;
mod output;
mod skill;
mod system;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use minato_api::{Request, Response, Target};

/// The suffix used when `[project] domain` is left out. It is also what
/// the resolver gets installed for.
const DEFAULT_DOMAIN_SUFFIX: &str = "localhost";
use minato_client::{Client, ClientError};

#[derive(Parser, Debug)]
#[command(
    name = "minato",
    version,
    about = "A development environment manager for AI agents",
    long_about = "Manages a preview environment per git worktree.\n\
                  Every command supports --json, and the exit code says what kind of failure it was."
)]
struct Cli {
    /// Print the response as JSON. This is what agents use.
    #[arg(long, global = true)]
    json: bool,

    /// The workspace to act on. Inferred from the current directory when
    /// left out.
    #[arg(long, short = 'w', global = true)]
    workspace: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Write a starter minato.toml
    Init {
        /// Overwrite an existing minato.toml
        #[arg(long)]
        force: bool,
    },

    /// Check that the daemon answers
    Ping,

    /// Diagnose the environment and say how to fix it
    Doctor,

    /// Walk through the privileged setup the URLs need
    Setup,

    /// List workspaces
    Ls {
        /// Cover every project
        #[arg(long)]
        all_projects: bool,
    },

    /// Create a worktree and bring its environment up
    New {
        /// The branch to check out, created if it does not exist
        branch: String,

        /// What to branch a new branch from
        #[arg(long)]
        base: Option<String>,

        /// Where to put the worktree
        #[arg(long)]
        path: Option<PathBuf>,

        /// Create it without starting anything
        #[arg(long)]
        no_start: bool,
    },

    /// Destroy a worktree and its environment
    Rm {
        /// Delete it even with uncommitted changes
        #[arg(long, short)]
        force: bool,
    },

    /// Start services
    Up {
        /// Which services. All of them when left out
        services: Vec<String>,
    },

    /// Stop services
    Down {
        /// Which services. All of this workspace's when left out
        services: Vec<String>,

        /// Stop every workspace in the project
        #[arg(long)]
        all: bool,
    },

    /// Show the current state
    Status,

    /// Print where a service can be reached, one line
    Url {
        /// The service name. The first reachable one when left out
        service: Option<String>,
    },

    /// Show logs
    Logs {
        /// Which services. All of them when left out
        services: Vec<String>,

        /// Keep waiting for new lines
        #[arg(long, short)]
        follow: bool,

        /// How many lines to show from the end
        #[arg(long, short = 'n')]
        tail: Option<usize>,
    },

    /// Run a command inside a container
    ///
    /// The command's exit code is passed straight through.
    Exec {
        /// Which service
        service: String,

        /// The command to run, written after `--`
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },

    /// Work with environment variables
    Env {
        #[command(subcommand)]
        command: EnvCommand,
    },

    /// Install the Skill for agents
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },

    /// Control the daemon
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
}

#[derive(Subcommand, Debug)]
enum EnvCommand {
    /// List environment variables
    Ls {
        /// Show the values instead of masking them
        #[arg(long)]
        reveal: bool,
    },

    /// Print one value, ready to pipe
    Get { key: String },

    /// Set an environment variable
    Set {
        /// In KEY=VALUE form
        assignment: String,

        /// Which layer to write to
        #[arg(long, default_value = "workspace")]
        scope: String,
    },

    /// Remove an environment variable
    Unset {
        key: String,

        #[arg(long, default_value = "workspace")]
        scope: String,
    },
}

#[derive(Subcommand, Debug)]
enum SkillCommand {
    /// Install it at .claude/skills/minato/SKILL.md
    Install {
        /// Overwrite an existing file whose contents differ
        #[arg(long)]
        force: bool,
    },

    /// Print what would be installed
    Show,
}

#[derive(Subcommand, Debug)]
enum DaemonCommand {
    /// Start the daemon
    Start,
    /// Stop the daemon
    Stop,
    /// Show the daemon's state
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

/// The errors the CLI deals with. Ones from the daemon keep its exit
/// code.
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
        .map_err(|err| CliError::Local(format!("cannot read the working directory: {err}")))?;

    // `init` needs no daemon.
    if let Command::Init { force } = &cli.command {
        let outcome = init::run(&cwd, *force).map_err(|err| CliError::Local(err.to_string()))?;

        if cli.json {
            output::print_json(&serde_json::json!({
                "path": outcome.path,
                "project": outcome.project,
            }));
        } else {
            println!("created {}", outcome.path.display());
            println!("project: {}", outcome.project);
            println!();
            println!("next, bring the environment up with `minato up`");
        }

        return Ok(ExitCode::SUCCESS);
    }

    // Installing the Skill needs no daemon either.
    if let Command::Skill { command } = &cli.command {
        return handle_skill(cli, command, &cwd);
    }

    let client = Client::from_env()
        .map_err(|err| CliError::Local(format!("cannot resolve the configuration directory: {err}")))?;

    if let Command::Daemon { command } = &cli.command {
        return handle_daemon(cli, &client, command).await;
    }

    let target = Target::new(cwd).workspace(cli.workspace.clone());
    let request = build_request(cli, target)?;

    let mut connection = client.connect_or_spawn().await?;

    // JSON output carries no progress — exactly one JSON document comes
    // back. For logs and exec the output *is* the result, so no progress
    // decoration either.
    let raw_output = matches!(cli.command, Command::Logs { .. } | Command::Exec { .. });
    let show_progress = !cli.json && request.is_long_running() && !raw_output;

    let response = connection
        .call(request, |event| {
            if raw_output {
                output::print_output_event(&event);
            } else if show_progress {
                output::print_event(&event);
            }
        })
        .await?;

    present(cli, &response)?;

    // exec passes the command's exit code straight through: an agent has
    // to be able to judge `minato exec web -- pnpm test` by exit status
    // alone.
    if let Response::Exec { exit_code } = &response {
        return Ok(ExitCode::from(clamp_exit_code(*exit_code)));
    }

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
        Command::Logs {
            services,
            follow,
            tail,
        } => Request::Logs {
            target,
            services: services.clone(),
            follow: *follow,
            tail: *tail,
        },
        Command::Exec { service, command } => Request::Exec {
            target,
            service: service.clone(),
            command: command.clone(),
        },
        Command::Env { command } => build_env_request(command, target)?,
        Command::Doctor | Command::Setup => Request::Doctor,
        Command::Init { .. } | Command::Daemon { .. } | Command::Skill { .. } => {
            unreachable!("the commands that need no daemon are handled before this")
        }
    };

    Ok(request)
}

/// Installing the Skill. Needs no daemon.
fn handle_skill(
    cli: &Cli,
    command: &SkillCommand,
    cwd: &std::path::Path,
) -> Result<ExitCode, CliError> {
    match command {
        SkillCommand::Show => {
            print!("{}", skill::contents());
            Ok(ExitCode::SUCCESS)
        }
        SkillCommand::Install { force } => {
            // Run from inside a worktree, it still goes at the repository
            // root.
            let root = minato_core::Repository::discover(cwd)
                .map(|repo| repo.main_root)
                .unwrap_or_else(|_| cwd.to_path_buf());

            let installed =
                skill::install(&root, *force).map_err(|err| CliError::Local(err.to_string()))?;

            if cli.json {
                output::print_json(&serde_json::json!({
                    "path": installed.path,
                    "overwritten": installed.overwritten,
                }));
            } else if installed.overwritten {
                println!("updated {}", installed.path.display());
            } else {
                println!("installed {}", installed.path.display());
            }

            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Squeezes an exit code into a process exit code.
///
/// A death by signal comes through negative. Rounding that to 0 would read
/// as success, so it stays a failure.
fn clamp_exit_code(code: i32) -> u8 {
    u8::try_from(code).unwrap_or(1)
}

fn build_env_request(command: &EnvCommand, target: Target) -> Result<Request, CliError> {
    let parse_scope = |raw: &str| -> Result<minato_api::EnvScope, CliError> {
        raw.parse::<minato_api::EnvScope>().map_err(CliError::Local)
    };

    Ok(match command {
        EnvCommand::Ls { reveal } => Request::EnvList {
            target,
            reveal: *reveal,
        },
        // `get` pulls from the listing, and the value itself is the
        // point, so nothing is masked.
        EnvCommand::Get { .. } => Request::EnvList {
            target,
            reveal: true,
        },
        EnvCommand::Set { assignment, scope } => {
            let Some((key, value)) = assignment.split_once('=') else {
                return Err(CliError::Local(format!(
                    "`{assignment}` is not in KEY=VALUE form"
                )));
            };

            Request::EnvSet {
                target,
                scope: parse_scope(scope)?,
                key: key.to_string(),
                value: value.to_string(),
            }
        }
        EnvCommand::Unset { key, scope } => Request::EnvUnset {
            target,
            scope: parse_scope(scope)?,
            key: key.clone(),
        },
    })
}

fn present(cli: &Cli, response: &Response) -> Result<(), CliError> {
    // `url` prints differently: one line, ready to pipe.
    if let Command::Url { service } = &cli.command {
        return present_url(cli, response, service.as_deref());
    }

    // `env get` prints one line too, for the same reason.
    if let Command::Env {
        command: EnvCommand::Get { key },
    } = &cli.command
    {
        return present_env_value(cli, response, key);
    }

    // `doctor` and `setup` add the host-side checks to the daemon's.
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
            println!("uptime: {}s", pong.uptime_secs);
        }
        Response::Workspaces { workspaces } => output::print_workspaces(workspaces),
        Response::Diagnostics(diagnostics) => output::print_diagnostics(diagnostics),
        Response::Env { entries } => {
            output::print_env(entries);

            // A change does not reach containers that are already
            // running. Left unsaid, that reads as "I set it and nothing
            // happened".
            if matches!(
                cli.command,
                Command::Env {
                    command: EnvCommand::Set { .. } | EnvCommand::Unset { .. }
                }
            ) {
                println!();
                println!("run `minato down` then `minato up` to pick this up");
            }
        }
        Response::Workspace { workspace } => output::print_workspace(workspace),
        // logs has already printed its lines; exec speaks through its
        // exit code.
        Response::Exec { .. } => {}
        Response::Empty if matches!(cli.command, Command::Logs { .. }) => {}
        Response::Empty => println!("done"),
    }

    Ok(())
}

fn present_url(cli: &Cli, response: &Response, service: Option<&str>) -> Result<(), CliError> {
    let Response::Workspace { workspace } = response else {
        return Err(CliError::Local(
            "cannot read the workspace".to_string(),
        ));
    };

    let target = match service {
        Some(name) => workspace.service(name).ok_or_else(|| {
            let available: Vec<&str> = workspace.services.iter().map(|s| s.name.as_str()).collect();
            CliError::Local(format!(
                "no service named `{name}`. Available: {}",
                available.join(", ")
            ))
        })?,
        None => workspace
            .services
            .iter()
            .find(|s| s.access().is_some())
            .ok_or_else(|| {
                CliError::Local(
                    "no service is reachable. Start one with `minato up`"
                        .to_string(),
                )
            })?,
    };

    let access = target.access().ok_or_else(|| {
        CliError::Local(format!(
            "service `{}` is not reachable yet (state: {})",
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
        // One undecorated line, ready to pipe.
        println!("{access}");
    }

    Ok(())
}

/// What `minato env get` prints: the value, on one line.
fn present_env_value(cli: &Cli, response: &Response, key: &str) -> Result<(), CliError> {
    let Response::Env { entries } = response else {
        return Err(CliError::Local(
            "cannot read the environment".to_string(),
        ));
    };

    let entry = entries
        .iter()
        .find(|entry| entry.key == key)
        .ok_or_else(|| {
            CliError::Local(format!(
                "`{key}` is not defined. Run `minato env ls` to see what is"
            ))
        })?;

    if cli.json {
        output::print_json(entry);
    } else {
        println!("{}", entry.value);
    }

    Ok(())
}

/// Shows the daemon's diagnostics with the host-side ones added.
///
/// The daemon cannot see `/etc/resolver` or whether the CA is trusted. The
/// CLI checks those itself and presents one combined result.
fn present_diagnostics(cli: &Cli, response: &Response) -> Result<(), CliError> {
    let Response::Diagnostics(diagnostics) = response else {
        return Err(CliError::Local(
            "cannot read the diagnostics".to_string(),
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

/// Walks through the privileged steps. **It never runs them.**
///
/// Running sudo on its own would hang an agent at the password prompt, and
/// from the user's side it would look like a silent privilege escalation.
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

    // (description, note, commands)
    let mut steps: Vec<(String, Option<String>, Vec<String>)> = Vec::new();
    let launchd_pending = pending("launchd");

    if launchd_pending {
        match prepare_launchd() {
            Ok((source, commands)) => steps.push((
                "let launchd hold 80/443/53 (the daemon itself stays non-root)"
                    .to_string(),
                Some(format!("generated plist: {}", source.display())),
                commands,
            )),
            Err(err) => eprintln!("warning: cannot write the plist: {err}"),
        }
    }

    // Installing launchd moves DNS to :53. A resolver still naming the
    // old port would stop resolving the moment it lands.
    let effective_dns_port = if launchd_pending {
        launchd::Ports::default().dns
    } else {
        dns_port.unwrap_or(53)
    };

    if pending("resolver") || launchd_pending {
        steps.push((
            format!("point *.{DEFAULT_DOMAIN_SUFFIX} at Minato's DNS"),
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
                "trust the local CA, so HTTPS stops warning".to_string(),
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
        println!("everything is set up. Confirm with `minato doctor`");
        return Ok(());
    }

    println!("The URLs need the following setup.");
    println!("It requires root, so read each command before running it.");

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
    println!("Afterwards run `minato daemon stop`; launchd will start it again.");

    if launchd_pending {
        println!();
        println!("To undo:");
        for command in launchd::uninstall_commands() {
            println!("   {command}");
        }
    }

    Ok(())
}

/// Picks the port out of a check's detail (`127.0.0.1:15353`).
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

/// Writes the launchd plist and returns the commands to install it.
fn prepare_launchd() -> anyhow::Result<(PathBuf, Vec<String>)> {
    let paths = minato_core::Paths::resolve()?;

    // The CLI and the daemon ship together, so it is next door.
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
                println!("minatod {} is running", pong.version);
            }
            Ok(ExitCode::SUCCESS)
        }
        DaemonCommand::Stop => {
            let mut connection = match client.connect().await {
                Ok(connection) => connection,
                Err(_) => {
                    if !cli.json {
                        println!("the daemon is not running");
                    }
                    return Ok(ExitCode::SUCCESS);
                }
            };

            connection.request(Request::Shutdown).await?;
            if !cli.json {
                println!("stopped the daemon");
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
                    println!("  uptime:   {}s", pong.uptime_secs);
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
                // Being stopped is not an error, so this succeeds.
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
        // An agent has to be able to reach for --json anywhere.
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
                assert!(start, "`minato new` brings the environment up by default");
                assert_eq!(branch, "feature/x");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn no_start_flag_is_respected() {
        let cli =
            Cli::try_parse_from(["minato", "new", "feature/x", "--no-start"]).expect("parses");
        let request = build_request(&cli, Target::new(PathBuf::from("/repo"))).expect("builds");

        match request {
            Request::New { start, .. } => assert!(!start),
            other => panic!("unexpected: {other:?}"),
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
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn url_asks_for_status() {
        // `url` has no request of its own; it reads the status result.
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
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn long_running_commands_show_progress() {
        // Whether progress is shown follows from the kind of request.
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

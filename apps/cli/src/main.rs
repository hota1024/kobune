//! minato — a thin client for the daemon.
//!
//! No logic lives here. Every decision is the daemon's; the CLI builds
//! requests and prints results (`docs/DESIGN.md` §3).

mod init;
mod launchd;
mod output;
mod skill;
mod system;
mod ui;
mod update;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use minato_api::{Request, Response, Target};

/// `0.1.0 (abc1234)`. Every nightly reports the same version, so the commit
/// is what tells one build from another.
fn version() -> &'static str {
    // Leaked once at startup: clap wants a `&'static str`, and the string is
    // built from two compile-time constants so there is nothing to free.
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| minato_core::version_string(env!("CARGO_PKG_VERSION")))
}

/// The suffix used when `[project] domain` is left out. It is also what
/// the resolver gets installed for.
const DEFAULT_DOMAIN_SUFFIX: &str = "localhost";
use minato_client::{Client, ClientError};

#[derive(Parser, Debug)]
#[command(
    name = "minato",
    version = version(),
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

        /// Rebuild images even when nothing Minato can see has changed
        #[arg(long)]
        build: bool,
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

        /// Rebuild images even when nothing Minato can see has changed
        ///
        /// A build is skipped when an image built from the same Dockerfile
        /// and build args is already there. That check cannot see a file
        /// the Dockerfile copies in, so this is how to pick one up.
        #[arg(long)]
        build: bool,
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

    /// Replace this installation with the latest build
    Update {
        /// Report whether a newer build exists, without installing it
        #[arg(long)]
        check: bool,
    },

    /// Print a shell completion script
    ///
    /// The installer writes these for you. This is how to do it by hand, or
    /// after adding a shell.
    Completions {
        /// bash, zsh, fish, elvish or powershell
        shell: clap_complete::Shell,
    },

    /// Reach this environment from outside, over Cloudflare Tunnel
    Tunnel {
        #[command(subcommand)]
        command: TunnelCommand,
    },
}

#[derive(Subcommand, Debug)]
enum TunnelCommand {
    /// Set the tunnel up and start it
    Enable {
        /// The Cloudflare zone the hostnames live under
        #[arg(long)]
        domain: Option<String>,

        /// Confirm that this goes on the public internet
        ///
        /// Minato cannot apply a Cloudflare Access policy — that needs the
        /// API, not cloudflared — so it will not expose an environment
        /// without being asked.
        #[arg(long)]
        public: bool,
    },

    /// Stop the tunnel
    Disable,

    /// Show where the tunnel stands
    Status,
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
    restore_sigpipe();

    let cli = Cli::parse();

    let outcome = run(&cli).await;

    // After the command, so a slow network cannot delay the output anyone is
    // waiting for. Never under `--json`: that stream is parsed, and a line
    // about a new build landing in it would be a bug rather than a nuisance.
    // stderr regardless, so `$(minato url web)` never picks it up.
    if !cli.json
        && wants_update_notice(&cli.command)
        && let Some(commit) = update_notice().await
    {
        ui::notice(vec![ui::hint(
            &format!("a newer build is available ({commit}). Install it with"),
            "minato update",
        )]);
    }

    match outcome {
        Ok(code) => code,
        Err(err) => {
            if cli.json {
                if let Some(api_error) = as_api_error(&err) {
                    output::print_error_json(api_error);
                } else {
                    output::print_error_json(&minato_api::ApiError::internal(err.to_string()));
                }
            } else {
                ui::error(&err.to_string(), hint_for(&err));
            }

            ExitCode::from(exit_code_for(&err) as u8)
        }
    }
}

/// Restores the default action for SIGPIPE.
///
/// Rust ignores it at startup, which turns a closed pipe into a write error
/// and then a panic inside `println!`. Since the documentation tells people
/// to pipe this — `minato url | …`, `minato logs | grep` — a panic on
/// `| head` is a papercut worth removing.
///
/// SAFETY: called once, before any thread is spawned, and restores the
/// disposition the operating system starts a process with.
fn restore_sigpipe() {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// Whether a command should carry the update notice.
///
/// `update` says everything there is to say about updates itself, and
/// following "installed c7282b8" with "a newer build is available" would
/// simply be wrong. `completions` is redirected into a file.
fn wants_update_notice(command: &Command) -> bool {
    !matches!(
        command,
        Command::Update { .. } | Command::Completions { .. }
    )
}

/// The once-a-day check, or nothing at all.
///
/// Silent on every failure, including having no configuration directory:
/// this runs after a command that already worked, and has nothing to add
/// when it cannot reach GitHub.
async fn update_notice() -> Option<String> {
    let paths = minato_core::Paths::resolve().ok()?;
    update::background_notice(&paths).await
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
            ui::done(
                "init",
                &[
                    ("created", outcome.path.display().to_string()),
                    ("project", outcome.project),
                ],
                vec![ui::hint("bring the environment up with", "minato up")],
            );
        }

        return Ok(ExitCode::SUCCESS);
    }

    // Installing the Skill needs no daemon either.
    if let Command::Skill { command } = &cli.command {
        return handle_skill(cli, command, &cwd);
    }

    // Updating talks to GitHub, not to the daemon.
    if let Command::Update { check } = &cli.command {
        return handle_update(cli, *check).await;
    }

    // Nor does printing a completion script.
    if let Command::Completions { shell } = &cli.command {
        let mut command = <Cli as CommandFactory>::command();
        let name = command.get_name().to_string();
        clap_complete::generate(*shell, &mut command, name, &mut std::io::stdout());
        return Ok(ExitCode::SUCCESS);
    }

    let client = Client::from_env().map_err(|err| {
        CliError::Local(format!("cannot resolve the configuration directory: {err}"))
    })?;

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

    // Ctrl-C asks the daemon to stop rather than killing the CLI where it
    // stands. Dropping the connection would leave the daemon working on
    // something nobody is waiting for, and say nothing about what it got
    // done. `logs -f` is the exception: Ctrl-C is how you leave it, and
    // there is nothing in flight to abandon.
    let response = if raw_output {
        connection
            .call(request, |event| output::print_output_event(&event))
            .await?
    } else {
        let progress = show_progress.then(ui::Progress::start);

        let result = connection
            .call_until(
                request,
                |event| {
                    if let Some(progress) = &progress {
                        progress.handle(&event);
                    }
                },
                {
                    let progress = progress.clone();
                    async move {
                        let _ = tokio::signal::ctrl_c().await;

                        let line = ui::note("stopping — the daemon is finishing what it can");
                        match &progress {
                            Some(progress) => progress.say(line),
                            None => ui::notice(vec![line]),
                        }
                    }
                },
            )
            .await;

        // Before the response is presented either way: the live line has
        // to be given back before anything else is printed, and an error
        // is printed too.
        if let Some(progress) = &progress {
            progress.finish();
        }

        result?
    };

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
            build,
        } => Request::New {
            target,
            branch: branch.clone(),
            base: base.clone(),
            path: path.clone(),
            start: !no_start,
            rebuild: *build,
        },
        Command::Rm { force } => Request::Rm {
            target,
            force: *force,
        },
        Command::Up { services, build } => Request::Up {
            target,
            services: services.clone(),
            rebuild: *build,
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
        Command::Tunnel { command } => match command {
            TunnelCommand::Enable { domain, public } => Request::TunnelEnable {
                target,
                domain: domain.clone(),
                public: *public,
            },
            TunnelCommand::Disable => Request::TunnelDisable { target },
            TunnelCommand::Status => Request::TunnelStatus { target },
        },
        Command::Doctor | Command::Setup => Request::Doctor { target },
        Command::Init { .. }
        | Command::Daemon { .. }
        | Command::Skill { .. }
        | Command::Completions { .. }
        | Command::Update { .. } => {
            unreachable!("the commands that need no daemon are handled before this")
        }
    };

    Ok(request)
}

/// Checks for a newer build, and installs it unless only asked to check.
async fn handle_update(cli: &Cli, check_only: bool) -> Result<ExitCode, CliError> {
    let status = update::check()
        .await
        .map_err(|err| CliError::Local(err.to_string()))?;

    let available = match &status {
        update::Status::Current => {
            if cli.json {
                output::print_json(&serde_json::json!({
                    "status": "current",
                    "commit": minato_core::BUILD_COMMIT,
                }));
            } else {
                ui::done(
                    "update",
                    &[("up to date", minato_core::BUILD_COMMIT_SHORT.to_string())],
                    vec![],
                );
            }
            return Ok(ExitCode::SUCCESS);
        }
        // Nothing to compare against, so nothing is claimed either way.
        // Saying "up to date" would be a guess, and saying "out of date"
        // would push someone off a build they made on purpose.
        update::Status::Unknown => {
            if cli.json {
                output::print_json(&serde_json::json!({
                    "status": "unknown",
                    "commit": minato_core::BUILD_COMMIT,
                }));
            } else {
                ui::done(
                    "update",
                    &[],
                    vec![ui::note("cannot tell: this build does not record a commit")],
                );
            }
            return Ok(ExitCode::SUCCESS);
        }
        update::Status::Available { commit } => commit.clone(),
    };

    let short: String = available.chars().take(7).collect();

    if check_only {
        if cli.json {
            output::print_json(&serde_json::json!({
                "status": "available",
                "commit": available,
                "running": minato_core::BUILD_COMMIT,
            }));
        } else {
            ui::done(
                "update",
                &[
                    ("available", short),
                    ("running", minato_core::BUILD_COMMIT_SHORT.to_string()),
                ],
                vec![ui::hint("install it with", "minato update")],
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    if !cli.json {
        ui::notice(vec![ui::note(&format!("installing {short}…"))]);
    }

    let installed = update::install()
        .await
        .map_err(|err| CliError::Local(err.to_string()))?;

    let installed_short: String = installed.chars().take(7).collect();

    if cli.json {
        output::print_json(&serde_json::json!({
            "status": "installed",
            "commit": installed,
        }));
    } else {
        // The running daemon is still the old binary. It is not restarted
        // here because that is launchd's job where launchd is installed,
        // and stopping it is what makes launchd pick the new one up.
        ui::done(
            "update",
            &[("installed", installed_short)],
            vec![
                ui::note("the running daemon is still the previous build"),
                ui::hint("replace it with", "minato daemon stop"),
            ],
        );
    }

    Ok(ExitCode::SUCCESS)
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
            } else {
                let label = if installed.overwritten {
                    "updated"
                } else {
                    "installed"
                };

                ui::done(
                    "skill",
                    &[(label, installed.path.display().to_string())],
                    vec![],
                );
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
        Response::Pong(pong) => ui::daemon(pong, None),
        Response::Workspaces { workspaces } => ui::workspaces(workspaces),
        Response::Diagnostics(diagnostics) => ui::diagnostics(diagnostics),
        Response::Env { entries } => {
            ui::env(entries);

            // A change does not reach containers that are already
            // running. Left unsaid, that reads as "I set it and nothing
            // happened".
            if matches!(
                cli.command,
                Command::Env {
                    command: EnvCommand::Set { .. } | EnvCommand::Unset { .. }
                }
            ) {
                ui::notice(vec![ui::hint(
                    "to pick this up, run",
                    "minato down && minato up",
                )]);
            }
        }
        Response::Workspace { workspace } => ui::workspace(workspace),
        Response::Tunnel(tunnel) => ui::tunnel(tunnel),
        // logs has already printed its lines; exec speaks through its
        // exit code.
        Response::Exec { .. } => {}
        Response::Empty if matches!(cli.command, Command::Logs { .. }) => {}
        Response::Empty => ui::confirm("done"),
    }

    Ok(())
}

fn present_url(cli: &Cli, response: &Response, service: Option<&str>) -> Result<(), CliError> {
    let Response::Workspace { workspace } = response else {
        return Err(CliError::Local("cannot read the workspace".to_string()));
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
                CliError::Local("no service is reachable. Start one with `minato up`".to_string())
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
        ui::value(&access);
    }

    Ok(())
}

/// What `minato env get` prints: the value, on one line.
fn present_env_value(cli: &Cli, response: &Response, key: &str) -> Result<(), CliError> {
    let Response::Env { entries } = response else {
        return Err(CliError::Local("cannot read the environment".to_string()));
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
        ui::value(&entry.value);
    }

    Ok(())
}

/// Shows the daemon's diagnostics with the host-side ones added.
///
/// The daemon cannot see `/etc/resolver` or whether the CA is trusted. The
/// CLI checks those itself and presents one combined result.
fn present_diagnostics(cli: &Cli, response: &Response) -> Result<(), CliError> {
    let Response::Diagnostics(diagnostics) = response else {
        return Err(CliError::Local("cannot read the diagnostics".to_string()));
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
        ui::diagnostics(&combined);
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

    let mut steps: Vec<ui::SetupStep> = Vec::new();
    let launchd_pending = pending("launchd");

    if launchd_pending {
        match prepare_launchd() {
            Ok((source, commands)) => steps.push(ui::SetupStep {
                description: "let launchd hold 80/443/53 (the daemon itself stays non-root)"
                    .to_string(),
                note: Some(format!("generated plist: {}", source.display())),
                commands,
            }),
            Err(err) => ui::error(&format!("cannot write the plist: {err}"), None),
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
        steps.push(ui::SetupStep {
            description: format!("point *.{DEFAULT_DOMAIN_SUFFIX} at Minato's DNS"),
            note: None,
            commands: vec![system::resolver_command(
                DEFAULT_DOMAIN_SUFFIX,
                effective_dns_port,
            )],
        });
    }

    if pending("ca-trust")
        && let Some(path) = ca_path
    {
        steps.push(ui::SetupStep {
            description: "trust the local CA, so HTTPS stops warning".to_string(),
            note: None,
            commands: vec![system::trust_command(path)],
        });
    }

    if cli.json {
        output::print_json(&serde_json::json!({ "steps": steps }));
        return Ok(());
    }

    // Only the LaunchDaemon leaves anything behind to take back out.
    let undo = if launchd_pending {
        launchd::uninstall_commands()
    } else {
        Vec::new()
    };

    ui::setup(&steps, &undo);

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
                ui::daemon(&pong, Some(client.socket_path()));
            }
            Ok(ExitCode::SUCCESS)
        }
        DaemonCommand::Stop => {
            let mut connection = match client.connect().await {
                Ok(connection) => connection,
                Err(_) => {
                    if !cli.json {
                        ui::confirm("the daemon is not running");
                    }
                    return Ok(ExitCode::SUCCESS);
                }
            };

            connection.request(Request::Shutdown).await?;
            if !cli.json {
                ui::confirm("stopped the daemon");
            }
            Ok(ExitCode::SUCCESS)
        }
        DaemonCommand::Status => match client.connect().await {
            Ok(mut connection) => {
                let pong = connection.handshake().await?;
                if cli.json {
                    output::print_json(&pong);
                } else {
                    ui::daemon(&pong, Some(client.socket_path()));
                }
                Ok(ExitCode::SUCCESS)
            }
            Err(_) => {
                if cli.json {
                    output::print_json(&serde_json::json!({ "running": false }));
                } else {
                    ui::daemon_stopped();
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
    fn enabling_a_tunnel_requires_saying_public_out_loud() {
        // Minato cannot apply a Cloudflare Access policy, so it cannot
        // promise one is there. The flag is the acknowledgement, and it
        // defaults off.
        let cli = Cli::try_parse_from(["minato", "tunnel", "enable", "--domain", "example.com"])
            .expect("parses");
        let request = build_request(&cli, Target::new(PathBuf::from("/repo"))).expect("builds");

        match request {
            Request::TunnelEnable { public, domain, .. } => {
                assert!(!public, "public is opt-in");
                assert_eq!(domain.as_deref(), Some("example.com"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn a_domain_can_be_left_out_once_it_is_known() {
        let cli = Cli::try_parse_from(["minato", "tunnel", "enable", "--public"]).expect("parses");
        let request = build_request(&cli, Target::new(PathBuf::from("/repo"))).expect("builds");

        match request {
            Request::TunnelEnable { domain, public, .. } => {
                assert!(domain.is_none(), "the daemon reuses the configured one");
                assert!(public);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn tunnel_status_asks_for_nothing_else() {
        let cli = Cli::try_parse_from(["minato", "tunnel", "status"]).expect("parses");
        let request = build_request(&cli, Target::new(PathBuf::from("/repo"))).expect("builds");

        assert!(matches!(request, Request::TunnelStatus { .. }));
    }

    #[test]
    fn long_running_commands_show_progress() {
        // Whether progress is shown follows from the kind of request.
        assert!(
            Request::Up {
                target: Target::new(PathBuf::from("/repo")),
                services: vec![],
                rebuild: false,
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

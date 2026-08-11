//! minato — a thin client for the daemon.
//!
//! No logic lives here. Every decision is the daemon's; the CLI builds
//! requests and prints results (`docs/DESIGN.md` §3).

mod attach;
mod init;
mod launchd;
mod output;
mod skill;
mod system;
mod ui;
mod uninstall;
mod update;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use minato_api::{Event, Request, Response, Target};
use minato_client::{Client, ClientError, DaemonStart};

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
    ///
    /// On a terminal each step is shown and then offered, one at a time,
    /// and only what you say yes to is run. With no terminal to ask at —
    /// an agent, a pipe, `--json` — the commands are printed for you to
    /// run yourself, which is what this command has always done.
    Setup {
        /// Run every step without asking
        #[arg(long, short = 'y')]
        yes: bool,

        /// Print the commands and run none of them
        #[arg(long)]
        dry_run: bool,
    },

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
    ///
    /// `-f` on a single service that has `tty = true` hands this terminal
    /// over: colour comes through and what you type reaches the program,
    /// which is what turborepo and other full-screen tools want. Ctrl-P
    /// then Ctrl-Q gives the terminal back, leaving the service running.
    Logs {
        /// Which services. All of them when left out
        services: Vec<String>,

        /// Keep waiting for new lines
        #[arg(long, short)]
        follow: bool,

        /// How many lines to show from the end
        #[arg(long, short = 'n')]
        tail: Option<usize>,

        /// Read only; do not offer this terminal to the service
        ///
        /// For watching a service that takes input without being able to
        /// type at it by accident.
        #[arg(long)]
        no_input: bool,
    },

    /// Run a command inside a container
    ///
    /// The command's exit code is passed straight through.
    Exec {
        /// Run in a throwaway container instead of the running one
        ///
        /// Same image, environment and volumes, without the service's
        /// start-up command, and removed when the command finishes. The
        /// service does not have to be running — which is the point, since
        /// a start-up script that fails leaves nothing to exec into.
        #[arg(long)]
        fresh: bool,

        /// Where to run it. The service's workdir when left out
        ///
        /// `-C` rather than `-w`, which is taken by `--workspace`.
        #[arg(long, short = 'C')]
        workdir: Option<String>,

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

    /// Take Minato back off this machine
    ///
    /// Containers, the daemon's state, the binaries and the shell
    /// completions. **Worktrees are left alone** — they are your
    /// checkouts, and `minato rm` is how one goes.
    ///
    /// What it found is shown first, and nothing happens until you say so.
    Uninstall {
        /// Go ahead without asking
        ///
        /// Required when there is no terminal to ask at, which is what an
        /// agent or a pipe has.
        #[arg(long, short = 'y')]
        yes: bool,

        /// List what would go, and remove nothing
        #[arg(long)]
        dry_run: bool,
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
    ///
    /// Without --service, only what every service shares. A service's own
    /// `env` in minato.toml belongs to that service.
    Ls {
        /// Show the values instead of masking them
        #[arg(long)]
        reveal: bool,

        /// Show what this service is given, its own env included
        #[arg(long, short = 's')]
        service: Option<String>,
    },

    /// Print one value, ready to pipe
    Get {
        key: String,

        /// Read it as this service would see it
        #[arg(long, short = 's')]
        service: Option<String>,
    },

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

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            if let Some(group) = missing_subcommand(&err) {
                return print_group_help(&group, wants_json());
            }

            if err.kind() == clap::error::ErrorKind::DisplayVersion {
                return print_version(&err).await;
            }

            err.exit()
        }
    };

    let outcome = run(&cli).await;

    // After the command, so a slow network cannot delay the output anyone is
    // waiting for. Never under `--json`: that stream is parsed, and a line
    // about a new build landing in it would be a bug rather than a nuisance.
    // stderr regardless, so `$(minato url web)` never picks it up.
    if !cli.json
        && wants_update_notice(&cli.command)
        && let Some(commit) = update_notice().await
    {
        print_update_notice(&commit);
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

/// The group a parse failure is about, when it is about a group that was
/// named without one of its subcommands — `["skill"]` for `minato skill`,
/// and empty for `minato` itself. `None` for every other failure.
///
/// Clap answers `minato skill` with the group's help, but only because
/// nothing at all followed it: `arg_required_else_help` counts arguments,
/// and a global flag is one. So `minato skill --json` came back as a usage
/// error instead — the same missing subcommand, a different answer,
/// decided by a flag that says nothing about which subcommand was meant.
fn missing_subcommand(err: &clap::Error) -> Option<Vec<String>> {
    use clap::error::{ContextKind, ContextValue, ErrorKind};

    if err.kind() != ErrorKind::MissingSubcommand {
        return None;
    }

    // Clap names the invocation that was left without one, the binary
    // first: `minato skill`.
    let Some(ContextValue::String(invocation)) = err.get(ContextKind::InvalidSubcommand) else {
        return None;
    };

    Some(
        invocation
            .split_whitespace()
            .skip(1)
            .map(str::to_string)
            .collect(),
    )
}

/// Prints a group's help, and gives back the code to leave with.
///
/// Under `--json` it goes to stderr, and unstyled: stdout carries the one
/// JSON document a command answers with, this is not one, and nobody
/// parsing it wants escape codes. Everywhere else it is stdout, which is
/// where `minato skill` has always put it.
fn print_group_help(group: &[String], json: bool) -> ExitCode {
    // Built first, so the groups carry their full name: the usage line
    // reads `minato skill`, not `skill`.
    let mut help = <Cli as CommandFactory>::command();
    help.build();

    for name in group {
        // Clap named the group, so it is there. The root's help is still
        // an answer if that ever stops being true.
        let Some(sub) = help.find_subcommand(name).cloned() else {
            break;
        };
        help = sub;
    }

    if json {
        eprint!("{}", help.render_help());
    } else {
        let _ = help.print_help();
    }

    // 2, the usage code — what clap leaves with both for this error and
    // for the help it prints when the group is named with nothing after
    // it at all.
    ExitCode::from(2)
}

/// `--version`, and the update check it carries.
///
/// The version line is clap's and goes out first, so someone asking what
/// they are running has the answer before anything touches the network —
/// and has it at all when there is no network to touch. The check follows,
/// and only says anything when a newer build exists: which build this is
/// was the question, and the line above already answered it.
///
/// It asks every time rather than once a day, for the reason
/// [`update::version_notice`] gives.
async fn print_version(version: &clap::Error) -> ExitCode {
    // Clap writes it to stdout, this being a request rather than a failure.
    // Stdout is line buffered and the line ends in a newline, so it is gone
    // before the notice that may follow it on stderr.
    let _ = version.print();

    // Never under `--json`: that stream is parsed, and stderr is the other
    // half of what `2>&1` captures.
    if !wants_json()
        && let Some(commit) = version_update_notice().await
    {
        print_update_notice(&commit);
    }

    ExitCode::SUCCESS
}

/// Whether `--json` was asked for, read off the command line.
///
/// The parse failed, so there is no [`Cli`] to ask. Only the exact flag
/// counts, which is all the two callers need: it decides which stream a
/// group's help goes to, and whether `--version` adds its update notice.
fn wants_json() -> bool {
    std::env::args().any(|arg| arg == "--json")
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

/// Says so when the daemon that just started cannot hold 80 or 443.
///
/// **Only when a LaunchDaemon is installed.** Without one, listening
/// elsewhere is the normal arrangement and saying so every time would be
/// noise. With one, it means launchd was meant to hand the ports over and
/// did not — the state that leaves every URL dead while `minato up` still
/// reports success.
fn warn_if_unprivileged(cli: &Cli, start: DaemonStart) {
    if cli.json || start != DaemonStart::Direct || !minato_core::launchd::is_installed() {
        return;
    }

    ui::notice(vec![
        ui::note("started a daemon outside launchd, so 80 and 443 are out and no URL will answer"),
        ui::hint(
            "bring socket activation back with",
            &minato_core::launchd::kickstart_command(),
        ),
    ]);
}

/// Whether a command should carry the update notice.
///
/// `update` says everything there is to say about updates itself, and
/// following "installed c7282b8" with "a newer build is available" would
/// simply be wrong. `completions` is redirected into a file.
fn wants_update_notice(command: &Command) -> bool {
    !matches!(
        command,
        // `uninstall` has just removed the binary, and often the cache
        // the check reads, so it would go to GitHub to recommend
        // reinstalling something the user has this second thrown away.
        Command::Update { .. } | Command::Completions { .. } | Command::Uninstall { .. }
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

/// The check `--version` makes, which asks every time rather than once a
/// day.
///
/// Silent on the same failures, including having no configuration directory
/// to leave the answer in.
async fn version_update_notice() -> Option<String> {
    let paths = minato_core::Paths::resolve().ok()?;
    update::version_notice(&paths).await
}

/// What either check has to say, in the one wording both use.
///
/// On stderr, through [`ui::notice`]: `$(minato url web)` must never pick
/// it up, and neither must anything parsing `--json`.
fn print_update_notice(commit: &str) {
    ui::notice(vec![ui::hint(
        &format!("a newer build is available ({commit}). Install it with"),
        "minato update",
    )]);
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

    // Uninstalling is half the daemon's and half this side's, and it has
    // to ask before doing either.
    if let Command::Uninstall { yes, dry_run } = &cli.command {
        return handle_uninstall(cli, &client, *yes, *dry_run).await;
    }

    let target = Target::new(cwd).workspace(cli.workspace.clone());
    let request = build_request(cli, target)?;

    let (mut connection, start) = client.connect_or_spawn().await?;
    warn_if_unprivileged(cli, start);

    // An interactive `logs` runs its own way: the terminal goes into raw
    // mode, and what comes back is not lines but a screen.
    if matches!(
        &request,
        Request::Logs {
            interactive: true,
            ..
        }
    ) {
        return run_attached(&mut connection, request).await;
    }

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

    let code = present(cli, &response)?;

    // exec passes the command's exit code straight through: an agent has
    // to be able to judge `minato exec web -- pnpm test` by exit status
    // alone.
    if let Response::Exec { exit_code } = &response {
        return Ok(ExitCode::from(clamp_exit_code(*exit_code)));
    }

    Ok(code)
}

/// Runs a `logs` that has this terminal lent to it.
///
/// The daemon decides whether the offer is taken up. Until [`Event::Attached`]
/// says it was, this behaves exactly like a plain `logs` — including
/// printing the reason it was not, which is the whole point of finding out
/// from an event rather than assuming.
async fn run_attached(
    connection: &mut minato_client::Connection,
    request: Request,
) -> Result<ExitCode, CliError> {
    let (typed, keys) = tokio::sync::mpsc::unbounded_channel();

    // **Taken, not cloned.** The last sender going is how the client is
    // told the person has detached, so this one has to be handed to the
    // pump rather than kept alive here beside it.
    let mut typed = Some(typed);
    let mut session = None;

    let outcome = connection
        .call_attached(
            request,
            |event| match event {
                Event::Attached { service } => {
                    attach::announce(&service);

                    match typed.take().map(attach::Session::start) {
                        Some(Ok(started)) => session = Some(started),
                        Some(Err(err)) => eprintln!("error: cannot take the terminal: {err}"),
                        None => {}
                    }
                }
                Event::Bytes { data, .. } => {
                    attach::Session::show(&minato_api::decode_bytes(&data))
                }
                other => output::print_output_event(&other),
            },
            keys,
        )
        .await;

    // **Before anything else is printed.** Raw mode is still on until the
    // session is dropped, and a message written under it comes out as a
    // staircase.
    let was_attached = session.is_some();
    drop(session);

    if was_attached {
        attach::restore();
    }

    outcome?;
    Ok(ExitCode::SUCCESS)
}

/// Whether `logs` should offer this terminal to the service.
///
/// Every condition is about not surprising anyone. Handing the terminal
/// over is the default where it can hardly mean anything else — someone
/// following one named service from a terminal — and quietly not the
/// default anywhere else: a pipeline, an agent reading `--json`, and a
/// person watching every service at once all want the plain stream.
///
/// Whether the service *has* a terminal is the daemon's to say. Only it
/// has read `minato.toml`, and it answers by attaching or by explaining
/// why it did not.
fn wants_to_type(
    cli: &Cli,
    services: &[String],
    follow: bool,
    no_input: bool,
    terminal: bool,
) -> bool {
    follow && !no_input && !cli.json && services.len() == 1 && terminal
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
            no_input,
        } => Request::Logs {
            target,
            services: services.clone(),
            follow: *follow,
            tail: *tail,
            window: attach::window(),
            interactive: wants_to_type(cli, services, *follow, *no_input, attach::is_a_terminal()),
        },
        Command::Exec {
            fresh,
            workdir,
            service,
            command,
        } => Request::Exec {
            target,
            service: service.clone(),
            command: command.clone(),
            fresh: *fresh,
            workdir: workdir.clone(),
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
        Command::Doctor | Command::Setup { .. } => Request::Doctor { target },
        Command::Init { .. }
        | Command::Daemon { .. }
        | Command::Skill { .. }
        | Command::Completions { .. }
        | Command::Uninstall { .. }
        | Command::Update { .. } => {
            unreachable!("the commands that need no daemon are handled before this")
        }
    };

    Ok(request)
}

/// What an update run came to.
enum Updated {
    /// Running the build that is published.
    Current,
    /// Nothing to compare against.
    Unknown,
    /// A newer build exists and was only reported, not fetched.
    Available(String),
    Installed(String),
}

/// Checks for a newer build, and installs it unless only asked to check.
///
/// The work is done in [`run_update`], which draws the live display, and
/// the answer is printed here — after the display has given the screen
/// back. A panel written into a viewport that is still holding a line
/// would be drawn over by the next repaint.
async fn handle_update(cli: &Cli, check_only: bool) -> Result<ExitCode, CliError> {
    // Nothing is drawn under `--json`: exactly one JSON document comes out
    // of this command, and a spinner in it would be a bug.
    let progress = (!cli.json).then(ui::Progress::start);

    let outcome = run_update(check_only, progress.as_ref()).await;

    if let Some(progress) = &progress {
        progress.finish();
    }

    match outcome? {
        Updated::Current => {
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
        }
        // Nothing to compare against, so nothing is claimed either way.
        // Saying "up to date" would be a guess, and saying "out of date"
        // would push someone off a build they made on purpose.
        Updated::Unknown => {
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
        }
        Updated::Available(commit) => {
            if cli.json {
                output::print_json(&serde_json::json!({
                    "status": "available",
                    "commit": commit,
                    "running": minato_core::BUILD_COMMIT,
                }));
            } else {
                ui::done(
                    "update",
                    &[
                        ("available", short_commit(&commit)),
                        ("running", minato_core::BUILD_COMMIT_SHORT.to_string()),
                    ],
                    vec![ui::hint("install it with", "minato update")],
                );
            }
        }
        Updated::Installed(commit) => {
            if cli.json {
                output::print_json(&serde_json::json!({
                    "status": "installed",
                    "commit": commit,
                }));
            } else {
                // The running daemon is still the old binary. It is not
                // restarted here because that is launchd's job where
                // launchd is installed, and stopping it is what makes
                // launchd pick the new one up.
                ui::done(
                    "update",
                    &[("installed", short_commit(&commit))],
                    vec![
                        ui::note("the running daemon is still the previous build"),
                        ui::hint("replace it with", "minato daemon stop"),
                    ],
                );
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// The steps of an update, named for the display.
///
/// Only the download reports a size; the rest are quick enough that the
/// spinner is the whole of what there is to show.
const CHECKING: &str = "checking";
const DOWNLOADING: &str = "downloading";
const VERIFYING: &str = "verifying";
const UNPACKING: &str = "unpacking";
const INSTALLING: &str = "installing";

/// Asks GitHub, and fetches when there is something to fetch.
///
/// Every step is announced before it is attempted, because each one can
/// take long enough on a slow network to look like a hang, and "checking
/// for a newer build" is the difference between waiting and wondering.
async fn run_update(
    check_only: bool,
    progress: Option<&ui::Progress>,
) -> Result<Updated, CliError> {
    if let Some(progress) = progress {
        progress.begin(CHECKING, "checking for a newer build");
    }

    let status = update::check()
        .await
        .map_err(|err| CliError::Local(err.to_string()))?;

    if let Some(progress) = progress {
        progress.settle(CHECKING);
    }

    let available = match status {
        update::Status::Current => return Ok(Updated::Current),
        update::Status::Unknown => return Ok(Updated::Unknown),
        update::Status::Available { commit } => commit,
    };

    if check_only {
        return Ok(Updated::Available(available));
    }

    if let Some(progress) = progress {
        progress.begin(
            DOWNLOADING,
            &format!("downloading {}", short_commit(&available)),
        );
    }

    let installed = update::install(|stage| {
        let Some(progress) = progress else {
            return;
        };

        // Each step but the first ends the one before it, so the history
        // fills in as the update goes rather than all at the end.
        match stage {
            update::Stage::Downloading { done, total } => {
                progress.advance(DOWNLOADING, done, total)
            }
            update::Stage::Verifying => {
                progress.settle(DOWNLOADING);
                progress.begin(VERIFYING, "verifying the checksum");
            }
            update::Stage::Unpacking => {
                progress.settle(VERIFYING);
                progress.begin(UNPACKING, "unpacking the archive");
            }
            update::Stage::Installing => {
                progress.settle(UNPACKING);
                progress.begin(INSTALLING, "installing minato and minatod");
            }
        }
    })
    .await
    .map_err(|err| CliError::Local(err.to_string()))?;

    if let Some(progress) = progress {
        progress.settle(INSTALLING);
    }

    Ok(Updated::Installed(installed))
}

/// A commit at the length that tells a reader something.
fn short_commit(commit: &str) -> String {
    commit.chars().take(7).collect()
}

/// Takes Minato off the machine.
///
/// Two halves. The daemon takes down what it made, because only it knows
/// what that is; everything else is removed here, because only this side
/// knows where it was installed from.
///
/// **Nothing happens before the list has been shown.** A terminal is asked
/// to confirm; anywhere else — a pipe, an agent — `--yes` is required, so
/// a command that cannot ask cannot proceed by accident.
async fn handle_uninstall(
    cli: &Cli,
    client: &Client,
    yes: bool,
    dry_run: bool,
) -> Result<ExitCode, CliError> {
    // Asking the daemon first, and only for a report. It may not be
    // running, and it may not be able to start — neither is a reason to
    // leave the rest of an uninstall undone.
    let mut connection = client.connect().await.ok();

    // Why it could not answer matters. "There is no daemon" means there is
    // nothing of its to remove; "the daemon said no" means there may well
    // be, and it is about to become unreachable. Collapsing the two would
    // let an uninstall leave containers running and say nothing.
    let daemon: Result<minato_api::PurgeReport, String> = match &mut connection {
        None => Err("it is not running, so it has nothing to take down".to_string()),
        Some(connection) => match connection.request(Request::Purge { dry_run: true }).await {
            Ok(Response::Purge(report)) => Ok(report),
            Ok(other) => Err(format!("it answered with {other:?} instead of a report")),
            Err(err) => Err(err.to_string()),
        },
    };

    // The CA lives under the daemon's root whether or not it answers, so
    // the keychain step does not depend on reaching it.
    let ca_path = minato_core::Paths::resolve()
        .ok()
        .map(|paths| paths.ca_dir().join("minato-ca.crt"));

    let plan = uninstall::plan(DEFAULT_DOMAIN_SUFFIX, ca_path.as_deref());

    let nothing_to_do = plan.is_empty()
        && daemon
            .as_ref()
            .map(|report| report.is_empty())
            .unwrap_or(true);

    // Under `--json` exactly one document comes back, so the plan is held
    // until the end and the outcome folded into it. Printing it here and
    // the result afterwards would put two documents on a stream that is
    // being parsed.
    let as_json = |removed: bool, failures: &[String], remaining: &[uninstall::Privileged]| {
        serde_json::json!({
            "removed": removed,
            "dry_run": dry_run,
            "daemon": daemon.as_ref().ok(),
            "daemon_error": daemon.as_ref().err(),
            "files": plan.files,
            "privileged": plan.privileged,
            "failures": failures,
            "remaining": remaining,
        })
    };

    if !cli.json {
        ui::uninstall_plan(&plan, daemon.as_ref(), dry_run);
    }

    if dry_run || nothing_to_do {
        if cli.json {
            output::print_json(&as_json(false, &[], &[]));
        }
        return Ok(ExitCode::SUCCESS);
    }

    if !yes && !confirm("Remove all of this?")? {
        if cli.json {
            output::print_json(&as_json(false, &[], &[]));
        } else {
            ui::confirm("nothing was removed");
        }
        return Ok(ExitCode::SUCCESS);
    }

    // The containers first: once the daemon is gone, nothing knows their
    // names.
    if let Some(connection) = &mut connection {
        let progress = (!cli.json).then(ui::Progress::start);

        // `call_until` rather than `call`, for the reason every other
        // long-running request uses it: Ctrl-C should ask the daemon to
        // stop and wait for its answer. Dropping the connection here would
        // leave it destroying workspaces for a CLI that has gone, and skip
        // everything below — the privileged steps, the shutdown, the
        // files — leaving a machine half uninstalled and unreported.
        let outcome = connection
            .call_until(
                Request::Purge { dry_run: false },
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

        if let Some(progress) = &progress {
            progress.finish();
        }

        if let Err(err) = outcome {
            ui::error(&format!("cannot take the containers down: {err}"), None);
        }
    }

    // Order matters here, and it is not the obvious one.
    //
    // The privileged steps come before anything is deleted, because two of
    // them depend on the state that deleting would destroy.
    //
    // `security remove-trusted-cert` names a *file*. Remove `~/.minato`
    // first and there is no certificate left to point at, the command
    // fails, and the CA stays trusted for good — which is the one thing an
    // uninstall really has to get right, since a trusted CA goes on
    // trusting anything signed by it.
    //
    // Booting the LaunchDaemon out has to come before asking the daemon to
    // stop, too. That is the whole point of launchd: `minato daemon stop`
    // with it installed is immediately followed by launchd starting the
    // daemon again, which would recreate the state directory a moment
    // before it was deleted.
    let privileged = run_privileged(&plan, yes, cli.json);

    // Now nothing will restart it, so it can go. This releases the socket
    // and stops it writing the state file back out from memory.
    if let Some(connection) = &mut connection {
        let _ = connection.request(Request::Shutdown).await;
    }

    let removed = uninstall::remove_files(&plan);

    // A file root owns is a second round of privileged work, found only by
    // trying. Running it now keeps the whole thing to one password prompt
    // from the user's point of view.
    let mut privileged = privileged;
    if !removed.needs_root.is_empty() {
        let second = uninstall::Plan {
            files: Vec::new(),
            privileged: removed.needs_root,
        };
        privileged.extend(run_privileged(&second, yes, cli.json));
    }

    let failures = removed.failures;

    if cli.json {
        output::print_json(&as_json(true, &failures, &privileged));
    } else {
        ui::uninstall_done(&failures, &privileged);
    }

    // Anything left undone is a failure, whichever half it is in. An
    // `uninstall --yes` from CI that could not run the privileged steps
    // leaves the LaunchDaemon installed — socket-activated on 80/443/53,
    // pointing at a binary that has just been deleted — and the CA still
    // trusted. Exiting 0 there would report that as a clean uninstall.
    let stranded = daemon
        .as_ref()
        .map(|report| !report.stranded.is_empty())
        .unwrap_or(false);

    Ok(
        if failures.is_empty() && privileged.is_empty() && !stranded {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        },
    )
}

/// Runs the steps that need root, and returns whatever is left to do.
///
/// sudo only where there is a terminal to type a password into. Under an
/// agent or a pipe it would hang at the prompt with nothing to say — the
/// same reason `minato setup` only walks through its steps on a terminal —
/// so there the commands are handed back to be run by hand.
fn run_privileged(plan: &uninstall::Plan, yes: bool, json: bool) -> Vec<uninstall::Privileged> {
    use std::io::IsTerminal;

    if plan.privileged.is_empty() {
        return Vec::new();
    }

    if !std::io::stdin().is_terminal() {
        return plan.privileged.clone();
    }

    // `--yes` skipped the question about the whole plan, and these are the
    // part of it that touches the system rather than this user's files.
    if !yes {
        ui::notice(vec![ui::note(
            "the next steps need root; sudo will ask for your password",
        )]);
    }

    let mut remaining = Vec::new();

    for step in &plan.privileged {
        let failed: Vec<String> = step
            .commands
            .iter()
            .filter(|command| !run_shell(command, json))
            .cloned()
            .collect();

        if !failed.is_empty() {
            remaining.push(uninstall::Privileged {
                label: step.label.clone(),
                commands: failed,
            });
        }
    }

    remaining
}

/// Runs one command through the shell, and says whether it worked.
///
/// Through `sh -c` because the commands are pipelines — `printf … | sudo
/// tee …` — and they are the same strings `minato setup` prints, so what
/// runs is what the documentation says runs.
fn run_shell(command: &str, quiet_stdout: bool) -> bool {
    let mut shell = std::process::Command::new("sh");
    shell.arg("-c").arg(command);

    // `update-ca-certificates` and friends write to stdout, which under
    // `--json` is the stream carrying the one document this command
    // promises. Their output is worth keeping, so it moves to stderr
    // rather than being thrown away.
    if quiet_stdout {
        shell.stdout(std::process::Stdio::null());
    }

    shell
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Asks, on a terminal.
///
/// Anywhere else this is an error rather than a default: something that
/// cannot be asked must not be assumed to have agreed.
fn confirm(question: &str) -> Result<bool, CliError> {
    use std::io::{IsTerminal, Write};

    if !std::io::stdin().is_terminal() {
        return Err(CliError::Local(
            "there is no terminal to confirm at. Pass --yes to go ahead, or \
             --dry-run to see the list first"
                .to_string(),
        ));
    }

    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();

    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|err| CliError::Local(format!("cannot read the answer: {err}")))?;

    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
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
        EnvCommand::Ls { reveal, service } => Request::EnvList {
            target,
            reveal: *reveal,
            service: service.clone(),
        },
        // `get` pulls from the listing, and the value itself is the
        // point, so nothing is masked.
        EnvCommand::Get { service, .. } => Request::EnvList {
            target,
            reveal: true,
            service: service.clone(),
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

/// Shows a response, and says what the command should leave with.
///
/// Almost everything here is a success by the time it is being printed.
/// `setup` is the exception: it runs commands, and one of them coming back
/// non-zero is a failure however well it is presented.
fn present(cli: &Cli, response: &Response) -> Result<ExitCode, CliError> {
    // `url` prints differently: one line, ready to pipe.
    if let Command::Url { service } = &cli.command {
        return present_url(cli, response, service.as_deref());
    }

    // `env get` prints one line too, for the same reason.
    if let Command::Env {
        command: EnvCommand::Get { key, .. },
    } = &cli.command
    {
        return present_env_value(cli, response, key);
    }

    // `doctor` and `setup` add the host-side checks to the daemon's.
    if matches!(cli.command, Command::Doctor | Command::Setup { .. }) {
        return present_diagnostics(cli, response);
    }

    if cli.json {
        output::print_json(response);
        return Ok(ExitCode::SUCCESS);
    }

    match response {
        Response::Pong(pong) => ui::daemon(pong, None),
        Response::Workspaces { workspaces } => ui::workspaces(workspaces),
        Response::Diagnostics(diagnostics) => ui::diagnostics(diagnostics),
        Response::Env { entries, .. } => {
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
        // exit code. `uninstall` presents its own two halves — the plan
        // and the outcome — and never reaches here.
        Response::Exec { .. } | Response::Purge(_) => {}
        Response::Empty if matches!(cli.command, Command::Logs { .. }) => {}
        Response::Empty => ui::confirm("done"),
    }

    Ok(ExitCode::SUCCESS)
}

fn present_url(
    cli: &Cli,
    response: &Response,
    service: Option<&str>,
) -> Result<ExitCode, CliError> {
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

    Ok(ExitCode::SUCCESS)
}

/// What `minato env get` prints: the value, on one line.
fn present_env_value(cli: &Cli, response: &Response, key: &str) -> Result<ExitCode, CliError> {
    let Response::Env { entries, .. } = response else {
        return Err(CliError::Local("cannot read the environment".to_string()));
    };

    let entry = entries
        .iter()
        .find(|entry| entry.key == key)
        .ok_or_else(|| {
            CliError::Local(format!(
                "`{key}` is not defined. Run `minato env ls` to see what is, \
                 or `minato env ls --service <name>` for one service's own"
            ))
        })?;

    if cli.json {
        output::print_json(entry);
    } else {
        ui::value(&entry.value);
    }

    Ok(ExitCode::SUCCESS)
}

/// Shows the daemon's diagnostics with the host-side ones added.
///
/// The daemon cannot see `/etc/resolver` or whether the CA is trusted. The
/// CLI checks those itself and presents one combined result.
fn present_diagnostics(cli: &Cli, response: &Response) -> Result<ExitCode, CliError> {
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

    if let Command::Setup { yes, dry_run } = &cli.command {
        return present_setup(cli, &combined, dns_port, ca_path.as_deref(), *yes, *dry_run);
    }

    if cli.json {
        output::print_json(&combined);
    } else {
        ui::diagnostics(&combined);
    }

    Ok(ExitCode::SUCCESS)
}

/// Walks through the privileged steps, asking before each one.
///
/// **Nothing runs unasked.** sudo started on its own would hang an agent at
/// the password prompt, and from the user's side it would look like a
/// silent privilege escalation — so the walk happens only where there is a
/// terminal to answer at, each command is on the screen before it is
/// offered, and anywhere else the commands are printed to be run by hand.
fn present_setup(
    cli: &Cli,
    diagnostics: &minato_api::Diagnostics,
    dns_port: Option<u16>,
    ca_path: Option<&std::path::Path>,
    yes: bool,
    dry_run: bool,
) -> Result<ExitCode, CliError> {
    let pending = |id: &str| {
        diagnostics
            .checks
            .iter()
            .any(|check| check.id == id && check.status != minato_api::CheckStatus::Ok)
    };

    let mut steps: Vec<ui::SetupStep> = Vec::new();
    let launchd_pending = pending("launchd");

    // Privileged ports being unavailable has two causes, and they take
    // opposite steps. **Whether launchd already has the job is what tells
    // them apart** — see [`minato_core::launchd::is_loaded`]. Asked only
    // where the answer can matter: with the sockets already handed over
    // there is nothing to install and nothing to wake.
    let wakes_launchd = launchd_pending && minato_core::launchd::is_loaded();
    let mut launchd_step = None;

    if wakes_launchd {
        launchd_step = Some(steps.len());
        steps.push(ui::SetupStep {
            description: "wake launchd's job, so it hands over 80/443/53".to_string(),
            note: Some(
                "the LaunchDaemon is installed already; its job is the part that is not running"
                    .to_string(),
            ),
            commands: launchd::wake_commands(),
        });
    } else if launchd_pending {
        match prepare_launchd() {
            Ok((source, commands)) => {
                launchd_step = Some(steps.len());
                steps.push(ui::SetupStep {
                    description: "let launchd hold 80/443/53 (the daemon itself stays non-root)"
                        .to_string(),
                    note: Some(format!("generated plist: {}", source.display())),
                    commands,
                });
            }
            Err(err) => ui::error(&format!("cannot write the plist: {err}"), None),
        }
    }

    // Whether the launchd step is the installation, as opposed to the wake
    // — which restarts the daemon itself, and so leaves nothing owed.
    let installs_launchd = launchd_step.is_some() && !wakes_launchd;

    // Installing launchd moves DNS to :53. A resolver still naming the
    // old port would stop resolving the moment it lands.
    let effective_dns_port = if launchd_pending {
        launchd::Ports::default().dns
    } else {
        dns_port.unwrap_or(53)
    };

    let mut resolver_step = None;

    if pending("resolver") || launchd_pending {
        resolver_step = Some(steps.len());
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

    // `--json` is what an agent reads, and it is a plan rather than a
    // report: nothing is run behind it, for the same reason nothing is run
    // without a terminal.
    if cli.json {
        output::print_json(&serde_json::json!({ "steps": steps }));
        return Ok(ExitCode::SUCCESS);
    }

    // Only the LaunchDaemon leaves anything behind to take back out, and
    // only if there is a step to install it: the plist is generated first,
    // and a failure there leaves nothing to undo.
    let undo = if launchd_step.is_some() {
        launchd::uninstall_commands()
    } else {
        Vec::new()
    };

    if steps.is_empty() {
        ui::setup(&steps, &undo, false);
        return Ok(ExitCode::SUCCESS);
    }

    if dry_run || !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        ui::setup(&steps, &undo, installs_launchd);

        // Without this, being handed a list of commands after asking for
        // `--yes` reads as the flag having been ignored.
        if !dry_run {
            ui::notice(vec![ui::note(
                "there is no terminal to ask at, so nothing was run",
            )]);
        }

        return Ok(ExitCode::SUCCESS);
    }

    ui::setup_plan(&steps);

    let mut outcomes: Vec<ui::SetupOutcome> = Vec::new();
    let total = steps.len();

    // Printing the steps, every one was going to be run; walking through
    // them, the answer to the first decides what the second should say. The
    // resolver step names the port DNS will be on *after* launchd lands, so
    // if launchd does not land it has to name the port DNS is on now —
    // otherwise saying no to one step quietly breaks resolution through the
    // next.
    //
    // **What matters is launchd holding :53, not the plist being on disk.**
    // An installed LaunchDaemon whose job stays asleep leaves DNS exactly
    // where it was.
    let mut launchd_landed = false;

    for (index, step) in steps.iter_mut().enumerate() {
        if Some(index) == resolver_step && launchd_pending && !launchd_landed {
            let port = dns_port.unwrap_or(53);
            step.commands = vec![system::resolver_command(DEFAULT_DOMAIN_SUFFIX, port)];
            step.note = Some(if wakes_launchd {
                format!("launchd's job is not awake, so DNS stays on :{port}")
            } else {
                format!("launchd was not installed, so DNS stays on :{port}")
            });
        }

        // The commands go on the screen first, every time. Agreeing to a
        // description is not agreeing to what it runs as root.
        ui::setup_step(index + 1, total, step);

        if !yes && !confirm("run this?")? {
            outcomes.push(ui::SetupOutcome::Skipped);
            ui::setup_outcome(ui::SetupOutcome::Skipped);
            continue;
        }

        // `all` stops at the first failure: these are pipelines whose later
        // halves assume the earlier ones landed.
        let ran = step
            .commands
            .iter()
            .all(|command| run_shell(command, cli.json));

        let outcome = if ran {
            ui::SetupOutcome::Ran
        } else {
            ui::SetupOutcome::Failed
        };

        if Some(index) == launchd_step && outcome == ui::SetupOutcome::Ran {
            launchd_landed = true;
        }

        outcomes.push(outcome);
        ui::setup_outcome(outcome);
    }

    // The undo is worth printing only if there is a LaunchDaemon to take
    // back out — one this run installed, or one that was there before it and
    // only needed waking.
    let undo = if launchd_landed || wakes_launchd {
        undo
    } else {
        Vec::new()
    };

    ui::setup_done(&steps, &outcomes, &undo, launchd_landed && installs_launchd);

    // A step that was declined is an answer. One that failed is not: sudo
    // said no, or a command did, and the machine is not set up.
    Ok(if outcomes.contains(&ui::SetupOutcome::Failed) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
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
            let (mut connection, start) = client.connect_or_spawn().await?;
            warn_if_unprivileged(cli, start);
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
    use clap::error::ErrorKind;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    /// Parses a `logs` command line and says whether it would attach.
    fn would_attach(args: &[&str], terminal: bool) -> bool {
        let cli = Cli::try_parse_from(args).expect("parses");
        let Command::Logs {
            services,
            follow,
            no_input,
            ..
        } = &cli.command
        else {
            panic!("not a logs command");
        };

        wants_to_type(&cli, services, *follow, *no_input, terminal)
    }

    #[test]
    fn following_one_service_from_a_terminal_offers_it() {
        assert!(would_attach(&["minato", "logs", "-f", "web"], true));
    }

    #[test]
    fn nothing_is_offered_without_a_terminal() {
        // `minato logs -f web | grep ready` and every agent invocation
        // land here. Raw mode needs a terminal on both sides, and escape
        // sequences in a pipe are noise.
        assert!(!would_attach(&["minato", "logs", "-f", "web"], false));
    }

    #[test]
    fn watching_every_service_stays_a_read() {
        // There is no one service to type at, and picking one would be a
        // guess. `minato logs -f` keeps meaning what it always meant.
        assert!(!would_attach(&["minato", "logs", "-f"], true));
        assert!(!would_attach(&["minato", "logs", "-f", "web", "api"], true));
    }

    #[test]
    fn a_finite_read_is_never_interactive() {
        // Without `-f` this prints what there is and stops. Taking the
        // terminal for that would be a session nobody asked to start.
        assert!(!would_attach(&["minato", "logs", "web"], true));
    }

    #[test]
    fn json_and_no_input_both_opt_out() {
        assert!(!would_attach(
            &["minato", "logs", "-f", "web", "--json"],
            true
        ));
        assert!(!would_attach(
            &["minato", "logs", "-f", "web", "--no-input"],
            true
        ));
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
    fn setup_takes_an_answer_up_front_and_a_way_to_run_nothing() {
        // Plain `minato setup` asks. The flags are for the two cases that
        // cannot: something that will not be there to answer, and someone
        // who only wants to read the commands.
        let cli = Cli::try_parse_from(["minato", "setup"]).expect("parses");
        assert!(matches!(
            cli.command,
            Command::Setup {
                yes: false,
                dry_run: false
            }
        ));

        for args in [
            vec!["minato", "setup", "--yes"],
            vec!["minato", "setup", "-y"],
        ] {
            let cli = Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert!(
                matches!(cli.command, Command::Setup { yes: true, .. }),
                "{args:?}"
            );
        }

        let cli = Cli::try_parse_from(["minato", "setup", "--dry-run"]).expect("parses");
        assert!(matches!(cli.command, Command::Setup { dry_run: true, .. }));
    }

    #[test]
    fn a_group_named_without_a_subcommand_is_answered_with_its_help() {
        // `minato skill` prints the group's help, and adding a global
        // flag must not turn that into a usage error: --json says
        // nothing about which subcommand was meant.
        for (args, expected) in [
            (vec!["minato", "skill", "--json"], vec!["skill"]),
            (vec!["minato", "env", "--json"], vec!["env"]),
            (
                vec!["minato", "daemon", "--workspace", "feat-1"],
                vec!["daemon"],
            ),
            (vec!["minato", "tunnel", "-w", "feat-1"], vec!["tunnel"]),
            // The root has the same shape, and the same flag on it.
            (vec!["minato", "--json"], vec![]),
        ] {
            let err = Cli::try_parse_from(&args).expect_err("no subcommand was named");
            let group = missing_subcommand(&err)
                .unwrap_or_else(|| panic!("{args:?}: {} is not answered with help", err.kind()));

            assert_eq!(group, expected, "{args:?}");

            // The name has to reach the help that gets printed.
            let mut command = Cli::command();
            command.build();
            for name in &group {
                command = command
                    .find_subcommand(name)
                    .unwrap_or_else(|| panic!("{args:?}: no `{name}` to print help for"))
                    .clone();
            }
        }
    }

    #[test]
    fn version_is_recognised_so_the_update_check_can_run() {
        // `--version` never reaches a subcommand, so clap answers it with
        // the version and stops. It is caught rather than left to exit on
        // its own, because the check that goes with it comes after.
        for args in [vec!["minato", "--version"], vec!["minato", "-V"]] {
            let err = Cli::try_parse_from(&args).expect_err("does not parse");

            assert_eq!(err.kind(), ErrorKind::DisplayVersion, "{args:?}");
            assert!(missing_subcommand(&err).is_none(), "{args:?}");
        }
    }

    #[test]
    fn every_other_parse_failure_is_left_to_clap() {
        // Help, a typo and a missing argument each say something clap
        // says better than a group's help would.
        for args in [
            vec!["minato", "--help"],
            vec!["minato", "bogus"],
            vec!["minato", "exec"],
            vec!["minato", "ls", "--nope"],
        ] {
            let err = Cli::try_parse_from(&args).expect_err("does not parse");
            assert!(
                missing_subcommand(&err).is_none(),
                "{args:?}: {}",
                err.kind()
            );
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
    fn uninstall_asks_before_it_acts() {
        // Neither flag set is the safe shape: the plan is shown and a
        // terminal is asked.
        let cli = Cli::try_parse_from(["minato", "uninstall"]).expect("parses");
        match cli.command {
            Command::Uninstall { yes, dry_run } => {
                assert!(!yes, "confirmation is not skipped by default");
                assert!(!dry_run);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn uninstall_can_be_told_to_go_ahead() {
        for args in [
            vec!["minato", "uninstall", "--yes"],
            vec!["minato", "uninstall", "-y"],
        ] {
            let cli = Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert!(matches!(cli.command, Command::Uninstall { yes: true, .. }));
        }
    }

    #[test]
    fn uninstall_can_report_without_removing() {
        let cli = Cli::try_parse_from(["minato", "uninstall", "--dry-run"]).expect("parses");
        assert!(matches!(
            cli.command,
            Command::Uninstall { dry_run: true, .. }
        ));
    }

    #[test]
    fn uninstalling_does_not_recommend_reinstalling() {
        // The binary has just been deleted, and often the cache the check
        // reads with it.
        assert!(!wants_update_notice(&Command::Uninstall {
            yes: true,
            dry_run: false
        }));

        // A dry run removed nothing, but the notice would still land
        // under a list the user is about to act on.
        assert!(!wants_update_notice(&Command::Uninstall {
            yes: false,
            dry_run: true
        }));

        // Everything else still gets it.
        assert!(wants_update_notice(&Command::Status));
    }

    #[test]
    fn the_purge_dry_run_is_not_treated_as_long_running() {
        // It is a read. Decorating it with a spinner would put a live
        // display in front of a list the user is about to be asked to
        // approve.
        assert!(!Request::Purge { dry_run: true }.is_long_running());
        assert!(Request::Purge { dry_run: false }.is_long_running());
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

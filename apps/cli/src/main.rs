//! kobune — a thin client for the daemon.
//!
//! No logic lives here. Every decision is the daemon's; the CLI builds
//! requests and prints results (`docs/DESIGN.md` §3).

mod attach;
mod compose;
mod followup;
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
use std::time::Duration;

use clap::{CommandFactory, Parser, Subcommand};
use kobune_api::{Event, Pong, Request, Response, Target, Window};
use kobune_client::{Client, ClientError, DaemonStart};

/// `0.1.0 (abc1234)`. Every nightly reports the same version, so the commit
/// is what tells one build from another.
fn version() -> &'static str {
    // Leaked once at startup: clap wants a `&'static str`, and the string is
    // built from two compile-time constants so there is nothing to free.
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| kobune_core::version_string(env!("CARGO_PKG_VERSION")))
}

/// The suffix used when `[project] domain` is left out. It is also what
/// the resolver gets installed for.
const DEFAULT_DOMAIN_SUFFIX: &str = "localhost";

#[derive(Parser, Debug)]
#[command(
    name = "kobune",
    version = version(),
    about = "A development environment manager built around git worktrees",
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
    /// Write a starter kobune.toml
    Init {
        /// Overwrite an existing kobune.toml
        #[arg(long)]
        force: bool,

        /// Convert a compose file instead of writing a starter one.
        ///
        /// Without a path, the usual names are tried. What has no
        /// equivalent here is reported rather than dropped quietly, and
        /// what compose cannot express is left as a TODO in the file.
        #[arg(long, value_name = "FILE", num_args = 0..=1)]
        from_compose: Option<Option<std::path::PathBuf>>,
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

        /// Rebuild images even when nothing Kobune can see has changed
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

        /// Rebuild images even when nothing Kobune can see has changed
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

    /// Print where services can be reached
    ///
    /// Naming one prints its URL and nothing else, for
    /// `curl "$(kobune url web)/"`. Naming none lists them all.
    Url {
        /// The service name. Every service when left out
        service: Option<String>,

        /// Draw the URL as a QR code, to open on a phone
        ///
        /// The tunnel URL when there is one: a `.localhost` name resolves
        /// on this machine and nowhere else.
        #[arg(long)]
        qr: bool,
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

    /// Take Kobune back off this machine
    ///
    /// Containers, the daemon's state, the binaries and the shell
    /// completions. **Worktrees are left alone** — they are your
    /// checkouts, and `kobune rm` is how one goes.
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
        ///
        /// The zone itself — `example.com`, not `dev.example.com`. A
        /// hostname one level below the zone is covered by the zone's
        /// Universal SSL certificate; one below that is not, and fails at
        /// the TLS handshake with the tunnel up and working.
        #[arg(long)]
        domain: Option<String>,

        /// Confirm that this goes on the public internet
        ///
        /// Kobune cannot apply a Cloudflare Access policy — that needs the
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
    /// `env` in kobune.toml belongs to that service.
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
    /// Install it at .claude/skills/kobune/SKILL.md
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
    /// Stop the daemon and start it again
    ///
    /// What a daemon left running from an older build needs: it answers
    /// happily and speaks a protocol this one does not.
    Restart,
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

    // The first run of a build the machine has not seen: what the update
    // left half-done. Before the notice below, which is about the *next*
    // build — this one is about the one in hand.
    if !cli.json && wants_followup_notice(&cli.command) {
        followup_notice().await;
    }

    // After the command, so a slow network cannot delay the output anyone is
    // waiting for. Never under `--json`: that stream is parsed, and a line
    // about a new build landing in it would be a bug rather than a nuisance.
    // stderr regardless, so `$(kobune url web)` never picks it up.
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
                    output::print_error_json(&kobune_api::ApiError::internal(err.to_string()));
                }
            } else {
                ui::error(&err.to_string(), hint_for(&err));
            }

            ExitCode::from(exit_code_for(&err) as u8)
        }
    }
}

/// The group a parse failure is about, when it is about a group that was
/// named without one of its subcommands — `["skill"]` for `kobune skill`,
/// and empty for `kobune` itself. `None` for every other failure.
///
/// Clap answers `kobune skill` with the group's help, but only because
/// nothing at all followed it: `arg_required_else_help` counts arguments,
/// and a global flag is one. So `kobune skill --json` came back as a usage
/// error instead — the same missing subcommand, a different answer,
/// decided by a flag that says nothing about which subcommand was meant.
fn missing_subcommand(err: &clap::Error) -> Option<Vec<String>> {
    use clap::error::{ContextKind, ContextValue, ErrorKind};

    if err.kind() != ErrorKind::MissingSubcommand {
        return None;
    }

    // Clap names the invocation that was left without one, the binary
    // first: `kobune skill`.
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
/// where `kobune skill` has always put it.
fn print_group_help(group: &[String], json: bool) -> ExitCode {
    // Built first, so the groups carry their full name: the usage line
    // reads `kobune skill`, not `skill`.
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
/// to pipe this — `kobune url | …`, `kobune logs | grep` — a panic on
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

/// A daemon that started where the privileged ports are not.
struct Unprivileged {
    /// What happened.
    said: String,
    /// What to do about it.
    next: String,
    /// The command that does it, where a command does.
    command: Option<String>,
}

/// What to say about a daemon that started outside launchd.
///
/// **Only when a LaunchDaemon is installed.** Without one, listening
/// elsewhere is the normal arrangement and saying so every time would be
/// noise. With one, it means launchd was meant to hand the ports over and
/// did not — the state that leaves every URL dead while `kobune up` still
/// reports success.
///
/// The three states it can be are three different answers, and the home is
/// what tells the last one apart: launchd's job may be holding those ports
/// for a `KOBUNE_HOME` that is not this daemon's, in which case nothing
/// here can move them — see [`kobune_core::launchd::Job`].
fn unprivileged_start(start: DaemonStart, home: &std::path::Path) -> Option<Unprivileged> {
    // Asked only where the answer can matter: this runs `launchctl print`,
    // and most commands on the machine start a daemon one way or another.
    if start != DaemonStart::Direct {
        return None;
    }

    unprivileged(kobune_core::launchd::job(home))
}

/// The same, for a state that has already been read.
///
/// Split out so each state's wording can be tested on a machine with no
/// LaunchDaemon on it, which is every machine that runs the tests.
fn unprivileged(job: kobune_core::launchd::Job) -> Option<Unprivileged> {
    use kobune_core::launchd::Job;

    let said = "started a daemon outside launchd, so 80 and 443 are out and no URL will answer"
        .to_string();

    match job {
        // No plist. Listening elsewhere is the arrangement, not a fault.
        Job::Missing => None,

        // Reaching :80 is what has just failed — `wake_launchd` runs
        // before the fall-back to a direct start — so naming
        // `kobune daemon restart` here would hand back the step that was
        // taken. Forcing the job is what is left, and it needs root.
        Job::Registered => Some(Unprivileged {
            said,
            next: "reaching :80 did not wake launchd's job. Force it with".to_string(),
            command: Some(kobune_core::launchd::kickstart_command()),
        }),

        // `kickstart` has no service to name here and comes back
        // `Could not find service`. What is missing is the installation.
        Job::Unregistered => Some(Unprivileged {
            said,
            next: "the plist is on disk but launchd does not have the job. Register it with"
                .to_string(),
            command: Some("kobune setup".to_string()),
        }),

        // Nothing to run. The job holds 80, 443 and 53 for another home,
        // `kobune setup` would ask launchd to bootstrap a label it already
        // has, and a `kickstart` starts that same job again.
        Job::Elsewhere(other) => Some(Unprivileged {
            said,
            next: format!(
                "launchd's job serves KOBUNE_HOME={}, so those ports are held for a daemon \
                 that is not this one. Point KOBUNE_HOME there to reach it, or keep the \
                 ports this daemon fell back to",
                other.display()
            ),
            command: None,
        }),
    }
}

/// Says so when the daemon that just started cannot hold 80 or 443.
///
/// A notice rather than a failure: the command that was asked for did
/// happen. [`start_daemon`] is the one place where this state *is* the
/// failure, because starting the daemon was the whole request.
fn warn_if_unprivileged(cli: &Cli, client: &Client, start: DaemonStart) {
    if cli.json {
        return;
    }

    let Some(unprivileged) = unprivileged_start(start, client.home()) else {
        return;
    };

    ui::notice(vec![
        ui::note(&unprivileged.said),
        match &unprivileged.command {
            Some(command) => ui::hint(&unprivileged.next, command),
            None => ui::note(&unprivileged.next),
        },
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
    let paths = kobune_core::Paths::resolve().ok()?;
    update::background_notice(&paths).await
}

/// The check `--version` makes, which asks every time rather than once a
/// day.
///
/// Silent on the same failures, including having no configuration directory
/// to leave the answer in.
async fn version_update_notice() -> Option<String> {
    let paths = kobune_core::Paths::resolve().ok()?;
    update::version_notice(&paths).await
}

/// What either check has to say, in the one wording both use.
///
/// On stderr, through [`ui::notice`]: `$(kobune url web)` must never pick
/// it up, and neither must anything parsing `--json`.
fn print_update_notice(commit: &str) {
    ui::notice(vec![ui::hint(
        &format!("a newer build is available ({commit}). Install it with"),
        "kobune update",
    )]);
}

/// Whether a command should carry the follow-up notice.
///
/// `update` has already printed the steps it could be sure of, and a
/// second list under the panel would read as a different one. `completions`
/// is redirected into a file and `uninstall` has just taken Kobune off the
/// machine.
///
/// **`daemon` is out for a reason of its own**: it is the command the steps
/// send people to, and `stop` returns as soon as the request is written,
/// leaving the socket up for a moment longer. Asking then would report the
/// daemon somebody has this second stopped as still running — and because
/// the notice is not printed, it is not remembered either, so it lands
/// intact on the next command.
fn wants_followup_notice(command: &Command) -> bool {
    !matches!(
        command,
        Command::Update { .. }
            | Command::Completions { .. }
            | Command::Uninstall { .. }
            | Command::Daemon { .. }
    )
}

/// What an update left to do, on the first run of the build it installed.
///
/// Empty on every other run, which is nearly all of them: the record only
/// disagrees once per build, and everything after it — including the
/// connection to the daemon socket — happens on the far side of that.
///
/// The build is remembered only once the steps have been printed, so a
/// command interrupted in between finds them again rather than having
/// marked itself as read.
async fn followup_notice() {
    let Ok(paths) = kobune_core::Paths::resolve() else {
        return;
    };

    if !followup::is_new_build(&paths) {
        return;
    }

    let daemon = match Client::from_env() {
        Ok(client) => followup::daemon(&client, version()).await,
        // No socket to ask, so nothing is claimed about what is on it.
        Err(_) => followup::Daemon::Stopped,
    };

    print_followup(&followup::steps(daemon, skill_root().as_deref()));

    followup::remember(&paths);
}

/// Where the Skill for the current directory lives, for the step that is
/// about it. The same answer `kobune skill install` would act on.
fn skill_root() -> Option<PathBuf> {
    std::env::current_dir()
        .ok()
        .map(|cwd| skill::root_for(&cwd))
}

/// The steps, under a line saying what they are doing there.
///
/// Without it a `kobune status` in the morning grows a remark about the
/// daemon out of nowhere. With it, the build that changed is named — which
/// is also the answer to "since when?".
fn print_followup(steps: &[followup::Step]) {
    if steps.is_empty() {
        return;
    }

    let mut lines = vec![ui::note(&format!(
        "kobune changed to {} since the last run",
        kobune_core::BUILD_COMMIT_SHORT
    ))];

    lines.extend(
        steps
            .iter()
            .map(|step| ui::step(&step.reason, &step.command)),
    );

    ui::notice(lines);
}

/// The errors the CLI deals with. Ones from the daemon keep its exit
/// code.
#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error(transparent)]
    Client(#[from] ClientError),

    #[error("{0}")]
    Local(String),

    /// The daemon started, but not where the ports are.
    ///
    /// Its own kind because it is the one failure that leaves something
    /// running: the message says what happened, and the hint is what to do
    /// about the state the machine is actually in — which of the three it
    /// is, [`unprivileged`] decides.
    #[error("{message}")]
    Unprivileged { message: String, hint: String },

    /// The stop did not take, so the restart met the daemon it meant to
    /// replace.
    ///
    /// Its own kind for the same reason as [`Self::Unprivileged`]: it is a
    /// failure that leaves something running, and what to do about the
    /// state the machine is in is not what the message says happened.
    #[error("{message}")]
    DidNotStop { message: String, hint: String },
}

fn as_api_error(err: &CliError) -> Option<&kobune_api::ApiError> {
    match err {
        CliError::Client(ClientError::Api(api)) => Some(api),
        _ => None,
    }
}

fn hint_for(err: &CliError) -> Option<&str> {
    match err {
        CliError::Client(client) => client.hint(),
        CliError::Local(_) => None,
        CliError::Unprivileged { hint, .. } | CliError::DidNotStop { hint, .. } => Some(hint),
    }
}

fn exit_code_for(err: &CliError) -> i32 {
    match err {
        CliError::Client(client) => client.exit_code(),
        CliError::Local(_) | CliError::Unprivileged { .. } | CliError::DidNotStop { .. } => 1,
    }
}

async fn run(cli: &Cli) -> Result<ExitCode, CliError> {
    let cwd = std::env::current_dir()
        .map_err(|err| CliError::Local(format!("cannot read the working directory: {err}")))?;

    // `init` needs no daemon.
    if let Command::Init {
        force,
        from_compose,
    } = &cli.command
    {
        let outcome = match from_compose {
            // `--from-compose` alone means "find it yourself"; with a
            // path, that one.
            Some(named) => init::from_compose(&cwd, named.as_deref(), *force),
            None => init::run(&cwd, *force),
        }
        .map_err(|err| CliError::Local(err.to_string()))?;

        if cli.json {
            output::print_json(&serde_json::json!({
                "path": outcome.path,
                "project": outcome.project,
                "from": outcome.from,
                "carried": outcome.carried,
                "dropped": outcome
                    .dropped
                    .iter()
                    .map(|(service, key)| serde_json::json!({ "service": service, "key": key }))
                    .collect::<Vec<_>>(),
            }));
        } else {
            let mut fields = vec![
                ("created", outcome.path.display().to_string()),
                ("project", outcome.project.clone()),
            ];

            if let Some(from) = &outcome.from {
                fields.push(("converted from", from.display().to_string()));
            }

            let next = if outcome.from.is_some() {
                vec![ui::note(
                    "read the TODOs in it before the first `kobune up`",
                )]
            } else {
                vec![ui::hint("bring the environment up with", "kobune up")]
            };

            ui::done("init", &fields, next);

            if outcome.from.is_some() {
                report_conversion(&outcome);
            }
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
    warn_if_unprivileged(cli, &client, start);

    // An interactive `logs` runs its own way: the terminal goes into raw
    // mode, and what comes back is not lines but a screen.
    if matches!(
        &request,
        Request::Logs {
            attach: Some(_),
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
    // to be able to judge `kobune exec web -- pnpm test` by exit status
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
    connection: &mut kobune_client::Connection,
    request: Request,
) -> Result<ExitCode, CliError> {
    let (typed, keys) = tokio::sync::mpsc::unbounded_channel();

    // **Taken, not cloned.** The last sender going is how the client is
    // told the person has detached, so this one has to be handed to the
    // pump rather than kept alive here beside it.
    let mut typed = Some(typed);
    let mut session = None;
    let mut screen = attach::Screen::new();

    // **Whether the terminal was handed over, not whether taking it
    // worked.** The daemon starts the service's terminal talking the
    // moment it says this, and what it sends first is what the program
    // made of its own — the alternate screen, the mouse. Those arrive
    // even when raw mode could not be entered, so what has to be put back
    // afterwards is decided here rather than by whether a session started.
    let mut attached = false;

    let outcome = connection
        .call_attached(
            request,
            |event| match event {
                Event::Attached { service } => {
                    attach::announce(&service);
                    attached = true;

                    match typed.take().map(attach::Session::start) {
                        Some(Ok(started)) => session = Some(started),
                        Some(Err(err)) => eprintln!("error: cannot take the terminal: {err}"),
                        None => {}
                    }
                }
                Event::Bytes { data } => screen.show(&kobune_api::decode_bytes(&data)),
                other => output::print_output_event(&other),
            },
            keys,
        )
        .await;

    // **Before anything else is printed.** Raw mode is still on until the
    // session is dropped, and a message written under it comes out as a
    // staircase.
    drop(session);

    if attached {
        screen.restore();
    }

    // **Detaching says nothing**, because whoever pressed the keys knows
    // what they did. The terminal closing on its own does: the last frame
    // is still on screen, and without a word it reads as the session
    // having frozen rather than the service having gone.
    if attached && matches!(outcome, Ok(kobune_client::Attached::Finished(_))) {
        eprintln!("the service's terminal closed. `kobune status` says why");
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
/// has read `kobune.toml`, and it answers by attaching or by explaining
/// why it did not.
fn terminal_to_offer(
    cli: &Cli,
    services: &[String],
    follow: bool,
    no_input: bool,
    terminal: Option<Window>,
) -> Option<Window> {
    if !follow || no_input || cli.json || services.len() != 1 {
        return None;
    }

    terminal
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
            attach: terminal_to_offer(cli, services, *follow, *no_input, attach::offered_window()),
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
                    "commit": kobune_core::BUILD_COMMIT,
                }));
            } else {
                ui::done(
                    "update",
                    &[("up to date", kobune_core::BUILD_COMMIT_SHORT.to_string())],
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
                    "commit": kobune_core::BUILD_COMMIT,
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
                    "running": kobune_core::BUILD_COMMIT,
                }));
            } else {
                ui::done(
                    "update",
                    &[
                        ("available", short_commit(&commit)),
                        ("running", kobune_core::BUILD_COMMIT_SHORT.to_string()),
                    ],
                    vec![ui::hint("install it with", "kobune update")],
                );
            }
        }
        Updated::Installed(commit) => {
            // The binaries are in place; the machine around them is a
            // step behind. What that leaves to do is worked out rather
            // than stated, so a machine with no daemon running is not
            // told to restart one.
            let steps = followup::steps_after_replacing(daemon_after_update().await);

            if cli.json {
                output::print_json(&serde_json::json!({
                    "status": "installed",
                    "commit": commit,
                    "next": steps,
                }));
            } else {
                ui::done(
                    "update",
                    &[("installed", short_commit(&commit))],
                    steps
                        .iter()
                        .map(|step| ui::step(&step.reason, &step.command))
                        .collect(),
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
                progress.begin(INSTALLING, "installing kobune and kobuned");
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

/// Whether a daemon survived the swap, which is the one thing the build
/// being replaced can still answer for.
async fn daemon_after_update() -> followup::Daemon {
    match Client::from_env() {
        Ok(client) => followup::daemon_after_replacing(&client).await,
        Err(_) => followup::Daemon::Stopped,
    }
}

/// A commit at the length that tells a reader something.
fn short_commit(commit: &str) -> String {
    commit.chars().take(7).collect()
}

/// Takes Kobune off the machine.
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
    let daemon: Result<kobune_api::PurgeReport, String> = match &mut connection {
        None => Err("it is not running, so it has nothing to take down".to_string()),
        Some(connection) => match connection.request(Request::Purge { dry_run: true }).await {
            Ok(Response::Purge(report)) => Ok(report),
            Ok(other) => Err(format!("it answered with {other:?} instead of a report")),
            Err(err) => Err(err.to_string()),
        },
    };

    // The CA lives under the daemon's root whether or not it answers, so
    // the keychain step does not depend on reaching it.
    let ca_path = kobune_core::Paths::resolve()
        .ok()
        .map(|paths| paths.ca_dir().join("kobune-ca.crt"));

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

    // What the daemon could not take after all. The plan above is what it
    // *expected* to be able to take, so a volume that turned out to be
    // held by something, or a runtime that stopped answering between the
    // two calls, is only known from the answer to the second one.
    let mut left_behind: Vec<String> = Vec::new();

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

        match outcome {
            Ok(Response::Purge(done)) => {
                left_behind.extend(
                    done.storage_left
                        .iter()
                        .map(|failure| format!("{}: {}", failure.what, failure.reason)),
                );
            }
            Ok(_) => {}
            Err(err) => ui::error(&format!("cannot take the containers down: {err}"), None),
        }
    }

    // Order matters here, and it is not the obvious one.
    //
    // The privileged steps come before anything is deleted, because two of
    // them depend on the state that deleting would destroy.
    //
    // `security remove-trusted-cert` names a *file*. Remove `~/.kobune`
    // first and there is no certificate left to point at, the command
    // fails, and the CA stays trusted for good — which is the one thing an
    // uninstall really has to get right, since a trusted CA goes on
    // trusting anything signed by it.
    //
    // Booting the LaunchDaemon out has to come before asking the daemon to
    // stop, too. That is the whole point of launchd: while launchd *has*
    // the job it holds 80, 443 and 53, so anything reaching one of them
    // demand-launches the daemon again — and a daemon that starts recreates
    // the state directory, a moment before it was to be deleted.
    //
    // Has the job, not has a plist: a file that was never bootstrapped
    // holds nothing (`kobune_core::launchd::is_loaded`). The ordering
    // costs nothing there and is required wherever it was, so it is not
    // conditional.
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

    // One list, because they are one answer to one question: what is still
    // on this machine. A volume nothing would let go of is exactly as much
    // "could not remove" as a file this user does not own.
    let mut failures = removed.failures;
    failures.extend(left_behind);

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
/// same reason `kobune setup` only walks through its steps on a terminal —
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
/// tee …` — and they are the same strings `kobune setup` prints, so what
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

/// What a conversion could not carry across.
///
/// **Printed every time, even when there is nothing.** "Nothing was
/// dropped" is a different sentence from silence, and only one of them
/// can be trusted.
fn report_conversion(outcome: &init::InitOutcome) {
    if !outcome.carried.is_empty() {
        ui::note_lines(
            "compose's `env_file` became `carry`",
            &[format!(
                "{} — copied into each new worktree. Kobune's own `env_file` \
                 writes rather than reads, so mapping it across would have \
                 overwritten these",
                outcome.carried.join(", ")
            )],
        );
    }

    if outcome.dropped.is_empty() {
        ui::note_lines(
            "nothing was dropped",
            &["every key had an equivalent".to_string()],
        );
        return;
    }

    let mut by_service: std::collections::BTreeMap<&str, Vec<&str>> =
        std::collections::BTreeMap::new();

    for (service, key) in &outcome.dropped {
        by_service
            .entry(service.as_str())
            .or_default()
            .push(key.as_str());
    }

    let lines: Vec<String> = by_service
        .into_iter()
        .map(|(service, keys)| format!("{service}: {}", keys.join(", ")))
        .collect();

    ui::note_lines("no equivalent here, so left out", &lines);
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
            let root = skill::root_for(cwd);

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
    let parse_scope = |raw: &str| -> Result<kobune_api::EnvScope, CliError> {
        raw.parse::<kobune_api::EnvScope>().map_err(CliError::Local)
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
    if let Command::Url { service, qr } = &cli.command {
        return present_url(cli, response, service.as_deref(), *qr);
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
                    "kobune down && kobune up",
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

/// What `kobune url` prints.
///
/// **Naming a service and naming none are different questions.** One is
/// "give me the address", asked from inside `$(…)` and answered with a
/// bare line. The other is "what is there", which the first reachable
/// service used to answer on behalf of all of them — and answering *which
/// URL* with one of several is how a request ends up at the wrong service.
fn present_url(
    cli: &Cli,
    response: &Response,
    service: Option<&str>,
    qr: bool,
) -> Result<ExitCode, CliError> {
    let Response::Workspace { workspace } = response else {
        return Err(CliError::Local("cannot read the workspace".to_string()));
    };

    let Some(name) = service else {
        return present_urls(cli, workspace, qr);
    };

    let target = workspace.service(name).ok_or_else(|| {
        let available: Vec<&str> = workspace.services.iter().map(|s| s.name.as_str()).collect();
        CliError::Local(format!(
            "no service named `{name}`. Available: {}",
            available.join(", ")
        ))
    })?;

    let access = target.access().ok_or_else(|| {
        CliError::Local(format!(
            "service `{}` is not reachable yet (state: {})",
            target.name,
            target.state.label()
        ))
    })?;

    if cli.json {
        output::print_json(&url_json(target));
    } else if qr {
        ui::url(target);
    } else {
        // One undecorated line, ready to pipe.
        ui::value(&access);
    }

    Ok(ExitCode::SUCCESS)
}

/// The listing, when no service was named.
///
/// Every service, including the ones with nowhere to go: a listing that
/// silently leaves those out reads as a workspace that does not define
/// them. Nothing reachable at all is still an error, so a script that
/// waits on `kobune url` keeps its signal.
fn present_urls(
    cli: &Cli,
    workspace: &kobune_api::WorkspaceInfo,
    qr: bool,
) -> Result<ExitCode, CliError> {
    if !workspace.services.iter().any(|s| s.access().is_some()) {
        return Err(CliError::Local(
            "no service is reachable. Start one with `kobune up`".to_string(),
        ));
    }

    if cli.json {
        let urls: Vec<serde_json::Value> = workspace.services.iter().map(url_json).collect();
        output::print_json(&urls);
    } else {
        ui::urls(workspace, qr);
    }

    Ok(ExitCode::SUCCESS)
}

/// One service, as `--json` has it.
///
/// `url` is absent rather than null when there is nowhere to go, which is
/// the shape every optional field in the API already has.
fn url_json(service: &kobune_api::ServiceInfo) -> serde_json::Value {
    let mut fields = serde_json::Map::new();
    fields.insert("service".into(), service.name.clone().into());
    fields.insert("state".into(), serde_json::json!(service.state));

    if let Some(url) = service.access() {
        fields.insert("url".into(), url.into());
    }
    if let Some(url) = &service.tunnel_url {
        fields.insert("tunnel_url".into(), url.clone().into());
    }

    serde_json::Value::Object(fields)
}

/// What `kobune env get` prints: the value, on one line.
fn present_env_value(cli: &Cli, response: &Response, key: &str) -> Result<ExitCode, CliError> {
    let Response::Env {
        entries, service, ..
    } = response
    else {
        return Err(CliError::Local("cannot read the environment".to_string()));
    };

    // Whichever listing this came from is the one to send someone back to.
    let listing = match service {
        Some(name) => format!("kobune env ls --service {name}"),
        None => "kobune env ls".to_string(),
    };

    let entry = entries
        .iter()
        .find(|entry| entry.key == key)
        .ok_or_else(|| {
            CliError::Local(format!(
                "`{key}` is not defined. Run `kobune env ls` to see what is, \
                 or `kobune env ls --service <name>` for one service's own"
            ))
        })?;

    // **This one prints the real value, for a script to use.** A value
    // that did not settle is shown as written, and handing one over as
    // though it had would put `${...}` into whatever read it. Refused as
    // a configuration problem, so a script sees the same exit code it
    // would have got from `kobune up`.
    if let Some(unsettled) = &entry.unsettled {
        let mut err = kobune_api::ApiError::new(
            kobune_api::ErrorCode::InvalidConfig,
            format!("`{key}` {}", ui::unsettled_reason(unsettled)),
        );

        err = match ui::unsettled_remedy(unsettled) {
            Some(remedy) => err.with_hint(remedy),
            None => err.with_hint(format!("`{listing}` shows the rest")),
        };

        return Err(CliError::Client(ClientError::Api(err)));
    }

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

    let combined = kobune_api::Diagnostics::new(all);

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
    diagnostics: &kobune_api::Diagnostics,
    dns_port: Option<u16>,
    ca_path: Option<&std::path::Path>,
    yes: bool,
    dry_run: bool,
) -> Result<ExitCode, CliError> {
    let pending = |id: &str| {
        diagnostics
            .checks
            .iter()
            .any(|check| check.id == id && check.status != kobune_api::CheckStatus::Ok)
    };

    let mut steps: Vec<ui::SetupStep> = Vec::new();
    let launchd_pending = pending("launchd");

    // Privileged ports being unavailable has more than one cause, and they
    // take different steps — see [`kobune_core::launchd::Job`]. Asked only
    // where the answer can matter: with the sockets already handed over
    // there is nothing to install and nothing to wake.
    //
    // The home is this CLI's, the same one [`prepare_launchd`] writes into
    // the plist. Where launchd's job serves another, it holds 80, 443 and
    // 53 for a daemon that is not the one being set up, and neither step
    // below is the answer.
    let job = launchd_pending.then(|| {
        kobune_core::Paths::resolve()
            .map(|paths| kobune_core::launchd::job(paths.root()))
            .unwrap_or(kobune_core::launchd::Job::Missing)
    });

    let elsewhere = match &job {
        Some(kobune_core::launchd::Job::Elsewhere(home)) => Some(home.clone()),
        _ => None,
    };

    let wakes_launchd = job == Some(kobune_core::launchd::Job::Registered);
    let mut launchd_step = None;

    if let Some(home) = &elsewhere {
        // Nothing is offered, because nothing here helps: `setup` would
        // ask launchd to bootstrap a label it already has, and a kickstart
        // would start the job serving that other home again.
        ui::notice(vec![
            ui::note(&format!(
                "launchd's job serves KOBUNE_HOME={}, so 80, 443 and 53 are held for a \
                 daemon that is not this one",
                home.display()
            )),
            ui::note(
                "point KOBUNE_HOME there to reach it, or leave this home on the ports it \
                 fell back to",
            ),
        ]);
    } else if wakes_launchd {
        launchd_step = Some(steps.len());
        steps.push(ui::SetupStep {
            description: "wake launchd's job, so it hands over 80/443/53".to_string(),
            // The escalation is a note rather than a second command. As a
            // command it would ask for a password on every run, including
            // the ones where restarting already did it — and the two ran
            // with `all`, so declining that prompt used to leave the
            // machine with no daemon at all.
            note: Some(format!(
                "the LaunchDaemon is installed already; its job is the part \
                 that is not running. If it stays inactive afterwards, \
                 `{}` forces it",
                kobune_core::launchd::kickstart_command()
            )),
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
    //
    // **The step, not the state.** With the job serving another home — or
    // with no plist that could be written — nothing here will ever take
    // :53, and a plan naming it is what `--json` and a run with no terminal
    // print to be followed by hand.
    let effective_dns_port = if launchd_step.is_some() {
        launchd::Ports::default().dns
    } else {
        dns_port.unwrap_or(53)
    };

    let mut resolver_step = None;

    if pending("resolver") || launchd_pending {
        resolver_step = Some(steps.len());
        steps.push(ui::SetupStep {
            description: format!("point *.{DEFAULT_DOMAIN_SUFFIX} at Kobune's DNS"),
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
            step.note = Some(match (&elsewhere, wakes_launchd) {
                (Some(home), _) => format!(
                    "launchd's job serves {}, so DNS here stays on :{port}",
                    home.display()
                ),
                (None, true) => format!("launchd's job is not awake, so DNS stays on :{port}"),
                (None, false) => format!("launchd was not installed, so DNS stays on :{port}"),
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

        if Some(index) == launchd_step {
            // **The step's own exit status says.** `kobune daemon start`
            // fails where it could not go through launchd, and the
            // installation's commands are `bootstrap`, whose status has
            // always said — so a step that ran is a step that landed.
            //
            // It did not always: the wake exited 0 whether the job came up
            // or a daemon started directly in its place, and believing it
            // there wrote `/etc/resolver/localhost` for :53 while DNS was
            // still on the fallback port.
            launchd_landed = outcome == ui::SetupOutcome::Ran;

            if wakes_launchd && outcome == ui::SetupOutcome::Failed {
                // What is left to run is the escalation, not the command
                // that has just been run to no effect. The summary prints
                // a step's commands under "still to run", so this is what
                // it has to be carrying by then; the command itself has
                // already said why it failed.
                step.commands = vec![kobune_core::launchd::kickstart_command()];

                ui::error(
                    "the resolver step below names the port DNS is on now, so run \
                     `kobune setup` again once the job is up",
                    None,
                );
            }
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
fn find_port(diagnostics: &kobune_api::Diagnostics, id: &str) -> Option<u16> {
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

fn find_detail<'a>(diagnostics: &'a kobune_api::Diagnostics, id: &str) -> Option<&'a str> {
    let check = diagnostics.checks.iter().find(|check| check.id == id)?;
    if check.detail.starts_with('/') {
        Some(&check.detail)
    } else {
        None
    }
}

/// Writes the launchd plist and returns the commands to install it.
fn prepare_launchd() -> anyhow::Result<(PathBuf, Vec<String>)> {
    let paths = kobune_core::Paths::resolve()?;

    // The CLI and the daemon ship together, so it is next door.
    let program = std::env::current_exe()?
        .parent()
        .map(|dir| dir.join("kobuned"))
        .unwrap_or_else(|| PathBuf::from("kobuned"));

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
        DaemonCommand::Start => start_daemon(cli, client).await,
        DaemonCommand::Stop => {
            let was_running = stop_daemon(client).await?;

            if !cli.json {
                if was_running {
                    ui::confirm("stopped the daemon");
                } else {
                    ui::confirm("the daemon is not running");
                }
            }

            Ok(ExitCode::SUCCESS)
        }
        DaemonCommand::Restart => restart_daemon(cli, client).await,
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

/// Starts the daemon and says what answered.
async fn start_daemon(cli: &Cli, client: &Client) -> Result<ExitCode, CliError> {
    let (_, pong) = start_or_meet_daemon(client).await?;
    report_daemon(cli, client, &pong);

    Ok(ExitCode::SUCCESS)
}

/// Starts a daemon, or meets the one already answering, and says which.
///
/// **A start that could not go through launchd is a failed start.** The
/// daemon is running, and holds none of the ports it was installed to hold,
/// so no URL answers — and the exit code is the only part of this that
/// `kobune setup`'s wake step, an agent, or a script reads. Saying 0 there
/// is how a `setup` run came to write `/etc/resolver/localhost` for a port
/// nothing was listening on.
///
/// Only [`start_daemon`] and [`restart_daemon`] come through here. Every
/// other command carries on and prints the notice: what they were asked to
/// do did happen.
///
/// Split out because starting is not the only thing a caller can want to
/// know: [`restart_daemon`] has to judge what answered before it can call
/// the result a restart, and it cannot judge an [`ExitCode`]. Nothing is
/// printed here for the same reason — a round that turns out not to have
/// restarted anything must not have announced a daemon on the way past.
async fn start_or_meet_daemon(client: &Client) -> Result<(DaemonStart, Pong), CliError> {
    let (mut connection, start) = client.connect_or_spawn().await?;

    if let Some(unprivileged) = unprivileged_start(start, client.home()) {
        return Err(CliError::Unprivileged {
            message: unprivileged.said,
            hint: match unprivileged.command {
                Some(command) => format!("{} {command}", unprivileged.next),
                None => unprivileged.next,
            },
        });
    }

    let pong = connection.handshake().await?;

    Ok((start, pong))
}

/// Prints the daemon that answered, in whichever form was asked for.
fn report_daemon(cli: &Cli, client: &Client, pong: &Pong) {
    if cli.json {
        output::print_json(pong);
    } else {
        ui::daemon(pong, Some(client.socket_path()));
    }
}

/// Asks the daemon to stop. `false` when there was none.
///
/// **Nothing here shakes hands.** The reason to stop a daemon is most
/// often that it is too old to talk to, and a version check on the way in
/// would refuse to do the one thing that fixes it.
async fn stop_daemon(client: &Client) -> Result<bool, CliError> {
    let Ok(mut connection) = client.connect().await else {
        return Ok(false);
    };

    connection.request(Request::Shutdown).await?;
    Ok(true)
}

/// Stops the daemon and starts one in its place.
///
/// **The stop's own failure is not a reason to abandon this.** The request
/// has already gone by then, and what fails on the way back is the daemon
/// going away mid-reply — or an old one whose reply this build cannot
/// decode, which is the very thing being restarted. Returning there would
/// leave the machine with no daemon at all, having run the harmful half of
/// a fix that every other part of the CLI now recommends.
///
/// A second round exists because starting shakes hands with whatever
/// answers ([`kobune_client::Client::connect_or_spawn`]), so a daemon that
/// outlived the wait is met, not replaced — and it is met exactly when it
/// is too old to talk to, which is when someone is most likely to be
/// restarting. The second round stops it again rather than only waiting
/// longer: what answers may be a daemon this run started, which nobody has
/// asked to stop, and no amount of waiting moves that one.
///
/// A daemon of *this* build outliving the wait is the same failure with
/// none of the noise: it shakes hands perfectly well, so the round above
/// never runs and the command reports a restart that did not happen. What
/// separates it from the daemon that legitimately answers here — launchd's
/// job, woken by a request arriving in the gap — is [`Outgoing`], read
/// before the stop.
async fn restart_daemon(cli: &Cli, client: &Client) -> Result<ExitCode, CliError> {
    const ROUNDS: usize = 2;

    let mut met = None;

    for round in 1..=ROUNDS {
        let last = round == ROUNDS;

        // **Before the stop**, because this is the only moment the daemon
        // being replaced can still be asked anything.
        let outgoing = outgoing_daemon(client).await;

        // Started the instant the reading was taken, and never before it:
        // the comparison afterwards holds the round's own seconds against
        // the daemon's, and a clock running from earlier would credit the
        // outgoing daemon with time it had not had yet.
        let since = std::time::Instant::now();

        let _ = stop_daemon(client).await;

        // **Waited for.** The next start binds the same socket, and a
        // daemon on its way out still holds it.
        wait_until_stopped(client).await;

        match start_or_meet_daemon(client).await {
            // The daemon the stop was for, still there. Nothing was
            // restarted, so nothing is printed and nothing is reported.
            Ok((DaemonStart::Existing, pong)) if outgoing.outlasted(&pong, since.elapsed()) => {
                met = Some(pong);
            }
            Ok((_, pong)) => {
                report_daemon(cli, client, &pong);
                return Ok(ExitCode::SUCCESS);
            }
            Err(err) if is_unspeakable(&err) && !last => {}
            Err(err) => return Err(err),
        }
    }

    let met = met.expect("the loop only falls through after meeting the outgoing daemon");

    Err(CliError::DidNotStop {
        message: format!(
            "the daemon outlasted every stop, so nothing was restarted: what is \
             answering has been up {}",
            ui::format_uptime(met.uptime_secs)
        ),
        hint: format!(
            "it may still be finishing what it was asked to do, so this is worth a \
             second try; `lsof {}` names the process holding the socket if it is not",
            client.socket_path().display()
        ),
    })
}

/// The daemon a restart means to replace, as it was on the way in.
///
/// **Read before the stop**, because the comparison afterwards is the whole
/// of what tells a daemon started in the gap from the one that would not go
/// away. Both answer a start with [`DaemonStart::Existing`], and nothing
/// else on the socket separates them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outgoing {
    /// Nothing answered.
    Absent,
    /// How long it said it had been up, in whole seconds.
    Up(u64),
    /// It answered, and this build could not read the answer.
    Mute,
}

impl Outgoing {
    /// Whether the daemon that answered a start is this one, still there.
    ///
    /// **`waited` is the round's own clock**, running from the reading
    /// above to this handshake. Without it the two uptimes are held
    /// against each other across seconds nobody counted, and a daemon
    /// three seconds old is judged to have outlasted one that was two
    /// seconds old when the round began — which is any restart of a
    /// daemon that has just come up.
    fn outlasted(self, met: &Pong, waited: Duration) -> bool {
        match self {
            // Nothing answered, so anything that started during the round
            // is a daemon this restart produced. Anything older was there
            // the whole time and was never asked to stop — the reading
            // above and the stop both failed to connect, which is rare,
            // and is exactly the state that must not report a restart.
            Self::Absent => met.uptime_secs > waited.as_secs(),

            // Still there means still counting: whatever it had on the
            // way in, it has the round on top of it by now. A daemon
            // started in its place cannot have more than the round.
            //
            // Whole seconds never fail the daemon that stayed —
            // `floor(a + b)` is never below `floor(a) + floor(b)` — and
            // where they do tie, the tie costs a round rather than a
            // restart reported wrongly.
            Self::Up(uptime) => met.uptime_secs >= uptime.saturating_add(waited.as_secs()),

            // Nothing to compare against, so having met anything at all
            // is the whole of the evidence. Another round costs a stop
            // and a wait; believing this one reports a restart that did
            // not happen, which is the failure worth avoiding.
            Self::Mute => true,
        }
    }
}

/// Asks the daemon on the socket how long it has been up.
///
/// **Nothing here shakes hands**, for the reason [`stop_daemon`] makes no
/// handshake either: the daemon most worth restarting is one this build
/// cannot talk to, and refusing to read its uptime would go blind exactly
/// there. A `Pong` that cannot be read at all is [`Outgoing::Mute`], which
/// is a state of its own rather than a missing number.
async fn outgoing_daemon(client: &Client) -> Outgoing {
    let Ok(mut connection) = client.connect().await else {
        return Outgoing::Absent;
    };

    match connection.request(Request::Ping).await {
        Ok(Response::Pong(pong)) => Outgoing::Up(pong.uptime_secs),
        _ => Outgoing::Mute,
    }
}

/// Whether the daemon that answered is one this build cannot talk to.
///
/// Not only a protocol mismatch: that is what a *decodable* answer from
/// another build looks like, and a build that changed the shape of the
/// handshake itself fails earlier, as a codec or protocol error. Losing the
/// connection reads the same way here — something was there and this build
/// could not hold a conversation with it.
fn is_unspeakable(err: &CliError) -> bool {
    matches!(
        err,
        CliError::Client(
            ClientError::VersionMismatch { .. }
                | ClientError::Protocol(_)
                | ClientError::Codec(_)
                | ClientError::Disconnected
        )
    )
}

/// Waits for the socket to stop answering, within reason.
///
/// Returning early would race the daemon's own shutdown for the socket.
///
/// Giving up quietly is right, but not because nothing goes wrong: a socket
/// that still answers is *not* cleared by `connect_or_spawn` — that only
/// happens for one nothing answers on — so a daemon still on its way out is
/// shaken hands with instead. [`restart_daemon`] is what handles that.
async fn wait_until_stopped(client: &Client) {
    const PATIENCE: Duration = Duration::from_secs(5);
    const GLANCE: Duration = Duration::from_millis(50);

    let deadline = std::time::Instant::now() + PATIENCE;
    while std::time::Instant::now() < deadline {
        if client.connect().await.is_err() {
            return;
        }

        tokio::time::sleep(GLANCE).await;
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

    #[test]
    fn a_machine_with_no_launchdaemon_is_not_told_anything() {
        // Listening on the fallback ports is the whole arrangement on
        // Linux, and on a Mac that never ran `kobune setup`. Saying it
        // every time the daemon starts would be noise.
        assert!(unprivileged(kobune_core::launchd::Job::Missing).is_none());
    }

    #[test]
    fn a_job_launchd_has_is_answered_with_the_kickstart() {
        // **Not `kobune daemon restart`.** This is only reached after
        // starting already reached for :80 and found nothing to wake, and
        // the restart does exactly that — so naming it hands back the step
        // that has just been taken.
        let said = unprivileged(kobune_core::launchd::Job::Registered).expect("says something");

        assert_eq!(
            said.command.as_deref(),
            Some(kobune_core::launchd::kickstart_command().as_str())
        );
    }

    #[test]
    fn a_plist_launchd_does_not_have_is_answered_with_setup() {
        // `kickstart` has no service to name here: launchd was never asked
        // to take the job, so what is missing is the installation.
        let said = unprivileged(kobune_core::launchd::Job::Unregistered).expect("says something");

        assert_eq!(said.command.as_deref(), Some("kobune setup"));
    }

    #[test]
    fn a_job_for_another_home_is_answered_with_no_command_at_all() {
        // Every command the other states take is wrong here, and offering
        // one anyway is how somebody ends up running `kobune setup` into
        // launchd's `Input/output error` for a label it already has.
        let said = unprivileged(kobune_core::launchd::Job::Elsewhere(PathBuf::from(
            "/Users/someone/.kobune",
        )))
        .expect("says something");

        assert!(said.command.is_none(), "{:?}", said.command);
        assert!(
            said.next.contains("/Users/someone/.kobune"),
            "name the home the ports are held for: {}",
            said.next
        );
    }

    #[test]
    fn a_start_that_could_not_reach_launchd_fails_with_its_hint() {
        // The exit code is the whole point: `kobune setup`'s wake step
        // reads it, and so does anything else driving the CLI. 1 is the
        // generic failure, and 2 belongs to clap.
        let err = CliError::Unprivileged {
            message: "started a daemon outside launchd".to_string(),
            hint: "force the job".to_string(),
        };

        assert_eq!(exit_code_for(&err), 1);
        assert_eq!(hint_for(&err), Some("force the job"));
    }

    #[test]
    fn a_daemon_that_cannot_be_talked_to_is_worth_another_round() {
        // What restarting is usually *for*. A daemon from another build
        // answers the socket, and how its refusal arrives depends on how
        // far apart the builds are: a readable `Pong` with the wrong
        // number is a mismatch, a changed message shape fails to decode,
        // and one that goes away mid-reply is a lost connection. Keying
        // the retry on the mismatch alone would miss the two that mean a
        // bigger difference between the builds.
        for err in [
            ClientError::VersionMismatch {
                client: 6,
                server: 3,
            },
            ClientError::Protocol("something unexpected".into()),
            ClientError::Disconnected,
        ] {
            assert!(is_unspeakable(&CliError::Client(err)), "one more round");
        }
    }

    #[test]
    fn a_daemon_that_simply_will_not_start_is_not_retried() {
        // A second round costs another five seconds of waiting, and these
        // say nothing about an old daemon holding the socket.
        for err in [
            CliError::Client(ClientError::SpawnTimeout),
            CliError::Client(ClientError::Spawn("no such binary".into())),
            CliError::Local("something else".into()),
        ] {
            assert!(!is_unspeakable(&err), "got: {err}");
        }
    }

    /// A handshake from a daemon that has been up this long.
    fn up_for(seconds: u64) -> Pong {
        Pong {
            version: "0.1.0 (abc1234)".to_string(),
            protocol: kobune_api::PROTOCOL_VERSION,
            runtime: "docker 28.0.0".to_string(),
            uptime_secs: seconds,
        }
    }

    /// The five seconds a round spends waiting for the socket to go quiet.
    const ROUND: Duration = Duration::from_secs(5);

    #[test]
    fn a_daemon_that_outlasted_the_wait_is_the_one_that_was_already_there() {
        // The failure the rounds are against, and the quiet one: a daemon
        // of *this* build shakes hands perfectly well, so meeting it
        // after a stop was reported as a restart that had happened. Its
        // uptime is what gives it away — it carries the round on top of
        // what it had on the way in.
        assert!(Outgoing::Up(3_600).outlasted(&up_for(3_605), ROUND));
    }

    #[test]
    fn a_daemon_launchd_woke_during_the_stop_is_a_restart() {
        // The good case wears the same `DaemonStart::Existing`: a request
        // reaching :80 in the gap wakes launchd's job, so the connect at
        // the top of `connect_or_spawn` succeeds and never gets as far as
        // waking anything itself. That machine is where the restart
        // wanted it, and what answers cannot have been up longer than the
        // round it appeared in.
        assert!(!Outgoing::Up(3_600).outlasted(&up_for(4), ROUND));
    }

    #[test]
    fn a_daemon_replaced_moments_after_it_started_is_still_a_restart() {
        // Held against each other alone, four seconds looks like more
        // than two and the restart calls itself a failure — on a machine
        // where it worked. The round's own five seconds are what the
        // comparison is missing, and restarting a daemon that has just
        // come up is not a rare thing to do.
        assert!(!Outgoing::Up(2).outlasted(&up_for(4), ROUND));
    }

    #[test]
    fn a_daemon_that_was_never_there_cannot_have_failed_to_stop() {
        // Nothing answered, so a restart is a start, and one that
        // produced a daemon did what it was asked.
        assert!(!Outgoing::Absent.outlasted(&up_for(4), ROUND));
    }

    #[test]
    fn a_daemon_older_than_the_round_was_never_asked_to_stop() {
        // The reading and the stop both connect, and both can fail to.
        // A daemon that answers afterwards having been up an hour was
        // there through all of it, and calling that a restart is the bug
        // by another route.
        assert!(Outgoing::Absent.outlasted(&up_for(3_600), ROUND));
    }

    #[test]
    fn a_daemon_that_would_not_say_is_taken_for_the_same_one() {
        // Nothing to compare against, so having met anything at all is
        // the whole of the evidence. Erring the other way is the bug: one
        // wasted round costs a stop and a wait, and believing it reports
        // a restart that did not happen.
        assert!(Outgoing::Mute.outlasted(&up_for(0), ROUND));
    }

    #[test]
    fn a_daemon_that_stayed_is_caught_however_long_the_round_took() {
        // Uptime arrives in whole seconds and the round's clock is read
        // in whole seconds too, but the daemon that stayed can never fall
        // through the gap between them: it gains every second the round
        // spends, and a floor never loses more than the two floors
        // beneath it.
        for waited in 0..30 {
            for uptime in [0, 1, 7, 3_600] {
                let stayed = up_for(uptime + waited);

                assert!(
                    Outgoing::Up(uptime).outlasted(&stayed, Duration::from_secs(waited)),
                    "up {uptime}s, waited {waited}s"
                );
            }
        }
    }

    #[test]
    fn the_remedy_for_an_old_daemon_is_a_command_that_exists() {
        // The message names a command, and for a while there was no such
        // command: a daemon left running from an older build told people
        // to run `kobune daemon restart` and clap answered "unrecognized
        // subcommand". Whatever the message says has to parse.
        let message = ClientError::VersionMismatch {
            client: 5,
            server: 3,
        }
        .to_string();

        let quoted = message
            .split('`')
            .nth(1)
            .unwrap_or_else(|| panic!("no command in: {message}"));

        let words: Vec<&str> = quoted.split_whitespace().collect();
        Cli::try_parse_from(&words)
            .unwrap_or_else(|err| panic!("`{quoted}` does not parse: {err}"));
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

        let window = terminal.then(|| Window::new(120, 40));
        terminal_to_offer(&cli, services, *follow, *no_input, window).is_some()
    }

    #[test]
    fn following_one_service_from_a_terminal_offers_it() {
        assert!(would_attach(&["kobune", "logs", "-f", "web"], true));
    }

    #[test]
    fn nothing_is_offered_without_a_terminal() {
        // `kobune logs -f web | grep ready` and every agent invocation
        // land here. Raw mode needs a terminal on both sides, and escape
        // sequences in a pipe are noise.
        assert!(!would_attach(&["kobune", "logs", "-f", "web"], false));
    }

    #[test]
    fn watching_every_service_stays_a_read() {
        // There is no one service to type at, and picking one would be a
        // guess. `kobune logs -f` keeps meaning what it always meant.
        assert!(!would_attach(&["kobune", "logs", "-f"], true));
        assert!(!would_attach(&["kobune", "logs", "-f", "web", "api"], true));
    }

    #[test]
    fn a_finite_read_is_never_interactive() {
        // Without `-f` this prints what there is and stops. Taking the
        // terminal for that would be a session nobody asked to start.
        assert!(!would_attach(&["kobune", "logs", "web"], true));
    }

    #[test]
    fn json_and_no_input_both_opt_out() {
        assert!(!would_attach(
            &["kobune", "logs", "-f", "web", "--json"],
            true
        ));
        assert!(!would_attach(
            &["kobune", "logs", "-f", "web", "--no-input"],
            true
        ));
    }

    #[test]
    fn json_flag_is_available_on_every_subcommand() {
        // An agent has to be able to reach for --json anywhere.
        for args in [
            vec!["kobune", "ls", "--json"],
            vec!["kobune", "status", "--json"],
            vec!["kobune", "up", "--json"],
            vec!["kobune", "new", "feature/x", "--json"],
            vec!["kobune", "url", "web", "--json"],
            vec!["kobune", "daemon", "status", "--json"],
        ] {
            let cli = Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert!(cli.json, "{args:?}");
        }
    }

    #[test]
    fn setup_takes_an_answer_up_front_and_a_way_to_run_nothing() {
        // Plain `kobune setup` asks. The flags are for the two cases that
        // cannot: something that will not be there to answer, and someone
        // who only wants to read the commands.
        let cli = Cli::try_parse_from(["kobune", "setup"]).expect("parses");
        assert!(matches!(
            cli.command,
            Command::Setup {
                yes: false,
                dry_run: false
            }
        ));

        for args in [
            vec!["kobune", "setup", "--yes"],
            vec!["kobune", "setup", "-y"],
        ] {
            let cli = Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert!(
                matches!(cli.command, Command::Setup { yes: true, .. }),
                "{args:?}"
            );
        }

        let cli = Cli::try_parse_from(["kobune", "setup", "--dry-run"]).expect("parses");
        assert!(matches!(cli.command, Command::Setup { dry_run: true, .. }));
    }

    #[test]
    fn a_group_named_without_a_subcommand_is_answered_with_its_help() {
        // `kobune skill` prints the group's help, and adding a global
        // flag must not turn that into a usage error: --json says
        // nothing about which subcommand was meant.
        for (args, expected) in [
            (vec!["kobune", "skill", "--json"], vec!["skill"]),
            (vec!["kobune", "env", "--json"], vec!["env"]),
            (
                vec!["kobune", "daemon", "--workspace", "feat-1"],
                vec!["daemon"],
            ),
            (vec!["kobune", "tunnel", "-w", "feat-1"], vec!["tunnel"]),
            // The root has the same shape, and the same flag on it.
            (vec!["kobune", "--json"], vec![]),
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
        for args in [vec!["kobune", "--version"], vec!["kobune", "-V"]] {
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
            vec!["kobune", "--help"],
            vec!["kobune", "bogus"],
            vec!["kobune", "exec"],
            vec!["kobune", "ls", "--nope"],
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
        let cli = Cli::try_parse_from(["kobune", "new", "feature/x"]).expect("parses");
        let request = build_request(&cli, Target::new(PathBuf::from("/repo"))).expect("builds");

        match request {
            Request::New { start, branch, .. } => {
                assert!(start, "`kobune new` brings the environment up by default");
                assert_eq!(branch, "feature/x");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn no_start_flag_is_respected() {
        let cli =
            Cli::try_parse_from(["kobune", "new", "feature/x", "--no-start"]).expect("parses");
        let request = build_request(&cli, Target::new(PathBuf::from("/repo"))).expect("builds");

        match request {
            Request::New { start, .. } => assert!(!start),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn workspace_flag_reaches_the_request() {
        let cli = Cli::try_parse_from(["kobune", "up", "--workspace", "feat-1"]).expect("parses");
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
        let cli = Cli::try_parse_from(["kobune", "url", "web"]).expect("parses");
        let request = build_request(&cli, Target::new(PathBuf::from("/repo"))).expect("builds");

        assert!(matches!(request, Request::Status { .. }));
    }

    #[test]
    fn a_code_is_asked_for_and_never_assumed() {
        // Plain `kobune url web` goes inside `$(…)`. A QR code there
        // would be a screenful of blocks where a URL was expected.
        let plain = Cli::try_parse_from(["kobune", "url", "web"]).expect("parses");
        assert!(matches!(plain.command, Command::Url { qr: false, .. }));

        let asked = Cli::try_parse_from(["kobune", "url", "web", "--qr"]).expect("parses");
        assert!(matches!(asked.command, Command::Url { qr: true, .. }));
    }

    #[test]
    fn a_code_can_be_asked_for_without_naming_a_service() {
        let cli = Cli::try_parse_from(["kobune", "url", "--qr"]).expect("parses");

        match cli.command {
            Command::Url { service, qr } => {
                assert!(service.is_none());
                assert!(qr);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn the_json_for_a_service_carries_both_urls() {
        let mut web = kobune_api::ServiceInfo {
            name: "web".into(),
            state: kobune_core::ServiceState::Ready,
            reason: None,
            scope: kobune_core::ServiceScope::Workspace,
            url: Some("https://web.localhost".into()),
            tunnel_url: None,
            endpoint: None,
            port: None,
            container_id: None,
            image: None,
        };

        let json = url_json(&web);
        assert_eq!(json["service"], "web");
        assert_eq!(json["url"], "https://web.localhost");
        assert!(json.get("tunnel_url").is_none(), "got: {json}");

        web.tunnel_url = Some("https://web.myapp.example.com".into());
        assert_eq!(
            url_json(&web)["tunnel_url"],
            "https://web.myapp.example.com"
        );
    }

    #[test]
    fn a_service_with_nowhere_to_go_carries_no_url_at_all() {
        // Absent rather than null, which is the shape every optional field
        // in the API already has — `.url // empty` in jq works either way,
        // and `has("url")` is the question being asked.
        let db = kobune_api::ServiceInfo {
            name: "db".into(),
            state: kobune_core::ServiceState::Ready,
            reason: None,
            scope: kobune_core::ServiceScope::Workspace,
            url: None,
            tunnel_url: None,
            endpoint: None,
            port: None,
            container_id: None,
            image: None,
        };

        let json = url_json(&db);
        assert!(json.get("url").is_none(), "got: {json}");
        assert_eq!(json["state"], "ready");
    }

    #[test]
    fn up_collects_service_names() {
        let cli = Cli::try_parse_from(["kobune", "up", "web", "api"]).expect("parses");
        let request = build_request(&cli, Target::new(PathBuf::from("/repo"))).expect("builds");

        match request {
            Request::Up { services, .. } => assert_eq!(services, vec!["web", "api"]),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn enabling_a_tunnel_requires_saying_public_out_loud() {
        // Kobune cannot apply a Cloudflare Access policy, so it cannot
        // promise one is there. The flag is the acknowledgement, and it
        // defaults off.
        let cli = Cli::try_parse_from(["kobune", "tunnel", "enable", "--domain", "example.com"])
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
        let cli = Cli::try_parse_from(["kobune", "tunnel", "enable", "--public"]).expect("parses");
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
        let cli = Cli::try_parse_from(["kobune", "tunnel", "status"]).expect("parses");
        let request = build_request(&cli, Target::new(PathBuf::from("/repo"))).expect("builds");

        assert!(matches!(request, Request::TunnelStatus { .. }));
    }

    #[test]
    fn uninstall_asks_before_it_acts() {
        // Neither flag set is the safe shape: the plan is shown and a
        // terminal is asked.
        let cli = Cli::try_parse_from(["kobune", "uninstall"]).expect("parses");
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
            vec!["kobune", "uninstall", "--yes"],
            vec!["kobune", "uninstall", "-y"],
        ] {
            let cli = Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert!(matches!(cli.command, Command::Uninstall { yes: true, .. }));
        }
    }

    #[test]
    fn uninstall_can_report_without_removing() {
        let cli = Cli::try_parse_from(["kobune", "uninstall", "--dry-run"]).expect("parses");
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
    fn an_update_does_not_repeat_its_own_steps() {
        // It has just printed what it could be sure of, in a panel. The
        // same lines under it, as a notice, would read as a second list.
        assert!(!wants_followup_notice(&Command::Update { check: false }));

        // The command after it is where the build that landed says the
        // rest, so that one does carry them.
        assert!(wants_followup_notice(&Command::Status));
    }

    #[test]
    fn stopping_the_daemon_is_not_answered_with_go_and_stop_the_daemon() {
        // `stop` returns as soon as the request is written, so the socket
        // is up for a moment after it. The notice would report the daemon
        // somebody has this second stopped as still running — and it is
        // the command the step sends them to in the first place.
        assert!(!wants_followup_notice(&Command::Daemon {
            command: DaemonCommand::Stop
        }));

        // Still carried by the update check, which has nothing to do with
        // what is on the socket.
        assert!(wants_update_notice(&Command::Daemon {
            command: DaemonCommand::Stop
        }));
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

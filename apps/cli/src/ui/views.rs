//! The views themselves.
//!
//! Each one turns a piece of the daemon's structured data into a
//! [`Panel`]. They hold no I/O and no decisions — the daemon has already
//! made those — so each is a pure function from a response to something
//! shaped like a screen, which is what makes them worth keeping when a
//! full-screen mode arrives.

use std::path::Path;

use minato_api::{
    Check, Diagnostics, EnvInfo, Pong, ServiceInfo, TunnelInfo, TunnelState, Unsettled,
    UnsettledReason, WorkspaceInfo,
};
use ratatui::text::{Line, Span};

use super::panel::{Grid, Panel};
use super::theme::{self, Decor};

/// Shown against a service with nowhere to go.
const NO_ADDRESS: &str = "—";

/// One workspace in full: `minato status`, and what `up`, `down` and `new`
/// leave on the screen when they are done.
pub fn workspace(info: &WorkspaceInfo, decor: Decor) -> Panel {
    let mut panel = Panel::new(decor, title(info)).lines(vec![Line::from(vec![
        Span::styled(info.branch.clone(), theme::muted()),
        Span::styled("  ", theme::muted()),
        Span::styled(display_path(&info.path), theme::muted()),
    ])]);

    if info.services.is_empty() {
        return panel.line(Span::styled("no services are defined", theme::muted()));
    }

    let mut services = Grid::new();
    for service in &info.services {
        services.push(vec![
            service_name(service),
            Line::styled(service.state.label(), theme::service_state(&service.state)),
            access(service),
        ]);
    }
    panel = panel.grid(services);

    // Only once a tunnel is up. A second column of blanks the rest of the
    // time costs more than it explains.
    let shared: Vec<&ServiceInfo> = info
        .services
        .iter()
        .filter(|service| service.tunnel_url.is_some())
        .collect();

    if !shared.is_empty() {
        let mut tunnel =
            Grid::new().caption(Span::styled("shared over the tunnel:", theme::heading()));
        for service in shared {
            tunnel.push(vec![
                Line::styled(service.name.clone(), theme::subject()),
                Line::styled(
                    service.tunnel_url.clone().unwrap_or_default(),
                    theme::link(),
                ),
            ]);
        }

        panel = panel.grid(tunnel);
    }

    // A failure explains itself here rather than in the state column,
    // which has room for one word and no more.
    panel.lines(
        info.services
            .iter()
            // **From `reason`, not from inside the state.** The state
            // arrives from the daemon as a plain string, so the text is
            // beside it — reading the enum's payload here would show an
            // empty line for every failure.
            .filter_map(|service| {
                let reason = service.reason.as_ref()?;

                Some(Line::from(vec![
                    Span::styled(
                        format!("{} ", theme::service_symbol(&service.state)),
                        theme::bad(),
                    ),
                    Span::styled(format!("{}: ", service.name), theme::subject()),
                    Span::styled(reason.clone(), theme::bad()),
                ]))
            })
            .collect(),
    )
}

/// Every workspace: `minato ls`.
///
/// The project column appears only when more than one is listed. With a
/// single project it is the same word on every row, and `ls` is something
/// people read at a glance.
pub fn workspaces(list: &[WorkspaceInfo], decor: Decor) -> Panel {
    if list.is_empty() {
        return Panel::new(decor, "workspaces")
            .line(Span::styled("none yet", theme::muted()))
            .line(hint("create one with", "minato new <branch>"));
    }

    let projects: std::collections::BTreeSet<&str> =
        list.iter().map(|w| w.project.as_str()).collect();
    let show_project = projects.len() > 1;

    let mut header: Vec<Line<'static>> = Vec::new();
    if show_project {
        header.push("PROJECT".into());
    }
    header.extend(["WORKSPACE".into(), "SERVICES".into(), "BRANCH".into()]);

    let mut grid = Grid::new().header(header);

    for workspace in list {
        let running = workspace
            .services
            .iter()
            .filter(|service| service.state.is_running())
            .count();
        let total = workspace.services.len();

        let mut row: Vec<Line<'static>> = Vec::new();
        if show_project {
            row.push(Line::styled(workspace.project.clone(), theme::muted()));
        }
        row.extend([
            Line::styled(workspace.display_name().to_string(), theme::subject()),
            Line::styled(format!("{running}/{total}"), running_style(running, total)),
            Line::styled(workspace.branch.clone(), theme::muted()),
        ]);

        grid.push(row);
    }

    Panel::new(decor, "workspaces").grid(grid)
}

/// `minato doctor`: what was found, and what to do about it.
pub fn diagnostics(diagnostics: &Diagnostics, decor: Decor) -> Panel {
    let mut checks = Grid::new();
    for check in &diagnostics.checks {
        checks.push(vec![
            Line::styled(check.status.symbol(), theme::check_status(check.status)),
            Line::styled(check.title.clone(), theme::subject()),
            Line::styled(check.detail.clone(), theme::muted()),
        ]);
    }

    let panel = Panel::new(decor, "doctor").grid(checks);

    // "It is broken" helps nobody: the fix travels with the finding.
    let fixes = diagnostics.fixes();
    if fixes.is_empty() {
        return panel.line(if diagnostics.has_failures() {
            Span::styled(
                "something is wrong, but there is no known fix",
                theme::bad(),
            )
        } else {
            Span::styled("nothing wrong found", theme::good())
        });
    }

    let mut lines = vec![Line::styled("to fix:", theme::heading())];
    for check in fixes {
        lines.extend(fix_lines(check));
    }

    panel
        .lines(lines)
        .line(hint("or walk through all of it with", "minato setup"))
}

fn fix_lines(check: &Check) -> Vec<Line<'static>> {
    vec![
        Line::from(vec![
            Span::styled(
                format!("{} ", check.status.symbol()),
                theme::check_status(check.status),
            ),
            Span::styled(check.title.clone(), theme::subject()),
        ]),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(check.fix.clone().unwrap_or_default(), theme::command()),
        ]),
    ]
}

/// One privileged step of `minato setup`.
///
/// Structured rather than pre-formatted so that `--json` and the panel are
/// built from the same thing (`docs/DESIGN.md` §3).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SetupStep {
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub commands: Vec<String>,
}

/// What became of one step of `minato setup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupOutcome {
    /// Every command in it ran, and none of them failed.
    Ran,
    /// Answered with no. Still printed at the end, to run by hand.
    Skipped,
    /// A command was run and came back non-zero.
    Failed,
}

/// `minato setup` with nowhere to ask: it says what to run, and runs none
/// of it.
///
/// `restart_needed` is whether the daemon still has to be restarted for
/// what these steps change to reach it. **Not the same as "something
/// landed":** waking launchd's job restarts the daemon on the way, so
/// saying it again there would stop what was just started.
pub fn setup(steps: &[SetupStep], undo: &[String], restart_needed: bool, decor: Decor) -> Panel {
    if steps.is_empty() {
        return Panel::new(decor, "setup")
            .line(Span::styled("everything is set up", theme::good()))
            .line(hint("confirm with", "minato doctor"));
    }

    let mut panel = Panel::new(decor, "setup").line(Span::styled(
        "the URLs need the following. It requires root, so read each command first.",
        theme::muted(),
    ));

    for (index, step) in steps.iter().enumerate() {
        let mut lines = vec![Line::from(vec![
            Span::styled(format!("{}. ", index + 1), theme::heading()),
            Span::styled(step.description.clone(), theme::subject()),
        ])];

        if let Some(note) = &step.note {
            lines.push(Line::styled(format!("   {note}"), theme::muted()));
        }

        for command in &step.commands {
            lines.push(Line::styled(format!("   {command}"), theme::command()));
        }

        panel = panel.lines(lines);
    }

    if restart_needed {
        panel = panel.lines(restart_hint());
    }

    if undo.is_empty() {
        return panel;
    }

    let mut lines = vec![Line::styled("to undo:", theme::heading())];
    lines.extend(
        undo.iter()
            .map(|command| Line::styled(format!("  {command}"), theme::command())),
    );

    panel.lines(lines)
}

/// What to do with the daemon once a step has changed what launchd holds.
fn restart_hint() -> Vec<Line<'static>> {
    vec![
        hint("afterwards run", "minato daemon stop"),
        Line::styled(
            "  launchd starts it again, with the new settings",
            theme::muted(),
        ),
    ]
}

/// What an interactive `minato setup` is about to walk through.
///
/// **The commands are not in here.** Each step prints its own the moment
/// before it is offered, so what is being agreed to is on the screen rather
/// than several questions further up.
pub fn setup_plan(steps: &[SetupStep], decor: Decor) -> Panel {
    let panel = Panel::new(decor, "setup").lines(vec![
        Line::styled(
            format!(
                "the URLs need {}, and {} root.",
                count(steps.len(), "step"),
                if steps.len() == 1 {
                    "it needs"
                } else {
                    "they need"
                }
            ),
            theme::muted(),
        ),
        Line::styled(
            "each one is shown before it is run, and nothing runs until you say so.",
            theme::muted(),
        ),
    ]);

    let lines = steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            Line::from(vec![
                Span::styled(format!("{}. ", index + 1), theme::heading()),
                Span::styled(step.description.clone(), theme::subject()),
            ])
        })
        .collect();

    panel.lines(lines)
}

/// One step, printed just before it is offered.
pub fn setup_step_lines(number: usize, total: usize, step: &SetupStep) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{number}/{total} "), theme::heading()),
        Span::styled(step.description.clone(), theme::subject()),
    ])];

    if let Some(note) = &step.note {
        lines.push(Line::styled(format!("  {note}"), theme::muted()));
    }

    lines.extend(
        step.commands
            .iter()
            .map(|command| Line::styled(format!("  {command}"), theme::command())),
    );

    lines
}

/// What that step came to, on the line under it.
pub fn setup_outcome_line(outcome: SetupOutcome) -> Line<'static> {
    let (symbol, text, style) = match outcome {
        SetupOutcome::Ran => ("✓", "done", theme::good()),
        SetupOutcome::Skipped => ("–", "skipped", theme::muted()),
        SetupOutcome::Failed => ("✗", "failed", theme::bad()),
    };

    Line::from(vec![
        Span::styled(format!("  {symbol} "), style),
        Span::styled(text, style),
    ])
}

/// Where an interactive `minato setup` left the machine.
///
/// `restart_needed` is as in [`setup`]: whether anything is left for the
/// daemon to be restarted for.
pub fn setup_done(
    steps: &[SetupStep],
    outcomes: &[SetupOutcome],
    undo: &[String],
    restart_needed: bool,
    decor: Decor,
) -> Panel {
    let ran = outcomes
        .iter()
        .filter(|outcome| **outcome == SetupOutcome::Ran)
        .count();

    let mut panel = Panel::new(decor, "setup");

    panel = if ran == outcomes.len() {
        panel.line(Line::from(vec![
            Span::styled("✓ ", theme::good()),
            Span::raw("every step is done"),
        ]))
    } else {
        panel.line(Span::styled(
            format!("{ran} of {} done", count(outcomes.len(), "step")),
            theme::muted(),
        ))
    };

    // Whatever was declined, or failed, gets the answer `minato setup` has
    // always given: the commands, to run by hand. Nothing is left to guess
    // at from a "skipped".
    let left: Vec<(&SetupStep, SetupOutcome)> = steps
        .iter()
        .zip(outcomes)
        .filter(|(_, outcome)| **outcome != SetupOutcome::Ran)
        .map(|(step, outcome)| (step, *outcome))
        .collect();

    if !left.is_empty() {
        let mut lines = vec![Line::styled("still to run, as root:", theme::heading())];

        for (step, outcome) in left {
            lines.push(Line::from(vec![
                Span::styled(
                    match outcome {
                        SetupOutcome::Failed => "  ✗ ",
                        _ => "  – ",
                    },
                    match outcome {
                        SetupOutcome::Failed => theme::bad(),
                        _ => theme::muted(),
                    },
                ),
                Span::styled(step.description.clone(), theme::subject()),
            ]));
            lines.extend(
                step.commands
                    .iter()
                    .map(|command| Line::styled(format!("    {command}"), theme::command())),
            );
        }

        panel = panel.lines(lines);
    }

    // Only worth saying when something landed: the daemon has nothing new
    // to pick up otherwise.
    if ran > 0 && restart_needed {
        panel = panel.lines(restart_hint());
    }

    if undo.is_empty() {
        return panel;
    }

    let mut lines = vec![Line::styled("to undo:", theme::heading())];
    lines.extend(
        undo.iter()
            .map(|command| Line::styled(format!("  {command}"), theme::command())),
    );

    panel.lines(lines)
}

/// `1 step` / `3 steps`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// `minato env ls`.
///
/// **Each value says which layer defined it.** With three layers, not
/// seeing that an unintended one is winning makes the cause impossible to
/// find.
/// Why a value is shown as written, in words.
///
/// **The daemon sends the reason, not the sentence** (`docs/DESIGN.md`
/// §3), so this is where it becomes English.
pub fn unsettled_reason(unsettled: &Unsettled) -> String {
    let name = unsettled.reference.as_deref().unwrap_or("");

    match &unsettled.reason {
        UnsettledReason::Undefined => format!("refers to ${{{name}}}, which nothing sets"),
        UnsettledReason::OnlyWithService { .. } => {
            format!("refers to ${{{name}}}, which this listing does not have")
        }
        UnsettledReason::NeedsProxy => {
            format!("refers to ${{{name}}}, and the proxy is not listening")
        }
        UnsettledReason::Secret => {
            format!("refers to ${{{name}}}, a secret — those resolve at start-up only")
        }
        UnsettledReason::Cycle { chain } => {
            format!("is part of a loop: {}", chain.join(" → "))
        }
    }
}

/// What to do about it, when there is something to do.
pub fn unsettled_remedy(unsettled: &Unsettled) -> Option<String> {
    match &unsettled.reason {
        UnsettledReason::OnlyWithService { service } => {
            Some(format!("minato env ls --service {service}"))
        }
        UnsettledReason::NeedsProxy => Some("minato doctor".to_string()),
        _ => None,
    }
}

pub fn env(entries: &[EnvInfo], decor: Decor) -> Panel {
    if entries.is_empty() {
        return Panel::new(decor, "environment")
            .line(Span::styled("nothing is defined", theme::muted()));
    }

    let mut grid = Grid::new().header(vec!["KEY".into(), "SCOPE".into(), "VALUE".into()]);

    for entry in entries {
        let mut value = vec![Span::styled(
            entry.value.clone(),
            if entry.secret || entry.unsettled.is_some() {
                theme::warn()
            } else {
                Default::default()
            },
        )];

        // Where a secret comes from, never what it is.
        if let Some(source) = &entry.source {
            value.push(Span::styled(format!(" → {source}"), theme::muted()));
        }

        grid.push(vec![
            Line::styled(entry.key.clone(), theme::subject()),
            Line::styled(entry.scope.label(), theme::muted()),
            Line::from(value),
        ]);
    }

    let mut panel = Panel::new(decor, "environment").grid(grid);

    // **Said with the listing, not instead of it.** The value at fault is
    // only findable by looking at the values, so the listing has to
    // arrive — but reading one as settled when it is not would be worse
    // than not having it at all.
    //
    // A line per value rather than one long one: a single sentence would
    // set the panel's preferred width and stretch the frame away from the
    // three short columns it is really made of.
    for entry in entries {
        let Some(unsettled) = &entry.unsettled else {
            continue;
        };

        panel = panel.line(Line::from(vec![
            Span::styled(format!("{} ", entry.key), theme::subject()),
            Span::styled(unsettled_reason(unsettled), theme::warn()),
        ]));

        if let Some(remedy) = unsettled_remedy(unsettled) {
            panel = panel.line(hint("  settled by", &remedy));
        }
    }

    panel
}

/// `minato tunnel status`, and where `enable` and `disable` leave things.
pub fn tunnel(info: &TunnelInfo, decor: Decor) -> Panel {
    let state = Span::styled(info.state.label(), tunnel_style(info.state));

    let mut heading = vec![state];
    if let Some(domain) = &info.domain {
        heading.push(Span::styled(format!("  *.{domain}"), theme::link()));
    }

    let mut panel = Panel::new(decor, "tunnel").line(Line::from(heading));

    if let Some(record) = &info.record {
        panel = panel.line(Line::from(vec![
            Span::styled("DNS  ", theme::muted()),
            Span::styled(record.clone(), theme::link()),
        ]));
    }

    // Being on the internet unauthenticated is the kind of thing that has
    // to be said out loud every time, not buried in the setup output.
    if info.state.is_running() && info.public {
        panel = panel.lines(vec![
            Line::styled(
                "this environment is reachable from the internet.",
                theme::bad(),
            ),
            Line::styled(
                "Minato cannot see whether a Cloudflare Access policy is in front of it.",
                theme::muted(),
            ),
        ]);
    }

    if info.setup.is_empty() {
        return panel;
    }

    let mut lines = vec![Line::styled("run these first:", theme::heading())];
    lines.extend(
        info.setup
            .iter()
            .map(|command| Line::styled(format!("  {command}"), theme::command())),
    );

    panel.lines(lines)
}

/// The daemon, as `ping` and `daemon status` report it.
pub fn daemon(pong: &Pong, socket: Option<&Path>, decor: Decor) -> Panel {
    let mut grid = Grid::new();
    let mut fact = |label: &str, value: String| {
        grid.push(vec![
            Line::styled(label.to_string(), theme::muted()),
            Line::raw(value),
        ]);
    };

    fact("version", pong.version.clone());
    fact("protocol", pong.protocol.to_string());
    fact("runtime", pong.runtime.clone());
    fact("uptime", format_uptime(pong.uptime_secs));
    if let Some(socket) = socket {
        fact("socket", display_path(socket));
    }

    Panel::new(decor, "minatod")
        .line(Span::styled("running", theme::good()))
        .grid(grid)
}

/// The daemon, when there is none.
pub fn daemon_stopped(decor: Decor) -> Panel {
    Panel::new(decor, "minatod")
        .line(Span::styled("stopped", theme::muted()))
        .line(hint("start it with", "minato daemon start"))
}

/// A short confirmation: a few facts, and what to do next.
///
/// What `init`, `skill install` and `update` leave behind. They have no
/// table to show, only the one thing they did.
pub fn done(
    title: &'static str,
    facts: &[(&'static str, String)],
    next: Vec<Line<'static>>,
    decor: Decor,
) -> Panel {
    let mut grid = Grid::new();
    for (label, value) in facts {
        grid.push(vec![
            Line::styled(*label, theme::muted()),
            Line::raw(value.clone()),
        ]);
    }

    Panel::new(decor, title).grid(grid).lines(next)
}

/// What `minato uninstall` is about to do, before it does any of it.
///
/// The containers come from the daemon, the rest from looking at the
/// machine. Worktrees get a section of their own because they are the one
/// thing here that is **not** going, and that has to be as visible as what
/// is.
pub fn uninstall_plan(
    plan: &crate::uninstall::Plan,
    daemon: Result<&minato_api::PurgeReport, &String>,
    dry_run: bool,
    decor: Decor,
) -> Panel {
    let mut panel = Panel::new(decor, "uninstall");

    let services = daemon.map(|report| report.service_count()).unwrap_or(0);

    if services > 0 {
        let mut grid = Grid::new().caption(Span::styled("containers:", theme::heading()));
        for project in daemon.iter().flat_map(|report| &report.projects) {
            for workspace in &project.workspaces {
                for service in &workspace.services {
                    grid.push(vec![
                        Line::styled(
                            format!("{} / {}", project.name, workspace.label),
                            theme::muted(),
                        ),
                        Line::styled(service.clone(), theme::subject()),
                    ]);
                }
            }
        }
        panel = panel.grid(grid);
    }

    // **Named, one by one.** A project volume outlives the worktrees that
    // used it, so this is where somebody's development database is — and
    // the real name is what they would need to save one before saying yes.
    // "3 volumes" would not be something anybody could check.
    let volumes = daemon
        .map(|report| report.volumes.as_slice())
        .unwrap_or(&[]);
    if !volumes.is_empty() {
        let mut grid = Grid::new().caption(Span::styled(
            "storage — the data in it goes too:",
            theme::heading(),
        ));
        for volume in volumes {
            grid.push(vec![
                Line::styled(volume.project.clone(), theme::muted()),
                Line::styled(volume.name.clone(), theme::subject()),
            ]);
        }
        panel = panel.grid(grid);
    }

    // Storage the daemon could not even ask about. Left out, this reads as
    // a machine with no volumes on it — and the whole plan is the thing
    // somebody is about to say yes to.
    let storage_left = daemon
        .map(|report| report.storage_left.as_slice())
        .unwrap_or(&[]);
    if !storage_left.is_empty() {
        let mut lines = vec![Line::styled(
            "storage that could not be accounted for, and stays:",
            theme::bad(),
        )];
        for failure in storage_left {
            lines.push(Line::from(vec![
                Span::styled(format!("  {} ", failure.what), theme::subject()),
                Span::styled(failure.reason.clone(), theme::muted()),
            ]));
        }
        panel = panel.lines(lines);
    }

    // Said out loud, and with the reason. A list that silently leaves the
    // containers out looks like a machine that has none.
    if let Err(reason) = daemon {
        panel = panel.lines(vec![
            Line::styled(
                "the daemon's containers and storage are not in this list:",
                theme::warn(),
            ),
            Line::styled(format!("  {reason}"), theme::muted()),
        ]);
    }

    if !plan.files.is_empty() {
        let mut grid = Grid::new().caption(Span::styled("files:", theme::heading()));
        for removal in &plan.files {
            grid.push(vec![
                Line::styled(removal.label, theme::muted()),
                Line::raw(display_path(&removal.path)),
            ]);
        }
        panel = panel.grid(grid);
    }

    if !plan.privileged.is_empty() {
        let mut lines = vec![Line::styled("needs root:", theme::heading())];
        for step in &plan.privileged {
            lines.push(Line::styled(format!("  {}", step.label), theme::subject()));
            lines.extend(
                step.commands
                    .iter()
                    .map(|command| Line::styled(format!("    {command}"), theme::command())),
            );
        }
        panel = panel.lines(lines);
    }

    // What stays in the Cloudflare account. Silence here would read as
    // "there is nothing left", and there is.
    if let Some(tunnel) = daemon.ok().and_then(|report| report.tunnel.as_ref()) {
        let mut lines = vec![Line::styled(
            match &tunnel.domain {
                Some(domain) => format!("left in your Cloudflare account (*.{domain}):"),
                None => "left in your Cloudflare account:".to_string(),
            },
            theme::heading(),
        )];
        lines.extend(
            tunnel
                .commands
                .iter()
                .map(|command| Line::styled(format!("  {command}"), theme::command())),
        );
        panel = panel.lines(lines);
    }

    // The point of saying so: someone uninstalling wants to know whether
    // their branches went with it.
    let worktrees = daemon
        .map(|report| report.worktrees.as_slice())
        .unwrap_or(&[]);
    if !worktrees.is_empty() {
        let mut lines = vec![Line::styled(
            format!(
                "left alone — {} worktree{}:",
                worktrees.len(),
                if worktrees.len() == 1 { "" } else { "s" }
            ),
            theme::good(),
        )];
        lines.extend(
            worktrees
                .iter()
                .map(|path| Line::styled(format!("  {}", display_path(path)), theme::muted())),
        );
        panel = panel.lines(lines);
    }

    if plan.files.is_empty()
        && plan.privileged.is_empty()
        && services == 0
        && volumes.is_empty()
        && storage_left.is_empty()
    {
        return panel.line(Span::styled(
            "nothing of Minato's was found on this machine",
            theme::muted(),
        ));
    }

    if let Ok(report) = daemon
        && !report.stranded.is_empty()
    {
        let mut lines = vec![Line::styled(
            "could not be reached, and will be kept for a later run:",
            theme::bad(),
        )];
        for failure in &report.stranded {
            lines.push(Line::from(vec![
                Span::styled(format!("  {} ", failure.project), theme::subject()),
                Span::styled(failure.reason.clone(), theme::muted()),
            ]));
        }
        panel = panel.lines(lines);
    }

    if dry_run {
        panel = panel.line(hint("to go ahead, run", "minato uninstall"));
    }

    panel
}

/// What `minato uninstall` managed, and what it did not.
pub fn uninstall_done(
    failures: &[String],
    remaining: &[crate::uninstall::Privileged],
    decor: Decor,
) -> Panel {
    let mut panel = Panel::new(decor, "uninstall");

    panel = if failures.is_empty() {
        panel.line(Line::from(vec![
            Span::styled("✓ ", theme::good()),
            Span::raw("removed"),
        ]))
    } else {
        let mut lines = vec![Line::styled("could not remove:", theme::bad())];
        lines.extend(
            failures
                .iter()
                .map(|failure| Line::styled(format!("  {failure}"), theme::muted())),
        );
        panel.lines(lines)
    };

    if remaining.is_empty() {
        return panel;
    }

    // Reached when there was no terminal to type a password into, or sudo
    // said no. Printing the commands is the same answer `minato setup`
    // gives, and it leaves the machine in a state a person can finish.
    let mut lines = vec![Line::styled("still to run, as root:", theme::heading())];
    for step in remaining {
        lines.push(Line::styled(format!("  {}", step.label), theme::subject()));
        lines.extend(
            step.commands
                .iter()
                .map(|command| Line::styled(format!("    {command}"), theme::command())),
        );
    }

    panel.lines(lines)
}

/// A remark of the CLI's own, set apart from the daemon's answer.
pub fn note(text: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("› ", theme::muted()),
        Span::styled(text.to_string(), theme::muted()),
    ])
}

/// `› run this` — the same shape wherever the CLI suggests a command.
pub fn hint(text: &str, command: &str) -> Line<'static> {
    let mut line = note(text);
    line.push_span(Span::raw(" "));
    line.push_span(Span::styled(command.to_string(), theme::command()));
    line
}

fn title(info: &WorkspaceInfo) -> Line<'static> {
    Line::from(vec![
        Span::styled(info.project.clone(), theme::subject()),
        Span::styled(" / ", theme::muted()),
        Span::styled(info.display_name().to_string(), theme::subject()),
    ])
}

fn service_name(service: &ServiceInfo) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{} ", theme::service_symbol(&service.state)),
            theme::service_state(&service.state),
        ),
        Span::styled(service.name.clone(), theme::subject()),
    ])
}

/// Where to reach a service.
///
/// A blank against something that is running reads as broken, so a service
/// with no way in says so.
fn access(service: &ServiceInfo) -> Line<'static> {
    match service.access() {
        Some(url) => Line::styled(url, theme::link()),
        None if service.state.is_running() => Line::styled("internal only", theme::muted()),
        None => Line::styled(NO_ADDRESS, theme::muted()),
    }
}

fn running_style(running: usize, total: usize) -> ratatui::style::Style {
    if total == 0 || running == 0 {
        theme::muted()
    } else if running == total {
        theme::good()
    } else {
        theme::warn()
    }
}

fn tunnel_style(state: TunnelState) -> ratatui::style::Style {
    match state {
        TunnelState::Running => theme::good(),
        TunnelState::NeedsLogin | TunnelState::Stopped => theme::warn(),
        TunnelState::NotInstalled => theme::bad(),
        TunnelState::Disabled => theme::muted(),
    }
}

/// `3d 4h`, `12m`, `8s` — the daemon's uptime at the precision anyone
/// reads it at.
fn format_uptime(seconds: u64) -> String {
    let (days, hours) = (seconds / 86_400, (seconds % 86_400) / 3_600);
    let (minutes, seconds) = ((seconds % 3_600) / 60, seconds % 60);

    match (days, hours, minutes) {
        (0, 0, 0) => format!("{seconds}s"),
        (0, 0, _) => format!("{minutes}m {seconds}s"),
        (0, _, _) => format!("{hours}h {minutes}m"),
        _ => format!("{days}d {hours}h"),
    }
}

/// Paths with `$HOME` folded back to `~`.
///
/// Worktree paths are long and the interesting half is the end of them.
fn display_path(path: &Path) -> String {
    let Some(home) = dirs::home_dir() else {
        return path.display().to_string();
    };

    match path.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::render;
    use minato_api::CheckStatus;
    use minato_core::ServiceState;
    use minato_core::{EnvScope, ServiceScope};
    use std::path::PathBuf;

    fn service(name: &str, state: ServiceState, url: Option<&str>) -> ServiceInfo {
        ServiceInfo {
            reason: state.reason().map(str::to_string),
            name: name.into(),
            state,
            scope: ServiceScope::Workspace,
            url: url.map(str::to_string),
            tunnel_url: None,
            endpoint: None,
            port: Some(3000),
            container_id: None,
            image: None,
        }
    }

    fn info(services: Vec<ServiceInfo>) -> WorkspaceInfo {
        WorkspaceInfo {
            project: "myapp".into(),
            workspace: Some("feat-1".into()),
            branch: "feature/user-auth".into(),
            path: PathBuf::from("/repo/myapp.wt/feat-1"),
            is_main: false,
            services,
        }
    }

    #[test]
    fn a_workspace_names_itself_and_its_branch() {
        let text = render(&workspace(&info(vec![]), Decor::PLAIN));

        assert!(text.contains("myapp / feat-1"), "got:\n{text}");
        assert!(text.contains("feature/user-auth"), "got:\n{text}");
    }

    #[test]
    fn a_url_is_never_truncated() {
        // The whole point of `status` is being able to copy the URL out of
        // it, and these are long enough to be worth checking.
        let url = "https://web.feature-user-auth.myapp.localhost";
        let text = render(&workspace(
            &info(vec![service("web", ServiceState::Ready, Some(url))]),
            Decor::FRAMED,
        ));

        assert!(text.contains(url), "got:\n{text}");
    }

    #[test]
    fn a_running_service_with_no_way_in_says_so() {
        // A blank there looks like something failed to be filled in.
        let text = render(&workspace(
            &info(vec![service("db", ServiceState::Ready, None)]),
            Decor::PLAIN,
        ));

        assert!(text.contains("internal only"), "got:\n{text}");
    }

    #[test]
    fn a_failure_gives_its_reason() {
        // The state column has room for one word; the reason is the part
        // that says what to do next.
        let text = render(&workspace(
            &info(vec![service(
                "web",
                ServiceState::failed("port 3000 is in use"),
                None,
            )]),
            Decor::PLAIN,
        ));

        assert!(text.contains("port 3000 is in use"), "got:\n{text}");
    }

    #[test]
    fn a_workspace_with_no_services_says_so_rather_than_showing_nothing() {
        let text = render(&workspace(&info(vec![]), Decor::PLAIN));
        assert!(text.contains("no services are defined"), "got:\n{text}");
    }

    #[test]
    fn the_project_column_appears_only_when_projects_differ() {
        let mut other = info(vec![]);
        other.project = "otherapp".into();

        let single = render(&workspaces(&[info(vec![])], Decor::PLAIN));
        assert!(!single.contains("PROJECT"), "got:\n{single}");

        let several = render(&workspaces(&[info(vec![]), other], Decor::PLAIN));
        assert!(several.contains("PROJECT"), "got:\n{several}");
        assert!(several.contains("otherapp"), "got:\n{several}");
    }

    #[test]
    fn an_empty_listing_says_how_to_start() {
        let text = render(&workspaces(&[], Decor::PLAIN));
        assert!(text.contains("minato new <branch>"), "got:\n{text}");
    }

    #[test]
    fn a_listing_counts_the_running_services() {
        let text = render(&workspaces(
            &[info(vec![
                service("web", ServiceState::Ready, None),
                service("db", ServiceState::Stopped, None),
            ])],
            Decor::PLAIN,
        ));

        assert!(text.contains("1/2"), "got:\n{text}");
    }

    #[test]
    fn a_diagnosis_carries_its_fix() {
        let text = render(&diagnostics(
            &Diagnostics::new(vec![
                Check::ok("runtime", "Docker", "29.4.0"),
                Check::fail("resolver", "DNS resolver", "not installed")
                    .with_fix("sudo minato setup"),
            ]),
            Decor::PLAIN,
        ));

        assert!(text.contains("Docker"), "got:\n{text}");
        assert!(text.contains("sudo minato setup"), "got:\n{text}");
    }

    #[test]
    fn a_clean_diagnosis_says_so_without_a_fix_section() {
        let text = render(&diagnostics(
            &Diagnostics::new(vec![Check::ok("runtime", "Docker", "29.4.0")]),
            Decor::PLAIN,
        ));

        assert!(text.contains("nothing wrong found"), "got:\n{text}");
        assert!(!text.contains("to fix"), "got:\n{text}");
    }

    #[test]
    fn a_failure_with_no_known_fix_admits_it() {
        let text = render(&diagnostics(
            &Diagnostics::new(vec![Check::fail("x", "X", "broken")]),
            Decor::PLAIN,
        ));

        assert!(text.contains("no known fix"), "got:\n{text}");
    }

    #[test]
    fn setup_numbers_its_steps_and_shows_the_commands() {
        let steps = vec![
            SetupStep {
                description: "let launchd hold the ports".into(),
                note: Some("generated plist: /tmp/x.plist".into()),
                commands: vec!["sudo cp /tmp/x.plist /Library/LaunchDaemons/".into()],
            },
            SetupStep {
                description: "point *.localhost at Minato".into(),
                note: None,
                commands: vec!["sudo tee /etc/resolver/localhost".into()],
            },
        ];

        let text = render(&setup(
            &steps,
            &["sudo rm /Library/x".into()],
            true,
            Decor::PLAIN,
        ));

        assert!(text.contains("1. let launchd"), "got:\n{text}");
        assert!(text.contains("2. point"), "got:\n{text}");
        assert!(text.contains("/tmp/x.plist"), "got:\n{text}");
        assert!(text.contains("to undo:"), "got:\n{text}");
        assert!(text.contains("minato daemon stop"), "got:\n{text}");
    }

    #[test]
    fn a_plan_that_restarts_the_daemon_itself_does_not_ask_for_it_again() {
        // Waking launchd's job stops the daemon on the way. Being told to
        // stop it afterwards would stop what had just been started.
        let steps = vec![SetupStep {
            description: "wake launchd's job".into(),
            note: None,
            commands: vec![
                "minato daemon stop".into(),
                "sudo launchctl kickstart -k x".into(),
            ],
        }];

        let text = render(&setup(&steps, &[], false, Decor::PLAIN));

        assert!(!text.contains("afterwards run"), "got:\n{text}");
    }

    #[test]
    fn nothing_left_to_set_up_is_not_an_empty_screen() {
        let text = render(&setup(&[], &[], false, Decor::PLAIN));
        assert!(text.contains("everything is set up"), "got:\n{text}");
    }

    /// Two steps, the first with a note, as `minato setup` builds them.
    fn setup_steps() -> Vec<SetupStep> {
        vec![
            SetupStep {
                description: "let launchd hold the ports".into(),
                note: Some("generated plist: /tmp/x.plist".into()),
                commands: vec!["sudo cp /tmp/x.plist /Library/LaunchDaemons/".into()],
            },
            SetupStep {
                description: "point *.localhost at Minato".into(),
                note: None,
                commands: vec!["sudo tee /etc/resolver/localhost".into()],
            },
        ]
    }

    fn text_of(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_plan_names_the_steps_without_their_commands() {
        // Each command is shown again the moment before it is offered.
        // Here it would only be something to scroll past.
        let text = render(&setup_plan(&setup_steps(), Decor::PLAIN));

        assert!(text.contains("1. let launchd"), "got:\n{text}");
        assert!(text.contains("2. point"), "got:\n{text}");
        assert!(!text.contains("sudo cp"), "got:\n{text}");
        assert!(
            text.contains("nothing runs until you say so"),
            "got:\n{text}"
        );
    }

    #[test]
    fn a_step_shows_its_command_before_it_is_offered() {
        // Agreeing to a description is not agreeing to what it runs as
        // root, so the command is on the screen when the question is.
        let steps = setup_steps();
        let text = text_of(&setup_step_lines(1, 2, &steps[0]));

        assert!(text.contains("1/2"), "got:\n{text}");
        assert!(text.contains("sudo cp /tmp/x.plist"), "got:\n{text}");
        assert!(text.contains("/tmp/x.plist"), "got:\n{text}");
    }

    #[test]
    fn what_was_declined_is_still_printed_to_run_by_hand() {
        let steps = setup_steps();
        let text = render(&setup_done(
            &steps,
            &[SetupOutcome::Ran, SetupOutcome::Skipped],
            &["sudo rm /Library/x".into()],
            true,
            Decor::PLAIN,
        ));

        assert!(text.contains("1 of 2 steps done"), "got:\n{text}");
        assert!(text.contains("still to run, as root:"), "got:\n{text}");
        assert!(text.contains("sudo tee /etc/resolver"), "got:\n{text}");
        // The one that ran needs nothing done to it.
        assert!(!text.contains("sudo cp /tmp/x.plist"), "got:\n{text}");
        // Something landed, so the daemon has to be restarted to see it.
        assert!(text.contains("minato daemon stop"), "got:\n{text}");
        assert!(text.contains("to undo:"), "got:\n{text}");
    }

    #[test]
    fn a_failed_step_is_not_reported_as_done() {
        let steps = setup_steps();
        let text = render(&setup_done(
            &steps,
            &[SetupOutcome::Failed, SetupOutcome::Failed],
            &[],
            true,
            Decor::PLAIN,
        ));

        assert!(!text.contains("every step is done"), "got:\n{text}");
        assert!(text.contains("0 of 2 steps done"), "got:\n{text}");
        assert!(text.contains("still to run, as root:"), "got:\n{text}");
        // Nothing landed, so there is nothing for the daemon to pick up.
        assert!(!text.contains("minato daemon stop"), "got:\n{text}");
    }

    #[test]
    fn a_finished_walk_says_so_and_leaves_nothing_to_run() {
        let steps = setup_steps();
        let text = render(&setup_done(
            &steps,
            &[SetupOutcome::Ran, SetupOutcome::Ran],
            &[],
            true,
            Decor::PLAIN,
        ));

        assert!(text.contains("every step is done"), "got:\n{text}");
        assert!(!text.contains("still to run"), "got:\n{text}");
        assert!(text.contains("minato daemon stop"), "got:\n{text}");
    }

    #[test]
    fn a_walk_that_restarted_the_daemon_does_not_ask_for_it_again() {
        // Every step ran, so this is not "nothing landed" — it is that
        // what landed had the restart in it.
        let steps = setup_steps();
        let text = render(&setup_done(
            &steps,
            &[SetupOutcome::Ran, SetupOutcome::Ran],
            &[],
            false,
            Decor::PLAIN,
        ));

        assert!(text.contains("every step is done"), "got:\n{text}");
        assert!(!text.contains("afterwards run"), "got:\n{text}");
    }

    #[test]
    fn an_outcome_line_names_what_happened() {
        for (outcome, expected) in [
            (SetupOutcome::Ran, "done"),
            (SetupOutcome::Skipped, "skipped"),
            (SetupOutcome::Failed, "failed"),
        ] {
            let text = text_of(&[setup_outcome_line(outcome)]);
            assert!(text.contains(expected), "got:\n{text}");
        }
    }

    #[test]
    fn env_shows_the_layer_each_value_came_from() {
        let entries = vec![
            EnvInfo {
                key: "DATABASE_URL".into(),
                value: "postgres://…".into(),
                scope: EnvScope::Injected,
                secret: false,
                source: None,
                unsettled: None,
            },
            EnvInfo {
                key: "API_KEY".into(),
                value: "••••".into(),
                scope: EnvScope::Workspace,
                secret: true,
                source: Some("1password://vault/item".into()),
                unsettled: None,
            },
        ];

        let text = render(&env(&entries, Decor::PLAIN));

        assert!(text.contains("injected"), "got:\n{text}");
        assert!(text.contains("workspace"), "got:\n{text}");
        // The reference, never the value behind it.
        assert!(text.contains("1password://vault/item"), "got:\n{text}");
    }

    /// One settled value and one that is not.
    fn mixed_entries() -> Vec<EnvInfo> {
        vec![
            EnvInfo {
                key: "SETTLED".into(),
                value: "https://api.feat-1.myapp.localhost".into(),
                scope: EnvScope::Service,
                secret: false,
                source: None,
                unsettled: None,
            },
            EnvInfo {
                key: "API_URL".into(),
                value: "${MINATO_URL_API}".into(),
                scope: EnvScope::Service,
                secret: false,
                source: None,
                unsettled: Some(Unsettled {
                    reference: Some("MINATO_URL_API".into()),
                    reason: UnsettledReason::NeedsProxy,
                }),
            },
        ]
    }

    #[test]
    fn a_listing_that_could_not_settle_still_lists() {
        // This is the tool for finding the value at fault, so it has to
        // arrive — saying so alongside, not instead of it.
        let text = render(&env(&mixed_entries(), Decor::PLAIN));

        assert!(text.contains("API_URL"), "the listing arrives:\n{text}");
        assert!(
            text.contains("the proxy is not listening"),
            "and says what is wrong:\n{text}"
        );
        assert!(
            text.contains("minato doctor"),
            "and what to do about it:\n{text}"
        );
    }

    #[test]
    fn only_the_value_at_fault_is_spoken_for() {
        // One bad reference used to put every other value under
        // suspicion, which left nothing to tell them apart by.
        let text = render(&env(&mixed_entries(), Decor::PLAIN));

        assert!(
            text.contains("https://api.feat-1.myapp.localhost"),
            "the settled one settled:\n{text}"
        );
        assert!(
            !text.contains("SETTLED refers"),
            "and is not accused of anything:\n{text}"
        );
    }

    #[test]
    fn the_reason_survives_a_narrow_terminal() {
        // At its preferred width nothing wraps, so asserting there says
        // nothing about the 80 columns someone actually has.
        let text = super::super::test_support::render_at(&env(&mixed_entries(), Decor::PLAIN), 40);

        assert!(text.contains("API_URL"), "got:\n{text}");
        assert!(text.contains("proxy"), "got:\n{text}");
    }

    #[test]
    fn one_long_reason_does_not_stretch_the_frame() {
        use super::super::View;

        // The note is a line like any other, and `preferred_width` takes
        // the widest — so a sentence left whole would drag the frame away
        // from the three short columns the listing is made of.
        let narrow = env(&mixed_entries(), Decor::PLAIN).preferred_width();

        let mut chained = mixed_entries();
        chained[1].unsettled = Some(Unsettled {
            reference: None,
            reason: UnsettledReason::Cycle {
                chain: vec!["A".into(), "B".into(), "A".into()],
            },
        });

        assert!(
            env(&chained, Decor::PLAIN).preferred_width() <= narrow + 8,
            "a reason should not decide the width of the listing"
        );
    }

    #[test]
    fn a_public_tunnel_says_so_every_time() {
        let text = render(&tunnel(
            &TunnelInfo {
                state: TunnelState::Running,
                domain: Some("example.com".into()),
                record: None,
                setup: vec![],
                public: true,
            },
            Decor::PLAIN,
        ));

        assert!(text.contains("reachable from the internet"), "got:\n{text}");
        assert!(text.contains("*.example.com"), "got:\n{text}");
    }

    #[test]
    fn a_private_tunnel_does_not_warn() {
        let text = render(&tunnel(&TunnelInfo::disabled(), Decor::PLAIN));
        assert!(!text.contains("internet"), "got:\n{text}");
        assert!(text.contains("disabled"), "got:\n{text}");
    }

    #[test]
    fn a_tunnel_awaiting_setup_shows_what_to_run() {
        let text = render(&tunnel(
            &TunnelInfo {
                state: TunnelState::NeedsLogin,
                domain: None,
                record: None,
                setup: vec!["cloudflared tunnel login".into()],
                public: false,
            },
            Decor::PLAIN,
        ));

        assert!(text.contains("cloudflared tunnel login"), "got:\n{text}");
    }

    #[test]
    fn the_daemon_reports_its_uptime_in_units_people_use() {
        assert_eq!(format_uptime(8), "8s");
        assert_eq!(format_uptime(750), "12m 30s");
        assert_eq!(format_uptime(9_000), "2h 30m");
        assert_eq!(format_uptime(300_000), "3d 11h");
    }

    #[test]
    fn a_stopped_daemon_says_how_to_start_it() {
        let text = render(&daemon_stopped(Decor::PLAIN));
        assert!(text.contains("minato daemon start"), "got:\n{text}");
    }

    fn purge_report() -> minato_api::PurgeReport {
        minato_api::PurgeReport {
            dry_run: true,
            projects: vec![minato_api::PurgeProject {
                name: "myapp".into(),
                workspaces: vec![minato_api::PurgeWorkspace {
                    label: "feat-1".into(),
                    services: vec!["web".into(), "db".into()],
                }],
            }],
            worktrees: vec![PathBuf::from("/repo/myapp.wt/feat-1")],
            volumes: vec![minato_api::PurgeVolume {
                project: "myapp".into(),
                name: "minato-myapp-pgdata".into(),
            }],
            ..Default::default()
        }
    }

    fn host_plan() -> crate::uninstall::Plan {
        crate::uninstall::Plan {
            files: vec![crate::uninstall::Removal {
                label: "state, logs and the local CA",
                path: PathBuf::from("/home/u/.minato"),
            }],
            privileged: vec![crate::uninstall::Privileged {
                label: "stop trusting the local CA".into(),
                commands: vec![
                    "sudo security remove-trusted-cert -d /home/u/.minato/ca.crt".into(),
                ],
            }],
        }
    }

    #[test]
    fn the_plan_shows_every_kind_of_thing_it_would_remove() {
        let report = purge_report();
        let text = render(&uninstall_plan(
            &host_plan(),
            Ok(&report),
            false,
            Decor::PLAIN,
        ));

        assert!(text.contains("web"), "containers:\n{text}");
        assert!(text.contains(".minato"), "files:\n{text}");
        assert!(text.contains("remove-trusted-cert"), "root steps:\n{text}");
        assert!(text.contains("minato-myapp-pgdata"), "storage:\n{text}");
    }

    #[test]
    fn storage_is_named_by_what_the_runtime_calls_it() {
        // `pgdata` is what `minato.toml` says; `minato-myapp-pgdata` is
        // what Docker was told. Someone who wants to keep a database
        // before saying yes has to be able to find it, and only the
        // second name does that.
        let report = purge_report();
        let text = render(&uninstall_plan(
            &host_plan(),
            Ok(&report),
            false,
            Decor::PLAIN,
        ));

        assert!(text.contains("storage"), "got:\n{text}");
        assert!(text.contains("minato-myapp-pgdata"), "got:\n{text}");
    }

    #[test]
    fn storage_that_could_not_be_listed_is_admitted_to() {
        // The dangerous shape: Docker is down, so the daemon finds no
        // volumes and the plan would read as a machine that has none —
        // right before removing everything else and exiting 0.
        let report = minato_api::PurgeReport {
            storage_left: vec![minato_api::PurgeStorageFailure {
                what: "docker".into(),
                reason: "its storage could not be listed: connection refused".into(),
            }],
            ..Default::default()
        };

        let text = render(&uninstall_plan(
            &crate::uninstall::Plan::default(),
            Ok(&report),
            false,
            Decor::PLAIN,
        ));

        assert!(!text.contains("nothing of Minato"), "got:\n{text}");
        assert!(text.contains("docker"), "got:\n{text}");
        assert!(text.contains("connection refused"), "got:\n{text}");
    }

    #[test]
    fn storage_on_its_own_is_not_nothing() {
        // The usual state by the time anyone uninstalls: the worktrees are
        // gone, so there are no containers, and the volumes they shared
        // are still there. Saying "nothing was found" and then deleting a
        // database is the failure this guards.
        let report = minato_api::PurgeReport {
            volumes: vec![minato_api::PurgeVolume {
                project: "myapp".into(),
                name: "minato-myapp-pgdata".into(),
            }],
            ..Default::default()
        };

        let text = render(&uninstall_plan(
            &crate::uninstall::Plan::default(),
            Ok(&report),
            false,
            Decor::PLAIN,
        ));

        assert!(!text.contains("nothing of Minato"), "got:\n{text}");
        assert!(text.contains("minato-myapp-pgdata"), "got:\n{text}");
    }

    #[test]
    fn the_plan_says_which_worktrees_survive() {
        // Someone uninstalling wants to know their branches did not go
        // with it, and silence does not answer that.
        let report = purge_report();
        let text = render(&uninstall_plan(
            &host_plan(),
            Ok(&report),
            false,
            Decor::PLAIN,
        ));

        assert!(text.contains("left alone"), "got:\n{text}");
        assert!(text.contains("myapp.wt/feat-1"), "got:\n{text}");
    }

    #[test]
    fn a_silent_daemon_is_admitted_to_rather_than_hidden() {
        // Without it the list looks complete when it is not, and someone
        // would think their containers had gone.
        let reason = "connection refused".to_string();
        let text = render(&uninstall_plan(
            &host_plan(),
            Err(&reason),
            false,
            Decor::PLAIN,
        ));
        assert!(text.contains("not in this list"), "got:\n{text}");
        assert!(text.contains("connection refused"), "got:\n{text}");
    }

    #[test]
    fn a_dry_run_says_how_to_go_ahead() {
        let report = purge_report();
        let text = render(&uninstall_plan(
            &host_plan(),
            Ok(&report),
            true,
            Decor::PLAIN,
        ));
        assert!(text.contains("minato uninstall"), "got:\n{text}");
    }

    #[test]
    fn finding_nothing_is_said_out_loud() {
        let empty = crate::uninstall::Plan::default();
        let reason = "it is not running".to_string();
        let text = render(&uninstall_plan(&empty, Err(&reason), false, Decor::PLAIN));
        assert!(text.contains("nothing of Minato"), "got:\n{text}");
    }

    #[test]
    fn what_could_not_be_removed_is_named() {
        let text = render(&uninstall_done(
            &["/usr/local/bin/minato: Permission denied".to_string()],
            &[],
            Decor::PLAIN,
        ));

        assert!(text.contains("could not remove"), "got:\n{text}");
        assert!(text.contains("Permission denied"), "got:\n{text}");
    }

    #[test]
    fn root_steps_that_did_not_run_are_handed_back() {
        // The machine is left in a state a person can finish by hand,
        // which is the same answer `minato setup` gives.
        let remaining = vec![crate::uninstall::Privileged {
            label: "stop the LaunchDaemon".into(),
            commands: vec!["sudo launchctl bootout system/dev.minato.daemon".into()],
        }];

        let text = render(&uninstall_done(&[], &remaining, Decor::PLAIN));
        assert!(text.contains("still to run, as root"), "got:\n{text}");
        assert!(text.contains("launchctl bootout"), "got:\n{text}");
    }

    #[test]
    fn a_status_check_is_valid_for_every_check_status() {
        for status in [CheckStatus::Ok, CheckStatus::Warn, CheckStatus::Fail] {
            assert!(!status.symbol().is_empty());
        }
    }
}

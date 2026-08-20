//! The views themselves.
//!
//! Each one turns a piece of the daemon's structured data into a
//! [`Panel`]. They hold no I/O and no decisions — the daemon has already
//! made those — so each is a pure function from a response to something
//! shaped like a screen, which is what makes them worth keeping when a
//! full-screen mode arrives.

use std::path::Path;

use kobune_api::{
    Check, ConfigInfo, Diagnostics, EnvInfo, Pong, ServiceInfo, TunnelAccess, TunnelInfo,
    TunnelState, Unsettled, UnsettledReason, WorkspaceInfo,
};
use ratatui::text::{Line, Span};

use super::panel::{Grid, Panel};
use super::theme::{self, Decor};

/// Shown against a service with nowhere to go.
const NO_ADDRESS: &str = "—";

/// A row somebody's cursor is on.
///
/// Only the full-screen mode has one; a printed panel passes `None` and
/// comes out exactly as it always did, without the column. It is a
/// display concern rather than a second view, which is why it lives here
/// and not in a copy of this function.
#[derive(Debug, Clone, Copy)]
pub struct Cursor<'a> {
    pub service: &'a str,
    /// Whether the keys are talking to this pane.
    ///
    /// A cursor in a pane that is not listening is still where the next
    /// `u` will act, so it is dimmed rather than taken away — the same
    /// thing being said about it as about the workspace list beside it.
    pub active: bool,
}

/// One workspace in full: `kobune status`, and what `up`, `down` and `new`
/// leave on the screen when they are done.
pub fn workspace(info: &WorkspaceInfo, cursor: Option<Cursor<'_>>, decor: Decor) -> Panel {
    let mut panel = Panel::new(decor, title(info)).lines(vec![Line::from(vec![
        Span::styled(info.branch.clone(), theme::secondary()),
        Span::styled("  ", theme::secondary()),
        Span::styled(display_path(&info.path), theme::secondary()),
    ])]);

    if info.services.is_empty() {
        return panel.line(Span::styled("no services are defined", theme::secondary()));
    }

    let mut services = Grid::new();
    for service in &info.services {
        let mut row = Vec::new();

        // A column that only exists when there is a cursor to put in it,
        // so nothing printed gains two spaces of indent it never had.
        if let Some(cursor) = cursor {
            row.push(cursor_marker(cursor, &service.name));
        }

        row.extend([
            service_name(service),
            Line::styled(service.state.label(), theme::service_state(&service.state)),
            access(service),
        ]);

        services.push(row);
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

/// `kobune url` with nothing named: every service, and the way in.
///
/// The whole list rather than the first reachable one. "Which URL" is the
/// question being asked, and answering it with one of several is how
/// somebody ends up curling the wrong service. Naming a service still
/// prints one bare line — see [`crate::present_url`].
pub fn urls(info: &WorkspaceInfo, qr: bool, decor: Decor) -> Panel {
    let mut grid = Grid::new();
    for service in &info.services {
        grid.push(vec![service_name(service), access(service)]);
    }

    let mut panel = Panel::new(decor, "urls").grid(grid);

    // The same second table `status` draws, for the same reason: a tunnel
    // URL is a different address to a different audience, and folding it
    // into the column above would leave two rows that differ by a domain.
    let mut shared = Grid::new().caption(Span::styled("shared over the tunnel:", theme::heading()));
    for service in &info.services {
        let Some(url) = &service.tunnel_url else {
            continue;
        };

        shared.push(vec![
            Line::styled(service.name.clone(), theme::subject()),
            Line::styled(url.clone(), theme::link()),
        ]);
    }
    panel = panel.grid(shared);

    if !qr {
        return panel;
    }

    for service in &info.services {
        panel = with_code(panel, service, decor);
    }

    panel
}

/// `kobune url <service> --qr`: one URL, drawn to be photographed.
pub fn url(service: &ServiceInfo, decor: Decor) -> Panel {
    with_code(Panel::new(decor, "url"), service, decor)
}

/// Adds a service's URL and the QR code for it.
///
/// **The tunnel URL wins when there is one.** A `.localhost` name resolves
/// through this machine's own resolver and nowhere else, so the phone that
/// just photographed it would get nothing — which is worth saying rather
/// than leaving somebody to find out with a camera in their hand.
///
/// Adds nothing for a service with no address at all, which is what lets
/// the listing offer a code for each without asking first.
fn with_code(panel: Panel, service: &ServiceInfo, decor: Decor) -> Panel {
    let Some(url) = service.tunnel_url.clone().or_else(|| service.access()) else {
        return panel;
    };

    let mut heading = vec![Line::from(vec![
        Span::styled(format!("{}  ", service.name), theme::subject()),
        Span::styled(url.clone(), theme::link()),
    ])];

    if service.tunnel_url.is_none() {
        heading.push(note(
            "only this machine resolves this name, so a phone will not",
        ));
    }

    // The glyphs describe the modules, but which of them is the *dark* one
    // is the terminal's own foreground once the styling is gone — and on a
    // dark theme that is an inverted code, which iOS' camera refuses.
    // Nothing to be done about it from here except say so.
    if !decor.styled {
        heading.push(note("drawn without colour, so a dark terminal inverts it"));
    }

    let Some(code) = super::qr::lines(&url) else {
        // Past 2,953 bytes, which no URL reaches. Said rather than
        // silently skipped: a missing code with no reason reads as a bug.
        heading.push(note("too long to draw as a QR code"));
        return panel.lines(heading);
    };

    // **Rigid, so a narrow window says so instead of shearing it.** The
    // rows are the same width all the way down; broken across two lines
    // each they still look like a QR code and scan as nothing at all.
    panel.rigid(
        heading,
        code,
        note("the window is too narrow to draw the QR code"),
    )
}

/// Every workspace: `kobune ls`.
///
/// The project column appears only when more than one is listed. With a
/// single project it is the same word on every row, and `ls` is something
/// people read at a glance.
pub fn workspaces(list: &[WorkspaceInfo], decor: Decor) -> Panel {
    if list.is_empty() {
        return Panel::new(decor, "workspaces")
            .line(Span::styled("none yet", theme::secondary()))
            .line(hint("create one with", "kobune new <branch>"));
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
            row.push(Line::styled(workspace.project.clone(), theme::secondary()));
        }
        row.extend([
            Line::styled(workspace.display_name().to_string(), theme::subject()),
            Line::styled(format!("{running}/{total}"), running_style(running, total)),
            Line::styled(workspace.branch.clone(), theme::secondary()),
        ]);

        grid.push(row);
    }

    Panel::new(decor, "workspaces").grid(grid)
}

/// `kobune doctor`: what was found, and what to do about it.
pub fn diagnostics(diagnostics: &Diagnostics, decor: Decor) -> Panel {
    let mut checks = Grid::new();
    for check in &diagnostics.checks {
        checks.push(vec![
            Line::styled(check.status.symbol(), theme::check_status(check.status)),
            Line::styled(check.title.clone(), theme::subject()),
            Line::styled(check.detail.clone(), theme::secondary()),
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
        .line(hint("or walk through all of it with", "kobune setup"))
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

/// One privileged step of `kobune setup`.
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

/// What became of one step of `kobune setup`.
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

/// `kobune setup` with nowhere to ask: it says what to run, and runs none
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
            .line(hint("confirm with", "kobune doctor"));
    }

    let mut panel = Panel::new(decor, "setup").line(Span::styled(
        "the URLs need the following. It requires root, so read each command first.",
        theme::secondary(),
    ));

    for (index, step) in steps.iter().enumerate() {
        let mut lines = vec![Line::from(vec![
            Span::styled(format!("{}. ", index + 1), theme::heading()),
            Span::styled(step.description.clone(), theme::subject()),
        ])];

        if let Some(note) = &step.note {
            lines.push(Line::styled(format!("   {note}"), theme::secondary()));
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
///
/// **`restart`, and it used to be `stop`.** Nothing here starts the daemon
/// again by itself: a clean exit is not restarted
/// (`KeepAlive { SuccessfulExit: false }`), and the job launchd started the
/// moment it was handed the plist exits cleanly too when it finds the
/// socket already owned. So stopping leaves the machine with no daemon at
/// all until something arrives on a port, which is a strange thing to leave
/// behind a command someone ran to make the URLs work.
///
/// `restart` starts one the way every other command does, and that asks
/// launchd first ([`kobune_client::Client::connect_or_spawn`]) — so what
/// comes back is the job, holding the ports and reading what these steps
/// changed.
fn restart_hint() -> Vec<Line<'static>> {
    vec![
        hint("afterwards run", kobune_core::launchd::RESTART_COMMAND),
        Line::styled(
            "  it comes back as launchd's job, with the new settings",
            theme::secondary(),
        ),
    ]
}

/// What an interactive `kobune setup` is about to walk through.
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
            theme::secondary(),
        ),
        Line::styled(
            "each one is shown before it is run, and nothing runs until you say so.",
            theme::secondary(),
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
        lines.push(Line::styled(format!("  {note}"), theme::secondary()));
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
        SetupOutcome::Skipped => ("–", "skipped", theme::secondary()),
        SetupOutcome::Failed => ("✗", "failed", theme::bad()),
    };

    Line::from(vec![
        Span::styled(format!("  {symbol} "), style),
        Span::styled(text, style),
    ])
}

/// Where an interactive `kobune setup` left the machine.
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
            theme::secondary(),
        ))
    };

    // Whatever was declined, or failed, gets the answer `kobune setup` has
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
                        _ => theme::secondary(),
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

/// `kobune env ls`.
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
            Some(format!("kobune env ls --service {service}"))
        }
        UnsettledReason::NeedsProxy => Some("kobune doctor".to_string()),
        _ => None,
    }
}

/// `kobune env ls`: the variables, and where each one came from.
///
/// `service` is whose environment this is, where it is one service's.
/// Without it two listings are structurally identical — which is why
/// [`kobune_api::Response::Env`] carries the name — and a reader looking
/// at a short list has no way to tell whether they asked the wrong
/// question or the answer is simply short.
pub fn env(entries: &[EnvInfo], service: Option<&str>, decor: Decor) -> Panel {
    let title = match service {
        Some(service) => Line::from(vec![
            Span::raw("environment"),
            Span::styled(" · ", theme::secondary()),
            Span::styled(service.to_string(), theme::subject()),
        ]),
        None => Line::raw("environment"),
    };

    if entries.is_empty() {
        return Panel::new(decor, title)
            .line(Span::styled("nothing is defined", theme::secondary()));
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
            value.push(Span::styled(format!(" → {source}"), theme::secondary()));
        }

        grid.push(vec![
            Line::styled(entry.key.clone(), theme::subject()),
            Line::styled(entry.scope.label(), theme::secondary()),
            Line::from(value),
        ]);
    }

    let mut panel = Panel::new(decor, title).grid(grid);

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

/// `kobune config show`: the layers in the order they were applied, and
/// what each of them settled.
///
/// **The paths are shown whether or not there was a file at them.** The
/// question this command answers is nearly always "why is my override not
/// applying", and the answer is nearly always that the file is somewhere
/// else — which a listing of only what was read cannot say.
pub fn config(info: &ConfigInfo, decor: Decor) -> Panel {
    let mut layers = Grid::new().header(vec!["LAYER".into(), "FILE".into(), "".into()]);

    for layer in &info.layers {
        let (note, style) = match layer.loaded {
            true => ("read", theme::good()),
            // Not a failure: two of the three layers are meant to be
            // missing most of the time.
            false => ("not found", theme::secondary()),
        };

        layers.push(vec![
            Line::styled(layer.layer.label(), theme::subject()),
            Line::styled(display_path(&layer.path), theme::secondary()),
            Line::styled(note, style),
        ]);
    }

    let mut panel = Panel::new(decor, "config").grid(layers);

    // **Under the layers, not above them.** The layers are the evidence
    // and this is the verdict, and a verdict printed before its evidence
    // reads as the listing having failed rather than the merge.
    if let Some(problem) = &info.problem {
        // Worded for the missing-file case too: two of the three paths
        // above are routinely not read at all, and "the files above were
        // read" is untrue exactly when the project file is the missing one.
        panel = panel
            .lines(warning_block(problem))
            .line(note("what the layers above merge to is what failed"));
    }

    // `--all` fills in `values`; without it only the contested keys come
    // back, which is the question the command exists to answer.
    let (rows, caption) = match info.all {
        true => (&info.values, "every key, and the layer that settled it"),
        false => (&info.overrides, "keys one layer took from another"),
    };

    if rows.is_empty() {
        // Said in words. An empty table under the layers reads as a
        // listing that broke, rather than as nothing to report.
        //
        // Not when something already failed: "every value comes from one
        // layer alone" beneath a warning reads as reassurance, and there
        // is nothing here to be reassured about.
        if info.problem.is_some() {
            return panel;
        }

        return panel.line(note("every value comes from one layer alone"));
    }

    let mut values = Grid::new()
        .caption(Span::styled(caption, theme::secondary()))
        .header(vec![
            "KEY".into(),
            "LAYER".into(),
            "VALUE".into(),
            "OVER".into(),
        ]);

    for row in rows {
        let overridden: Vec<&str> = row.overridden.iter().map(|layer| layer.label()).collect();

        values.push(vec![
            Line::styled(row.key.clone(), theme::subject()),
            Line::styled(row.layer.label(), theme::secondary()),
            Line::raw(row.value.clone()),
            Line::styled(overridden.join(", "), theme::secondary()),
        ]);
    }

    panel.grid(values)
}

/// `kobune tunnel status`, and where `enable` and `disable` leave things.
pub fn tunnel(info: &TunnelInfo, decor: Decor) -> Panel {
    let state = Span::styled(info.state.label(), tunnel_style(info.state));

    // **Which service is carrying it**, now that there is more than one
    // to be. Everything else on this panel means something different
    // depending on the answer — a `stopped` quick tunnel has taken its
    // URLs with it, a stopped named one has not — and a domain is only
    // some of them.
    let mut heading = vec![
        state,
        Span::styled(format!("  {}", info.provider), theme::secondary()),
    ];

    if let Some(domain) = &info.domain {
        heading.push(Span::styled(format!("  *.{domain}"), theme::link()));
    }

    let mut panel = Panel::new(decor, "tunnel").line(Line::from(heading));

    if let Some(record) = &info.record {
        panel = panel.line(Line::from(vec![
            Span::styled("DNS  ", theme::secondary()),
            Span::styled(record.clone(), theme::link()),
        ]));
    }

    // What just changed about the zone. Above the standing warning below,
    // because this one is about the run that just happened.
    //
    // Through `warning`, so it carries `!` as well as the colour — the
    // shape is what a `NO_COLOR` terminal has left.
    if !info.notes.is_empty() {
        panel = panel.lines(info.notes.iter().map(|note| warning(note)).collect());
    }

    // Being on the internet unauthenticated is the kind of thing that has
    // to be said out loud every time, not buried in the setup output.
    //
    // **Yellow, not red.** `tunnel enable --public` prints this on the way
    // out of a command that worked, and red against a ✓ has been read as
    // the command having failed. Red is reserved for something that did.
    //
    // The second line is worded here rather than sent down the wire: the
    // daemon reports which of the three cases it is, and how to say it is
    // this screen's business (`docs/DESIGN.md` §3).
    let unguarded = info
        .state
        .is_running()
        .then(|| access_note(info.access))
        .flatten()
        .filter(|_| info.public);

    if let Some(detail) = unguarded {
        panel = panel.lines(vec![
            warning("this environment is reachable from the internet."),
            Line::styled(detail, theme::secondary()),
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
            Line::styled(label.to_string(), theme::secondary()),
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

    Panel::new(decor, "kobuned")
        .line(Span::styled("running", theme::good()))
        .grid(grid)
}

/// The daemon, when there is none.
pub fn daemon_stopped(decor: Decor) -> Panel {
    Panel::new(decor, "kobuned")
        .line(Span::styled("stopped", theme::secondary()))
        .line(hint("start it with", "kobune daemon start"))
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
            Line::styled(*label, theme::secondary()),
            Line::raw(value.clone()),
        ]);
    }

    Panel::new(decor, title).grid(grid).lines(next)
}

/// What `kobune uninstall` is about to do, before it does any of it.
///
/// The containers come from the daemon, the rest from looking at the
/// machine. Worktrees get a section of their own because they are the one
/// thing here that is **not** going, and that has to be as visible as what
/// is.
pub fn uninstall_plan(
    plan: &crate::uninstall::Plan,
    daemon: Result<&kobune_api::PurgeReport, &String>,
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
                            theme::secondary(),
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
                Line::styled(volume.project.clone(), theme::secondary()),
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
                Span::styled(failure.reason.clone(), theme::secondary()),
            ]));
        }
        panel = panel.lines(lines);
    }

    // Said out loud, and with the reason. A list that silently leaves the
    // containers out looks like a machine that has none.
    if let Err(reason) = daemon {
        panel = panel.lines(vec![
            warning("the daemon's containers and storage are not in this list:"),
            Line::styled(format!("  {reason}"), theme::secondary()),
        ]);
    }

    if !plan.files.is_empty() {
        let mut grid = Grid::new().caption(Span::styled("files:", theme::heading()));
        for removal in &plan.files {
            grid.push(vec![
                Line::styled(removal.label, theme::secondary()),
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

    // What stays in the account of whatever service carried the tunnel.
    // Silence here would read as "there is nothing left", and there is —
    // the daemon leaves this out entirely when there is not.
    if let Some(tunnel) = daemon.ok().and_then(|report| report.tunnel.as_ref()) {
        let mut lines = vec![Line::styled(
            match &tunnel.domain {
                Some(domain) => format!("left in your account (*.{domain}):"),
                None => "left in your account:".to_string(),
            },
            theme::heading(),
        )];
        lines.extend(
            tunnel
                .commands
                .iter()
                .map(|command| Line::styled(format!("  {command}"), theme::command())),
        );
        // Not styled as commands: these are things to go and do, and a
        // line that looks runnable but is not wastes the reader's attempt.
        lines.extend(
            tunnel
                .notes
                .iter()
                .map(|note| Line::styled(format!("  {note}"), theme::secondary())),
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
                .map(|path| Line::styled(format!("  {}", display_path(path)), theme::secondary())),
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
            "nothing of Kobune's was found on this machine",
            theme::secondary(),
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
                Span::styled(failure.reason.clone(), theme::secondary()),
            ]));
        }
        panel = panel.lines(lines);
    }

    if dry_run {
        panel = panel.line(hint("to go ahead, run", "kobune uninstall"));
    }

    panel
}

/// What `kobune uninstall` managed, and what it did not.
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
                .map(|failure| Line::styled(format!("  {failure}"), theme::secondary())),
        );
        panel.lines(lines)
    };

    if remaining.is_empty() {
        return panel;
    }

    // Reached when there was no terminal to type a password into, or sudo
    // said no. Printing the commands is the same answer `kobune setup`
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
        Span::styled("› ", theme::secondary()),
        Span::styled(text.to_string(), theme::secondary()),
    ])
}

/// Something that worked and is worth being careful about.
///
/// **The symbol carries it as much as the colour does**, the same bargain
/// the service states make: a monochrome terminal, `NO_COLOR`, and a
/// reader who cannot tell red from yellow all still get the emphasis. And
/// a shape of its own is what keeps a warning from being read as an error
/// — see [`super::error`], which is the one that gets `✗` and red.
pub fn warning(text: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("! ", theme::warn()),
        Span::styled(text.to_string(), theme::warn()),
    ])
}

/// A warning that arrived as more than one line.
///
/// **Only the first carries the `!`.** A message its formatter split — a
/// `toml` error puts the field on one line and the table it was in on the
/// next — is one complaint, and a marker per line would present it as
/// several.
fn warning_block(text: &str) -> Vec<Line<'static>> {
    let mut parts = text.lines();
    let mut out = vec![warning(parts.next().unwrap_or_default())];

    out.extend(parts.map(|part| Line::raw(format!("  {part}"))));
    out
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
        Span::styled(" / ", theme::secondary()),
        Span::styled(info.display_name().to_string(), theme::subject()),
    ])
}

/// The mark against the row a cursor is on, or the space it occupies.
fn cursor_marker(cursor: Cursor<'_>, service: &str) -> Line<'static> {
    if cursor.service != service {
        return Line::raw(" ");
    }

    let style = if cursor.active {
        theme::good()
    } else {
        theme::secondary()
    };

    Line::styled("▸", style)
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
        None if service.state.is_running() => Line::styled("internal only", theme::secondary()),
        None => Line::styled(NO_ADDRESS, theme::secondary()),
    }
}

fn running_style(running: usize, total: usize) -> ratatui::style::Style {
    if total == 0 || running == 0 {
        theme::secondary()
    } else if running == total {
        theme::good()
    } else {
        theme::warn()
    }
}

/// What to say under "reachable from the internet", if anything.
///
/// **The three cases are not the same warning.** "Kobune cannot see a
/// policy" invites you to go and check one is there; on a hostname the
/// service handed out there is nothing to check and nothing you could
/// have put there, and printing the first sentence would send somebody
/// looking for a dashboard page that does not apply to them.
fn access_note(access: TunnelAccess) -> Option<&'static str> {
    match access {
        TunnelAccess::Unknown => {
            Some("Kobune cannot see whether an access policy is in front of it.")
        }
        TunnelAccess::Open => Some("There is no access control: anyone with the URL reaches it."),
        // Nothing is unguarded, so there is nothing to warn about.
        TunnelAccess::Managed => None,
    }
}

fn tunnel_style(state: TunnelState) -> ratatui::style::Style {
    match state {
        TunnelState::Running => theme::good(),
        TunnelState::NeedsLogin | TunnelState::Stopped => theme::warn(),
        TunnelState::NotInstalled => theme::bad(),
        TunnelState::Disabled => theme::secondary(),
    }
}

/// `3d 4h`, `12m`, `8s` — the daemon's uptime at the precision anyone
/// reads it at.
pub fn format_uptime(seconds: u64) -> String {
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
    use kobune_api::CheckStatus;
    use kobune_core::ServiceState;
    use kobune_core::{EnvScope, ServiceScope};
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
        let text = render(&workspace(&info(vec![]), None, Decor::PLAIN));

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
            None,
            Decor::FRAMED,
        ));

        assert!(text.contains(url), "got:\n{text}");
    }

    #[test]
    fn a_running_service_with_no_way_in_says_so() {
        // A blank there looks like something failed to be filled in.
        let text = render(&workspace(
            &info(vec![service("db", ServiceState::Ready, None)]),
            None,
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
            None,
            Decor::PLAIN,
        ));

        assert!(text.contains("port 3000 is in use"), "got:\n{text}");
    }

    #[test]
    fn a_workspace_with_no_services_says_so_rather_than_showing_nothing() {
        let text = render(&workspace(&info(vec![]), None, Decor::PLAIN));
        assert!(text.contains("no services are defined"), "got:\n{text}");
    }

    #[test]
    fn the_listing_names_every_service_and_its_url() {
        let text = render(&urls(
            &info(vec![
                service("web", ServiceState::Ready, Some("https://web.localhost")),
                service("api", ServiceState::Ready, Some("https://api.localhost")),
            ]),
            false,
            Decor::PLAIN,
        ));

        assert!(text.contains("web"), "got:\n{text}");
        assert!(text.contains("https://web.localhost"), "got:\n{text}");
        assert!(text.contains("api"), "got:\n{text}");
        assert!(text.contains("https://api.localhost"), "got:\n{text}");
    }

    #[test]
    fn the_listing_keeps_a_service_with_nowhere_to_go() {
        // Left out, this reads as a workspace that does not define it —
        // and "where is my database" is a question the listing should not
        // answer with silence.
        let text = render(&urls(
            &info(vec![
                service("web", ServiceState::Ready, Some("https://web.localhost")),
                service("db", ServiceState::Ready, None),
            ]),
            false,
            Decor::PLAIN,
        ));

        assert!(text.contains("db"), "got:\n{text}");
        assert!(text.contains("internal only"), "got:\n{text}");
    }

    #[test]
    fn the_listing_shows_the_tunnel_url_beside_the_local_one() {
        let mut web = service("web", ServiceState::Ready, Some("https://web.localhost"));
        web.tunnel_url = Some("https://web.myapp.example.com".into());

        let text = render(&urls(&info(vec![web]), false, Decor::PLAIN));

        assert!(text.contains("shared over the tunnel"), "got:\n{text}");
        assert!(
            text.contains("https://web.myapp.example.com"),
            "got:\n{text}"
        );
        assert!(text.contains("https://web.localhost"), "got:\n{text}");
    }

    #[test]
    fn no_code_is_drawn_unless_it_was_asked_for() {
        let text = render(&urls(
            &info(vec![service(
                "web",
                ServiceState::Ready,
                Some("https://web.localhost"),
            )]),
            false,
            Decor::PLAIN,
        ));

        assert!(!text.contains('█'), "got:\n{text}");
    }

    #[test]
    fn a_code_is_drawn_for_each_service_that_has_a_url() {
        let text = render(&urls(
            &info(vec![
                service("web", ServiceState::Ready, Some("https://web.localhost")),
                service("api", ServiceState::Ready, Some("https://api.localhost")),
                service("db", ServiceState::Ready, None),
            ]),
            true,
            Decor::PLAIN,
        ));

        // Three finder patterns to a code — top left, top right, bottom
        // left — and the top of each pairs to this. Two codes, and none
        // for the service with no address.
        let finders = text.matches("█▀▀▀▀▀█").count();
        assert_eq!(finders, 6, "three finders per code, two codes:\n{text}");
    }

    #[test]
    fn the_code_carries_the_tunnel_url_when_there_is_one() {
        // A `.localhost` name is not one a phone can resolve, and the
        // camera is the whole reason the code is being drawn.
        let mut web = service("web", ServiceState::Ready, Some("https://web.localhost"));
        web.tunnel_url = Some("https://web.myapp.example.com".into());

        let text = render(&url(&web, Decor::PLAIN));

        assert!(
            text.contains("https://web.myapp.example.com"),
            "got:\n{text}"
        );
        assert!(
            !text.contains("only this machine resolves"),
            "the tunnel URL resolves anywhere:\n{text}"
        );
    }

    #[test]
    fn a_local_url_says_a_phone_will_not_reach_it() {
        // Otherwise this is found out with a camera already in hand.
        let text = render(&url(
            &service("web", ServiceState::Ready, Some("https://web.localhost")),
            Decor::PLAIN,
        ));

        assert!(text.contains("only this machine resolves"), "got:\n{text}");
    }

    #[test]
    fn a_service_with_no_address_has_no_code() {
        let text = render(&url(
            &service("db", ServiceState::Ready, None),
            Decor::PLAIN,
        ));

        assert!(!text.contains('█'), "got:\n{text}");
    }

    #[test]
    fn a_narrow_window_says_so_rather_than_shearing_the_code() {
        // Wrapped at the column, the rows still look like a QR code and
        // scan as nothing — which is the one failure that costs somebody a
        // minute of pointing a camera at it. The URL survives: it is the
        // half still worth reading in a window with no room for the rest.
        let view = url(
            &service("web", ServiceState::Ready, Some("https://web.localhost")),
            Decor::PLAIN,
        );

        let text = crate::ui::test_support::render_at(&view, 24);

        // The message itself wraps like any other line at this width, so
        // the assertion is about it reaching the screen, not where it
        // broke.
        let unwrapped = text.replace('\n', "");
        assert!(unwrapped.contains("too narrow to draw"), "got:\n{text}");
        assert!(unwrapped.contains("https://web.localhost"), "got:\n{text}");
        assert!(!text.contains('█'), "no half-drawn code:\n{text}");
    }

    #[test]
    fn a_window_with_the_room_draws_it_whole() {
        let view = url(
            &service("web", ServiceState::Ready, Some("https://web.localhost")),
            Decor::PLAIN,
        );

        let text = render(&view);

        assert!(!text.contains("too narrow"), "got:\n{text}");
        assert!(text.contains("█▀▀▀▀▀█"), "the finder pattern:\n{text}");
    }

    #[test]
    fn a_code_with_no_colour_behind_it_says_what_that_costs() {
        // The glyphs say which modules are set; the colours say which way
        // round. Without them a dark terminal draws an inverted code, and
        // somebody photographing it deserves to know why nothing happens.
        let unstyled = render(&url(
            &service("web", ServiceState::Ready, Some("https://web.localhost")),
            Decor::PLAIN,
        ));
        assert!(unstyled.contains("dark terminal inverts it"), "{unstyled}");

        let styled = render(&url(
            &service("web", ServiceState::Ready, Some("https://web.localhost")),
            Decor::FRAMED,
        ));
        assert!(
            !styled.contains("dark terminal inverts it"),
            "nothing to warn about when the colour reaches:\n{styled}"
        );
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
        assert!(text.contains("kobune new <branch>"), "got:\n{text}");
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
                    .with_fix("sudo kobune setup"),
            ]),
            Decor::PLAIN,
        ));

        assert!(text.contains("Docker"), "got:\n{text}");
        assert!(text.contains("sudo kobune setup"), "got:\n{text}");
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
                description: "point *.localhost at Kobune".into(),
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
        assert!(text.contains("kobune daemon restart"), "got:\n{text}");
    }

    #[test]
    fn a_plan_that_restarts_the_daemon_itself_does_not_ask_for_it_again() {
        // Waking launchd's job stops the daemon on the way. Being told to
        // restart it afterwards would take down what had just come up.
        let steps = vec![SetupStep {
            description: "wake launchd's job".into(),
            note: None,
            commands: vec![
                "kobune daemon stop".into(),
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

    /// Two steps, the first with a note, as `kobune setup` builds them.
    fn setup_steps() -> Vec<SetupStep> {
        vec![
            SetupStep {
                description: "let launchd hold the ports".into(),
                note: Some("generated plist: /tmp/x.plist".into()),
                commands: vec!["sudo cp /tmp/x.plist /Library/LaunchDaemons/".into()],
            },
            SetupStep {
                description: "point *.localhost at Kobune".into(),
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
        assert!(text.contains("kobune daemon restart"), "got:\n{text}");
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
        assert!(!text.contains("kobune daemon restart"), "got:\n{text}");
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
        assert!(text.contains("kobune daemon restart"), "got:\n{text}");
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

        let text = render(&env(&entries, None, Decor::PLAIN));

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
                value: "${KOBUNE_URL_API}".into(),
                scope: EnvScope::Service,
                secret: false,
                source: None,
                unsettled: Some(Unsettled {
                    reference: Some("KOBUNE_URL_API".into()),
                    reason: UnsettledReason::NeedsProxy,
                }),
            },
        ]
    }

    #[test]
    fn a_listing_that_could_not_settle_still_lists() {
        // This is the tool for finding the value at fault, so it has to
        // arrive — saying so alongside, not instead of it.
        let text = render(&env(&mixed_entries(), None, Decor::PLAIN));

        assert!(text.contains("API_URL"), "the listing arrives:\n{text}");
        assert!(
            text.contains("the proxy is not listening"),
            "and says what is wrong:\n{text}"
        );
        assert!(
            text.contains("kobune doctor"),
            "and what to do about it:\n{text}"
        );
    }

    #[test]
    fn only_the_value_at_fault_is_spoken_for() {
        // One bad reference used to put every other value under
        // suspicion, which left nothing to tell them apart by.
        let text = render(&env(&mixed_entries(), None, Decor::PLAIN));

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
        let text =
            super::super::test_support::render_at(&env(&mixed_entries(), None, Decor::PLAIN), 40);

        assert!(text.contains("API_URL"), "got:\n{text}");
        assert!(text.contains("proxy"), "got:\n{text}");
    }

    #[test]
    fn one_long_reason_does_not_stretch_the_frame() {
        use super::super::View;

        // The note is a line like any other, and `preferred_width` takes
        // the widest — so a sentence left whole would drag the frame away
        // from the three short columns the listing is made of.
        let narrow = env(&mixed_entries(), None, Decor::PLAIN).preferred_width();

        let mut chained = mixed_entries();
        chained[1].unsettled = Some(Unsettled {
            reference: None,
            reason: UnsettledReason::Cycle {
                chain: vec!["A".into(), "B".into(), "A".into()],
            },
        });

        assert!(
            env(&chained, None, Decor::PLAIN).preferred_width() <= narrow + 8,
            "a reason should not decide the width of the listing"
        );
    }

    #[test]
    fn a_public_tunnel_says_so_every_time() {
        let text = render(&tunnel(
            &TunnelInfo {
                state: TunnelState::Running,
                provider: kobune_core::DEFAULT_TUNNEL_PROVIDER.into(),
                domain: Some("example.com".into()),
                record: None,
                setup: vec![],
                notes: vec![],
                public: true,
                access: TunnelAccess::Unknown,
            },
            Decor::PLAIN,
        ));

        assert!(text.contains("reachable from the internet"), "got:\n{text}");
        assert!(text.contains("*.example.com"), "got:\n{text}");
        // The mark that survives `NO_COLOR` and a monochrome terminal.
        assert!(text.contains("! this environment"), "got:\n{text}");
    }

    #[test]
    fn a_zone_note_is_marked_like_any_other_warning() {
        // It arrives from the daemon as prose, and reaches the reader the
        // same way the standing warning does — `!` first, so a monochrome
        // terminal keeps the emphasis.
        let text = render(&tunnel(
            &TunnelInfo {
                state: TunnelState::Running,
                provider: kobune_core::DEFAULT_TUNNEL_PROVIDER.into(),
                domain: Some("example.com".into()),
                record: Some("*.example.com".into()),
                setup: vec![],
                notes: vec!["*.example.com now points here.".into()],
                public: true,
                access: TunnelAccess::Unknown,
            },
            Decor::PLAIN,
        ));

        assert!(
            text.contains("! *.example.com now points here."),
            "got:\n{text}"
        );
    }

    #[test]
    fn a_warning_is_not_dressed_as_a_failure() {
        // `tunnel enable --public` prints its warning on the way out of a
        // command that worked, and this one was read as the command
        // having failed. Red belongs to the things that did fail.
        let line = warning("this environment is reachable from the internet.");

        for span in &line.spans {
            assert_eq!(span.style, theme::warn(), "got: {span:?}");
            assert_ne!(span.style, theme::bad(), "got: {span:?}");
        }
    }

    #[test]
    fn an_open_tunnel_is_not_told_to_check_for_a_policy() {
        // There is nothing to check. The hostname belongs to the service,
        // so "Kobune cannot see whether a policy is in front of it" would
        // imply one could be.
        let text = render(&tunnel(
            &TunnelInfo {
                state: TunnelState::Running,
                provider: "quick".into(),
                domain: None,
                record: None,
                setup: vec![],
                notes: vec![],
                public: true,
                access: TunnelAccess::Open,
            },
            Decor::PLAIN,
        ));

        assert!(text.contains("reachable from the internet"), "got:\n{text}");
        assert!(text.contains("no access control"), "got:\n{text}");
        assert!(!text.contains("cannot see whether"), "got:\n{text}");
    }

    #[test]
    fn a_guarded_tunnel_does_not_warn_about_being_unguarded() {
        // The one case where being on the internet is not news.
        assert_eq!(access_note(TunnelAccess::Managed), None);
    }

    #[test]
    fn a_tunnel_says_which_service_carries_it() {
        // With two providers, `running` on its own does not say whether
        // the URLs survive a restart. A quick tunnel's do not.
        let text = render(&tunnel(
            &TunnelInfo {
                state: TunnelState::Running,
                provider: "quick".into(),
                domain: None,
                record: None,
                setup: vec![],
                notes: vec![],
                public: true,
                access: TunnelAccess::Unknown,
            },
            Decor::PLAIN,
        ));

        assert!(text.contains("quick"), "got:\n{text}");
        // No zone was involved, so nothing claims one.
        assert!(!text.contains("*."), "got:\n{text}");
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
                provider: kobune_core::DEFAULT_TUNNEL_PROVIDER.into(),
                domain: None,
                record: None,
                setup: vec!["cloudflared tunnel login".into()],
                notes: vec![],
                public: false,
                access: TunnelAccess::Unknown,
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
        assert!(text.contains("kobune daemon start"), "got:\n{text}");
    }

    fn purge_report() -> kobune_api::PurgeReport {
        kobune_api::PurgeReport {
            dry_run: true,
            projects: vec![kobune_api::PurgeProject {
                name: "myapp".into(),
                workspaces: vec![kobune_api::PurgeWorkspace {
                    label: "feat-1".into(),
                    services: vec!["web".into(), "db".into()],
                }],
            }],
            worktrees: vec![PathBuf::from("/repo/myapp.wt/feat-1")],
            volumes: vec![kobune_api::PurgeVolume {
                project: "myapp".into(),
                name: "kobune-myapp-pgdata".into(),
            }],
            ..Default::default()
        }
    }

    fn host_plan() -> crate::uninstall::Plan {
        crate::uninstall::Plan {
            files: vec![crate::uninstall::Removal {
                label: "state, logs and the local CA",
                path: PathBuf::from("/home/u/.kobune"),
            }],
            privileged: vec![crate::uninstall::Privileged {
                label: "stop trusting the local CA".into(),
                commands: vec![
                    "sudo security remove-trusted-cert -d /home/u/.kobune/ca.crt".into(),
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
        assert!(text.contains(".kobune"), "files:\n{text}");
        assert!(text.contains("remove-trusted-cert"), "root steps:\n{text}");
        assert!(text.contains("kobune-myapp-pgdata"), "storage:\n{text}");
    }

    #[test]
    fn storage_is_named_by_what_the_runtime_calls_it() {
        // `pgdata` is what `kobune.toml` says; `kobune-myapp-pgdata` is
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
        assert!(text.contains("kobune-myapp-pgdata"), "got:\n{text}");
    }

    #[test]
    fn storage_that_could_not_be_listed_is_admitted_to() {
        // The dangerous shape: Docker is down, so the daemon finds no
        // volumes and the plan would read as a machine that has none —
        // right before removing everything else and exiting 0.
        let report = kobune_api::PurgeReport {
            storage_left: vec![kobune_api::PurgeStorageFailure {
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

        assert!(!text.contains("nothing of Kobune"), "got:\n{text}");
        assert!(text.contains("docker"), "got:\n{text}");
        assert!(text.contains("connection refused"), "got:\n{text}");
    }

    #[test]
    fn storage_on_its_own_is_not_nothing() {
        // The usual state by the time anyone uninstalls: the worktrees are
        // gone, so there are no containers, and the volumes they shared
        // are still there. Saying "nothing was found" and then deleting a
        // database is the failure this guards.
        let report = kobune_api::PurgeReport {
            volumes: vec![kobune_api::PurgeVolume {
                project: "myapp".into(),
                name: "kobune-myapp-pgdata".into(),
            }],
            ..Default::default()
        };

        let text = render(&uninstall_plan(
            &crate::uninstall::Plan::default(),
            Ok(&report),
            false,
            Decor::PLAIN,
        ));

        assert!(!text.contains("nothing of Kobune"), "got:\n{text}");
        assert!(text.contains("kobune-myapp-pgdata"), "got:\n{text}");
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
        assert!(text.contains("kobune uninstall"), "got:\n{text}");
    }

    #[test]
    fn finding_nothing_is_said_out_loud() {
        let empty = crate::uninstall::Plan::default();
        let reason = "it is not running".to_string();
        let text = render(&uninstall_plan(&empty, Err(&reason), false, Decor::PLAIN));
        assert!(text.contains("nothing of Kobune"), "got:\n{text}");
    }

    #[test]
    fn what_could_not_be_removed_is_named() {
        let text = render(&uninstall_done(
            &["/usr/local/bin/kobune: Permission denied".to_string()],
            &[],
            Decor::PLAIN,
        ));

        assert!(text.contains("could not remove"), "got:\n{text}");
        assert!(text.contains("Permission denied"), "got:\n{text}");
    }

    #[test]
    fn root_steps_that_did_not_run_are_handed_back() {
        // The machine is left in a state a person can finish by hand,
        // which is the same answer `kobune setup` gives.
        let remaining = vec![crate::uninstall::Privileged {
            label: "stop the LaunchDaemon".into(),
            commands: vec!["sudo launchctl bootout system/dev.kobune.daemon".into()],
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

    fn layers(local_loaded: bool) -> Vec<kobune_api::ConfigLayerInfo> {
        vec![
            kobune_api::ConfigLayerInfo {
                layer: kobune_core::ConfigLayer::Global,
                path: PathBuf::from("/home/someone/.kobune/config.toml"),
                loaded: true,
            },
            kobune_api::ConfigLayerInfo {
                layer: kobune_core::ConfigLayer::Project,
                path: PathBuf::from("/repo/kobune.toml"),
                loaded: true,
            },
            kobune_api::ConfigLayerInfo {
                layer: kobune_core::ConfigLayer::Local,
                path: PathBuf::from("/repo/kobune.local.toml"),
                loaded: local_loaded,
            },
        ]
    }

    #[test]
    fn the_layers_are_listed_in_the_order_they_are_applied() {
        let text = render(&config(
            &ConfigInfo {
                layers: layers(true),
                overrides: vec![],
                values: vec![],
                all: false,
                problem: None,
            },
            Decor::PLAIN,
        ));

        let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("got:\n{text}"));
        assert!(at("global") < at("project"), "got:\n{text}");
        assert!(at("project") < at("local"), "got:\n{text}");
    }

    #[test]
    fn a_layer_with_no_file_still_shows_the_path_it_looked_at() {
        // "My override is not applying" is nearly always "the file is
        // somewhere else", which only the path can answer.
        let text = render(&config(
            &ConfigInfo {
                layers: layers(false),
                overrides: vec![],
                values: vec![],
                all: false,
                problem: None,
            },
            Decor::PLAIN,
        ));

        assert!(text.contains("kobune.local.toml"), "got:\n{text}");
        assert!(text.contains("not found"), "got:\n{text}");
    }

    #[test]
    fn an_overridden_key_names_the_layers_it_beat() {
        let text = render(&config(
            &ConfigInfo {
                layers: layers(true),
                overrides: vec![kobune_api::ConfigValueInfo {
                    key: "runtime.default".into(),
                    value: "apple".into(),
                    layer: kobune_core::ConfigLayer::Local,
                    overridden: vec![kobune_core::ConfigLayer::Project],
                }],
                values: vec![],
                all: false,
                problem: None,
            },
            Decor::PLAIN,
        ));

        assert!(text.contains("runtime.default"), "got:\n{text}");
        assert!(text.contains("apple"), "got:\n{text}");
        assert!(text.contains("project"), "got:\n{text}");
    }

    #[test]
    fn nothing_contested_is_said_rather_than_left_blank() {
        // An empty table under the layers reads as a listing that broke.
        let text = render(&config(
            &ConfigInfo {
                layers: layers(false),
                overrides: vec![],
                values: vec![],
                all: false,
                problem: None,
            },
            Decor::PLAIN,
        ));

        assert!(text.contains("one layer alone"), "got:\n{text}");
    }

    #[test]
    fn all_is_stated_rather_than_read_off_an_empty_list() {
        // `--all` against a configuration with nothing in `origins` used
        // to fall through to the contested-keys wording, which answers a
        // question nobody asked.
        let text = render(&config(
            &ConfigInfo {
                layers: layers(true),
                overrides: vec![],
                values: vec![],
                all: true,
                problem: None,
            },
            Decor::PLAIN,
        ));

        assert!(
            !text.contains("one layer took from another"),
            "got:\n{text}"
        );
    }

    #[test]
    fn a_merge_that_will_not_load_still_lists_its_layers() {
        // The case this command exists for. Failing here would leave the
        // person with the same message they already had and nothing to
        // read it against.
        let text = render(&config(
            &ConfigInfo {
                layers: layers(true),
                overrides: vec![],
                values: vec![],
                all: false,
                problem: Some("unknown field `defalut`, expected `default`\nin `runtime`".into()),
            },
            Decor::PLAIN,
        ));

        assert!(text.contains("defalut"), "got:\n{text}");
        assert!(text.contains("kobune.local.toml"), "got:\n{text}");
        assert!(
            !text.contains("one layer alone"),
            "no reassurance under a warning:\n{text}"
        );

        // One complaint, so one marker. A `!` per line would present a
        // message its formatter split as several separate problems.
        assert_eq!(
            text.lines().filter(|line| line.contains('!')).count(),
            1,
            "got:\n{text}"
        );
    }
}

//! The views themselves.
//!
//! Each one turns a piece of the daemon's structured data into a
//! [`Panel`]. They hold no I/O and no decisions — the daemon has already
//! made those — so each is a pure function from a response to something
//! shaped like a screen, which is what makes them worth keeping when a
//! full-screen mode arrives.

use std::path::Path;

use minato_api::{
    Check, Diagnostics, EnvInfo, Pong, ServiceInfo, TunnelInfo, TunnelState, WorkspaceInfo,
};
use minato_core::ServiceState;
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
            .filter_map(|service| match &service.state {
                ServiceState::Failed { reason } => Some(Line::from(vec![
                    Span::styled(
                        format!("{} ", theme::service_symbol(&service.state)),
                        theme::bad(),
                    ),
                    Span::styled(format!("{}: ", service.name), theme::subject()),
                    Span::styled(reason.clone(), theme::bad()),
                ])),
                _ => None,
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

/// `minato setup`. **It never runs any of this** — it says what to run.
pub fn setup(steps: &[SetupStep], undo: &[String], decor: Decor) -> Panel {
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

    panel = panel.lines(vec![
        hint("afterwards run", "minato daemon stop"),
        Line::styled(
            "  launchd starts it again, with the new settings",
            theme::muted(),
        ),
    ]);

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

/// `minato env ls`.
///
/// **Each value says which layer defined it.** With three layers, not
/// seeing that an unintended one is winning makes the cause impossible to
/// find.
pub fn env(entries: &[EnvInfo], decor: Decor) -> Panel {
    if entries.is_empty() {
        return Panel::new(decor, "environment")
            .line(Span::styled("nothing is defined", theme::muted()));
    }

    let mut grid = Grid::new().header(vec!["KEY".into(), "SCOPE".into(), "VALUE".into()]);

    for entry in entries {
        let mut value = vec![Span::styled(
            entry.value.clone(),
            if entry.secret {
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

    Panel::new(decor, "environment").grid(grid)
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
    use minato_core::{EnvScope, ServiceScope};
    use std::path::PathBuf;

    fn service(name: &str, state: ServiceState, url: Option<&str>) -> ServiceInfo {
        ServiceInfo {
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

        let text = render(&setup(&steps, &["sudo rm /Library/x".into()], Decor::PLAIN));

        assert!(text.contains("1. let launchd"), "got:\n{text}");
        assert!(text.contains("2. point"), "got:\n{text}");
        assert!(text.contains("/tmp/x.plist"), "got:\n{text}");
        assert!(text.contains("to undo:"), "got:\n{text}");
    }

    #[test]
    fn nothing_left_to_set_up_is_not_an_empty_screen() {
        let text = render(&setup(&[], &[], Decor::PLAIN));
        assert!(text.contains("everything is set up"), "got:\n{text}");
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
            },
            EnvInfo {
                key: "API_KEY".into(),
                value: "••••".into(),
                scope: EnvScope::Workspace,
                secret: true,
                source: Some("1password://vault/item".into()),
            },
        ];

        let text = render(&env(&entries, Decor::PLAIN));

        assert!(text.contains("injected"), "got:\n{text}");
        assert!(text.contains("workspace"), "got:\n{text}");
        // The reference, never the value behind it.
        assert!(text.contains("1password://vault/item"), "got:\n{text}");
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

    #[test]
    fn a_status_check_is_valid_for_every_check_status() {
        for status in [CheckStatus::Ok, CheckStatus::Warn, CheckStatus::Fail] {
            assert!(!status.symbol().is_empty());
        }
    }
}

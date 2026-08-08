//! Presentation: turning the daemon's structured data into something a
//! person reads.
//!
//! The daemon holds no pre-formatted strings at all — the GUI has to be
//! able to build its own progress display from the same events
//! (`docs/DESIGN.md` §3).

use minato_api::{
    ApiError, Diagnostics, EnvInfo, Event, LogLevel, ServiceInfo, StepStatus, WorkspaceInfo,
};
use minato_core::ServiceState;

/// Prints progress.
///
/// Nothing at the start; one line when a step settles. Printing both ends
/// would scroll the screen away as soon as there were a few services.
pub fn print_event(event: &Event) {
    match event {
        Event::Step { label, status, .. } => match status {
            StepStatus::Started | StepStatus::Progress { .. } => {}
            StepStatus::Done => println!("  ✓ {label}"),
            StepStatus::Failed { reason } => eprintln!("  ✗ {label}: {reason}"),
            StepStatus::Skipped { reason } => println!("  - {label} ({reason})"),
        },
        Event::Log { level, message } => match level {
            LogLevel::Debug => {}
            LogLevel::Info => println!("  {message}"),
            LogLevel::Warn => eprintln!("  warning: {message}"),
            LogLevel::Error => eprintln!("  error: {message}"),
        },
        // State changes show up in the summary, not on the way there.
        Event::ServiceState { .. } => {}
        Event::Output { line, .. } => println!("  │ {line}"),
    }
}

/// What `logs` and `exec` print.
///
/// Undecorated, so it can be grepped through a pipe or read as-is by an
/// agent. stderr goes to stderr.
pub fn print_output_event(event: &Event) {
    match event {
        Event::Output { line, stream, .. } => match stream {
            minato_api::OutputStream::Stdout => println!("{line}"),
            minato_api::OutputStream::Stderr => eprintln!("{line}"),
        },
        Event::Log {
            level: LogLevel::Warn,
            message,
        } => eprintln!("warning: {message}"),
        Event::Log {
            level: LogLevel::Error,
            message,
        } => eprintln!("error: {message}"),
        _ => {}
    }
}

/// Prints the state of one workspace.
pub fn print_workspace(workspace: &WorkspaceInfo) {
    println!();
    println!(
        "{} / {}  ({})",
        workspace.project,
        workspace.display_name(),
        workspace.branch
    );
    println!("  {}", workspace.path.display());
    println!();

    if workspace.services.is_empty() {
        println!("  no services are defined");
        return;
    }

    let name_width = workspace
        .services
        .iter()
        .map(|s| s.name.chars().count())
        .max()
        .unwrap_or(4)
        .max(4);

    for service in &workspace.services {
        println!(
            "  {:<name_width$}  {:<8}  {}",
            service.name,
            state_label(&service.state),
            access_label(service),
            name_width = name_width
        );
    }
}

/// Prints a listing of workspaces.
pub fn print_workspaces(workspaces: &[WorkspaceInfo]) {
    if workspaces.is_empty() {
        println!("no workspaces. Create one with `minato new <branch>`");
        return;
    }

    let name_width = workspaces
        .iter()
        .map(|w| w.display_name().chars().count())
        .max()
        .unwrap_or(9)
        .max(9);

    println!(
        "{:<name_width$}  {:<10}  BRANCH",
        "WORKSPACE",
        "SERVICES",
        name_width = name_width
    );

    for workspace in workspaces {
        let running = workspace
            .services
            .iter()
            .filter(|s| s.state.is_running())
            .count();

        println!(
            "{:<name_width$}  {:<10}  {}",
            workspace.display_name(),
            format!("{running}/{}", workspace.services.len()),
            workspace.branch,
            name_width = name_width
        );
    }
}

/// The state label to display.
fn state_label(state: &ServiceState) -> &'static str {
    state.label()
}

/// Where to reach it. Falls back to the listening address when no URL
/// has been issued yet.
fn access_label(service: &ServiceInfo) -> String {
    match service.access() {
        Some(access) => access,
        None if service.state.is_running() => "(internal only)".to_string(),
        None => "-".to_string(),
    }
}

/// Prints the environment.
///
/// **Each value says which layer defined it.** With three layers, not
/// seeing that an unintended one is winning makes the cause impossible to
/// find.
pub fn print_env(entries: &[EnvInfo]) {
    if entries.is_empty() {
        println!("no environment variables are defined");
        return;
    }

    let key_width = entries
        .iter()
        .map(|entry| entry.key.chars().count())
        .max()
        .unwrap_or(3)
        .max(3);

    let scope_width = entries
        .iter()
        .map(|entry| entry.scope.label().chars().count())
        .max()
        .unwrap_or(5)
        .max(5);

    for entry in entries {
        let value = match &entry.source {
            Some(source) => format!("{} → {source}", entry.value),
            None => entry.value.clone(),
        };

        println!(
            "{:<key_width$}  {:<scope_width$}  {}",
            entry.key,
            entry.scope.label(),
            value,
            key_width = key_width,
            scope_width = scope_width
        );
    }
}

/// Prints the diagnostics, always with the fix alongside.
pub fn print_diagnostics(diagnostics: &Diagnostics) {
    println!();
    for check in &diagnostics.checks {
        println!(
            "  {} {:<28}  {}",
            check.status.symbol(),
            check.title,
            check.detail
        );
    }

    let fixes = diagnostics.fixes();
    if fixes.is_empty() {
        println!();
        if diagnostics.has_failures() {
            println!("something is wrong, but there is no known fix");
        } else {
            println!("nothing wrong found");
        }
        return;
    }

    println!();
    println!("To fix:");
    for check in fixes {
        println!();
        println!("  {} {}", check.status.symbol(), check.title);
        println!("    {}", check.fix.as_deref().unwrap_or_default());
    }
    println!();
    println!("`minato setup` walks through all of it");
}

/// Prints an error, always with its `hint` when there is one.
pub fn print_error(message: &str, hint: Option<&str>) {
    eprintln!("error: {message}");
    if let Some(hint) = hint {
        eprintln!("hint: {hint}");
    }
}

/// How errors print under `--json`.
///
/// On stdout, so an agent has nothing to watch but the exit code and one
/// JSON stream.
pub fn print_error_json(error: &ApiError) {
    let payload = serde_json::json!({ "error": error });
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
    );
}

pub fn print_json<T: serde::Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(json) => println!("{json}"),
        Err(err) => eprintln!("error: cannot render the response as JSON: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minato_api::{CheckStatus, ServiceScope};

    fn service(name: &str, state: ServiceState, endpoint: Option<&str>) -> ServiceInfo {
        ServiceInfo {
            name: name.into(),
            state,
            scope: ServiceScope::Workspace,
            url: None,
            tunnel_url: None,
            endpoint: endpoint.map(str::to_string),
            port: Some(3000),
            container_id: None,
            image: None,
        }
    }

    #[test]
    fn shows_endpoint_when_available() {
        let svc = service("web", ServiceState::Ready, Some("127.0.0.1:49312"));
        assert_eq!(access_label(&svc), "http://127.0.0.1:49312");
    }

    #[test]
    fn marks_running_services_without_endpoint_as_internal() {
        let svc = service("db", ServiceState::Ready, None);
        assert_eq!(
            access_label(&svc),
            "(internal only)",
            "a blank against a running service looks broken"
        );
    }

    #[test]
    fn shows_dash_for_stopped_services() {
        let svc = service("web", ServiceState::Stopped, None);
        assert_eq!(access_label(&svc), "-");
    }

    #[test]
    fn diagnostics_symbols_distinguish_severity() {
        assert_eq!(CheckStatus::Ok.symbol(), "✓");
        assert_ne!(CheckStatus::Warn.symbol(), CheckStatus::Ok.symbol());
        assert_ne!(CheckStatus::Fail.symbol(), CheckStatus::Warn.symbol());
    }

    #[test]
    fn state_labels_are_stable() {
        assert_eq!(state_label(&ServiceState::Ready), "ready");
        assert_eq!(state_label(&ServiceState::Stopped), "stopped");
        assert_eq!(state_label(&ServiceState::failed("x")), "failed");
    }
}

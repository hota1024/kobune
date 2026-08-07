//! 表示。daemon から届いた構造化データを人間向けに変換する。
//!
//! daemon 側には整形済みの文字列を一切持たせない。同じイベント列から
//! GUI も進捗表示を作れる必要があるため（`docs/DESIGN.md` §3）。

use minato_api::{ApiError, Event, LogLevel, ServiceInfo, StepStatus, WorkspaceInfo};
use minato_core::ServiceState;

/// 進行中のステップを表示する。
///
/// 開始時点では何も出さず、決着したときだけ 1 行出す。開始と終了を
/// 両方出すと、サービスが増えたときに画面が流れて読めなくなる。
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
            LogLevel::Warn => eprintln!("  警告: {message}"),
            LogLevel::Error => eprintln!("  エラー: {message}"),
        },
        // 状態遷移はサマリで見せるので、途中では出さない。
        Event::ServiceState { .. } => {}
        Event::Output { line, .. } => println!("  │ {line}"),
    }
}

/// workspace 1 つの状態を表示する。
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
        println!("  サービスが定義されていません");
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

/// 複数 workspace を一覧表示する。
pub fn print_workspaces(workspaces: &[WorkspaceInfo]) {
    if workspaces.is_empty() {
        println!("workspace がありません。`minato new <branch>` で作成してください");
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

/// 表示用の状態ラベル。
fn state_label(state: &ServiceState) -> &'static str {
    state.label()
}

/// アクセス先。まだ URL が発行されていなければ待ち受けアドレスを出す。
fn access_label(service: &ServiceInfo) -> String {
    match service.access() {
        Some(access) => access,
        None if service.state.is_running() => "(内部のみ)".to_string(),
        None => "-".to_string(),
    }
}

/// エラーを表示する。`hint` があれば必ず添える。
pub fn print_error(message: &str, hint: Option<&str>) {
    eprintln!("エラー: {message}");
    if let Some(hint) = hint {
        eprintln!("ヒント: {hint}");
    }
}

/// `--json` 指定時のエラー出力。
///
/// 標準出力に出すことで、エージェントが終了コードと 1 本の
/// JSON ストリームだけを見ればよい形にする。
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
        Err(err) => eprintln!("エラー: 応答を JSON にできません: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minato_api::ServiceScope;

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
            "(内部のみ)",
            "起動しているのに空欄だと壊れて見える"
        );
    }

    #[test]
    fn shows_dash_for_stopped_services() {
        let svc = service("web", ServiceState::Stopped, None);
        assert_eq!(access_label(&svc), "-");
    }

    #[test]
    fn state_labels_are_stable() {
        assert_eq!(state_label(&ServiceState::Ready), "ready");
        assert_eq!(state_label(&ServiceState::Stopped), "stopped");
        assert_eq!(state_label(&ServiceState::failed("x")), "failed");
    }
}

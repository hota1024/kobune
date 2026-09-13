// Mirrors `apps/studio/src/model.rs`, which in turn wraps `kobune-api`.
// The names are the API's, not new ones: a field that reads differently
// here than in `ServiceInfo` is a field somebody will have to translate
// twice.

/** `ServiceState`, as `kobune-core` labels it. */
export type ServiceState =
  | 'stopped'
  | 'starting'
  | 'ready'
  | 'idle'
  | 'failed'
  | 'unknown'

export type ServiceScope = 'workspace' | 'project'

export interface ServiceInfo {
  name: string
  state: ServiceState
  /** Why, when `state` is `failed`. Beside it, not inside it. */
  reason?: string | null
  scope: ServiceScope
  /** The `*.localhost` address. Absent for a service with no port. */
  url?: string | null
  /** The public address, when the tunnel is up. */
  tunnel_url?: string | null
  endpoint?: string | null
  port?: number | null
  container_id?: string | null
  image?: string | null
}

export interface WorkspaceView {
  /** `(main)` for the main worktree. */
  label: string
  /** `null` for the main worktree — its hostname carries no label. */
  workspace: string | null
  branch: string
  path: string
  is_main: boolean
  /** Counted with `ServiceState::is_running()` — starting | ready | idle. */
  running: number
  total: number
  services: ServiceInfo[]
}

export interface DaemonView {
  version: string
  protocol: number
  runtime: string
  uptime_secs: number
}

export interface TunnelView {
  state: string
  running: boolean
  domain?: string
  public: boolean
}

export interface StateView {
  daemon: DaemonView
  project: string | null
  tunnel: TunnelView
  workspaces: WorkspaceView[]
}

/** `ok` is fine, `warn` is usable, `fail` is not. */
export type CheckStatus = 'ok' | 'warn' | 'fail'

export interface Check {
  /** Stable; agents branch on it. Not for reading. */
  id: string
  title: string
  status: CheckStatus
  detail: string
  /** The command that fixes it, when one does. */
  fix?: string | null
}

export interface DoctorView {
  checks: Check[]
}

export interface EnvEntry {
  key: string
  value: string
  scope: string
  secret: boolean
  source?: string | null
}

export interface EnvView {
  service?: string
  entries: EnvEntry[]
}

export interface LogLine {
  service?: string
  /** `stdout`, `stderr`, or `log` for the daemon's own commentary. */
  stream: 'stdout' | 'stderr' | 'log'
  text: string
}

/**
 * `path` is the workspace's own root, never its label.
 *
 * The main worktree has no label — `WorkspaceView.workspace` is `null` for
 * it — and a request carrying no label sends the daemon to whichever
 * worktree the studio was started in. Targeting by path is what the TUI
 * does, and it cannot mis-resolve.
 */
export type Action =
  | { kind: 'up'; path?: string | null; services?: string[]; rebuild?: boolean }
  | { kind: 'down'; path?: string | null; services?: string[] }
  | { kind: 'restart'; path?: string | null; services?: string[] }
  | { kind: 'rm'; path?: string | null; force?: boolean }
  | { kind: 'new'; branch: string; base?: string | null; start?: boolean }

import { ServiceCard } from './ServiceCard'
import type { Action, WorkspaceView } from '../lib/types'

/**
 * The open workspace: what it is, and what is running in it.
 *
 * `rm` is here, in its own colour, and it is the only chromatic thing on
 * the screen apart from a failure. It deletes a worktree with somebody's
 * uncommitted work in it, so it is not going to sit in a row of grey
 * buttons looking like `env`.
 */
export function WorkspaceDetail({
  workspace,
  tunnelRunning,
  showQr,
  onAct,
  onEnv,
  onLogs,
}: {
  workspace: WorkspaceView
  tunnelRunning: boolean
  showQr: boolean
  onAct: (action: Action) => void
  onEnv: () => void
  onLogs: (service?: string) => void
}) {
  // The workspace's own root, not its label: the main worktree has no
  // label, and a request without one resolves to wherever the studio was
  // started. See `Studio::workspace` in `apps/studio/src/api.rs`.
  const target = workspace.path

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex flex-col gap-2.5 border-b border-shell-rule px-[22px] pt-[18px] pb-3.5">
        <div className="flex items-center gap-3">
          <div className="font-mono text-21 font-semibold tracking-[-0.015em] text-shell-fg">
            {workspace.label}
          </div>
          <div className="border border-shell-wire px-2 py-[3px] font-mono text-11 text-ink-good">
            {workspace.running} running
          </div>

          <div className="flex-1" />

          <div className="flex gap-2">
            <button
              type="button"
              onClick={() => onAct({ kind: 'up', path: target })}
              className="cursor-pointer bg-primary px-3.5 py-1.5 font-mono text-12 font-semibold text-on-primary hover:opacity-90"
            >
              ▶ up
            </button>
            <button
              type="button"
              onClick={() => onAct({ kind: 'down', path: target })}
              className="cursor-pointer border border-shell-rule px-3.5 py-1.5 font-mono text-12 text-shell-fg hover:border-shell-muted"
            >
              ▮▮ down
            </button>
            {/* `exec` lends a terminal to a container. That needs a
                terminal to lend, and this page has none — the API's
                `Attached` / `Bytes` half is a PTY, not a log. The button
                says where it lives rather than pretending. */}
            <button
              type="button"
              disabled
              title="kobune exec runs in a terminal — use the CLI"
              className="cursor-not-allowed border border-shell-rule px-3 py-1.5 font-mono text-12 text-shell-muted opacity-60"
            >
              ⌘ exec
            </button>
            <button
              type="button"
              onClick={onEnv}
              className="cursor-pointer border border-shell-rule px-3 py-1.5 font-mono text-12 text-shell-muted hover:text-shell-fg"
            >
              env
            </button>
            <button
              type="button"
              onClick={() => onAct({ kind: 'rm', path: target })}
              className="cursor-pointer border border-danger-rule px-3 py-1.5 font-mono text-12 text-ink-bad hover:bg-danger-wash"
            >
              rm
            </button>
          </div>
        </div>

        <div className="flex gap-[18px] overflow-hidden font-mono text-115 whitespace-nowrap text-shell-muted">
          <span>{workspace.branch}</span>
          <span className="text-shell-rule">|</span>
          <span className="truncate">{workspace.path}</span>
          <span className="text-shell-rule">|</span>
          <span>{workspace.is_main ? 'main worktree' : 'worktree'}</span>
        </div>
      </div>

      <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-[22px] py-4">
        {workspace.services.length === 0 && (
          <div className="font-mono text-12 text-shell-muted">
            No services. `kobune.toml` in this worktree defines none.
          </div>
        )}

        {workspace.services.map((service) => (
          <ServiceCard
            key={service.name}
            service={service}
            tunnelRunning={tunnelRunning}
            showQr={showQr}
            onLogs={() => onLogs(service.name)}
            onAct={(what) =>
              onAct(
                what === 'restart'
                  ? { kind: 'restart', path: target, services: [service.name] }
                  : { kind: what, path: target, services: [service.name] },
              )
            }
          />
        ))}
      </div>
    </div>
  )
}

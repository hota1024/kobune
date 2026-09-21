import type { WorkspaceView } from '../lib/types'

/**
 * The bottom rail: what the daemon is in the middle of, and the keys.
 *
 * The left half is the one thing a poll cannot show as a state — a start
 * in progress. `reason` is where the daemon puts "health check 2/10", so
 * it is read from the service rather than counted here.
 */
export function StatusBar({
  workspace,
  workspaces,
}: {
  workspace: WorkspaceView | null
  workspaces: WorkspaceView[]
}) {
  const starting = workspace?.services.find((service) => service.state === 'starting')
  const failed = workspace?.services.find((service) => service.state === 'failed')
  const runningTotal = workspaces.reduce((total, one) => total + one.running, 0)

  return (
    <div className="flex h-7 shrink-0 items-center gap-4 border-t border-shell-rule px-4 font-mono text-105">
      {failed ? (
        <>
          <span className="text-ink-bad">✗</span>
          <span className="truncate text-ink-bad">
            {failed.name} failed{failed.reason ? ` · ${failed.reason}` : ''}
          </span>
        </>
      ) : starting ? (
        <>
          <span className="kb-pulse text-ink-warn">◐</span>
          <span className="text-shell-muted">waiting for {starting.name}</span>
          {starting.reason && <span className="text-shell-muted">· {starting.reason}</span>}
        </>
      ) : (
        <>
          <span className="text-ink-good">●</span>
          <span className="text-shell-muted">nothing in flight</span>
        </>
      )}

      <div className="flex-1" />

      <span className="text-shell-muted">
        {runningTotal} running across {workspaces.length}{' '}
        {workspaces.length === 1 ? 'workspace' : 'workspaces'}
      </span>
      <span className="text-shell-dim">⌘1–9</span>
      <span className="text-shell-muted">tab</span>
      <span className="text-shell-dim">⌘J</span>
      <span className="text-shell-muted">logs</span>
      <span className="text-shell-dim">⌘K</span>
      <span className="text-shell-muted">anything</span>
    </div>
  )
}

import type { ReactElement, ReactNode } from 'react'
import { ContextMenu } from '@base-ui/react/context-menu'
import type { Action, WorkspaceView } from '../lib/types'

/**
 * What a workspace offers on a right click, wherever it is shown.
 *
 * **Right click, not left.** The left one already means something on both
 * surfaces — a sidebar row opens as a tab, a tab comes forward — and those
 * are the actions worth one click rather than two. This is the same set
 * the detail pane's buttons carry, brought to the row so that acting on a
 * workspace does not require opening it first.
 *
 * `render` merges the trigger into the caller's own element, so the row
 * stays one button and gains a second gesture rather than a wrapper.
 */
export function WorkspaceMenu({
  workspace,
  trigger,
  children,
  onOpen,
  onCloseTab,
  onAct,
  onEnv,
  onLogs,
}: {
  workspace: WorkspaceView
  /** The element the trigger becomes. Its content is `children`. */
  trigger: ReactElement
  children: ReactNode
  /** Shown when the workspace has no tab yet. */
  onOpen?: () => void
  /** Shown on a tab. Closing one is a tab operation, never a `down`. */
  onCloseTab?: () => void
  onAct: (action: Action) => void
  onEnv: () => void
  onLogs: () => void
}) {
  // The workspace's own root, never its label — the main worktree has no
  // label. See `Studio::workspace` in `apps/studio/src/api.rs`.
  const path = workspace.path
  const idle = workspace.running === 0

  return (
    <ContextMenu.Root>
      <ContextMenu.Trigger render={trigger}>{children}</ContextMenu.Trigger>
      <ContextMenu.Portal>
        <ContextMenu.Positioner className="z-50 outline-none">
          <ContextMenu.Popup className="kb-float min-w-[200px] origin-[var(--transform-origin)] border border-shell-edge bg-shell-palette py-1 outline-none">
            <div className="truncate px-3 pt-1 pb-1.5 font-mono text-10 tracking-[.12em] text-shell-muted uppercase select-none">
              {workspace.label}
            </div>

            {onOpen && (
              <>
                <Item onClick={onOpen}>Open as a tab</Item>
                <Rule />
              </>
            )}

            <Item onClick={() => onAct({ kind: 'up', path })} disabled={!idle && workspace.running === workspace.total}>
              ▶ up
            </Item>
            <Item onClick={() => onAct({ kind: 'down', path })} disabled={idle}>
              ▮▮ down
            </Item>
            <Item onClick={() => onAct({ kind: 'restart', path })} disabled={idle}>
              restart
            </Item>

            <Rule />

            <Item onClick={onEnv}>env</Item>
            <Item onClick={onLogs}>logs</Item>

            {onCloseTab && (
              <>
                <Rule />
                {/* Closing the tab and nothing else. `down` is above it and
                    stays a separate, deliberate act. */}
                <Item onClick={onCloseTab}>Close tab</Item>
              </>
            )}

            <Rule />

            {/* The one chromatic thing in the menu, for the one item that
                deletes a worktree with somebody's uncommitted work in it. */}
            <Item onClick={() => onAct({ kind: 'rm', path })} danger>
              rm
            </Item>
          </ContextMenu.Popup>
        </ContextMenu.Positioner>
      </ContextMenu.Portal>
    </ContextMenu.Root>
  )
}

function Item({
  children,
  onClick,
  disabled,
  danger,
}: {
  children: React.ReactNode
  onClick: () => void
  disabled?: boolean
  danger?: boolean
}) {
  return (
    <ContextMenu.Item
      disabled={disabled}
      onClick={onClick}
      className={`flex cursor-pointer px-3 py-1.5 font-mono text-115 outline-none select-none data-highlighted:bg-shell-high data-disabled:cursor-not-allowed data-disabled:opacity-40 ${
        danger ? 'text-ink-bad' : 'text-shell-soft data-highlighted:text-shell-fg'
      }`}
    >
      {children}
    </ContextMenu.Item>
  )
}

function Rule() {
  return <ContextMenu.Separator className="my-1 h-px bg-shell-rule" />
}

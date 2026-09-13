import type { WorkspaceView } from '../lib/types'

/**
 * The workspaces you have open.
 *
 * **✕ closes the tab and nothing else.** The environment keeps running,
 * and `down` and `rm` stay where they are — explicit, in the detail pane.
 * A close button that stopped containers would be one stray click away
 * from taking somebody's database down, and the two actions are not
 * close enough in consequence to share a control.
 */
export function TabStrip({
  tabs,
  workspaces,
  selected,
  onSelect,
  onClose,
  onPick,
}: {
  tabs: string[]
  workspaces: WorkspaceView[]
  selected: string | null
  onSelect: (label: string) => void
  onClose: (label: string) => void
  onPick: () => void
}) {
  const notOpen = workspaces.filter((workspace) => !tabs.includes(workspace.label)).length

  return (
    <div className="flex h-9 shrink-0 border-b border-shell-rule bg-shell-sink">
      {tabs.map((label) => {
        const workspace = workspaces.find((candidate) => candidate.label === label)
        const isSelected = label === selected

        return (
          <div
            key={label}
            className={`flex items-center gap-[9px] border-t-2 border-r border-r-shell-rule px-[15px] ${
              isSelected
                ? 'border-t-bright bg-shell-bg'
                : 'border-t-transparent bg-shell-sink'
            }`}
          >
            <button
              type="button"
              onClick={() => onSelect(label)}
              className={`cursor-pointer font-mono text-12 ${
                isSelected ? 'font-semibold text-shell-fg' : 'text-shell-muted'
              }`}
            >
              {label}
            </button>
            {workspace && (
              <span className="font-mono text-105 text-shell-muted">
                {workspace.running}/{workspace.total}
              </span>
            )}
            <button
              type="button"
              onClick={() => onClose(label)}
              aria-label={`Close the ${label} tab`}
              title="Close the tab. The environment keeps running."
              className="cursor-pointer font-mono text-10 text-shell-muted hover:text-shell-fg"
            >
              ✕
            </button>
          </div>
        )
      })}

      <button
        type="button"
        onClick={onPick}
        aria-label="Open a workspace as a tab"
        className="flex cursor-pointer items-center border-r border-shell-rule px-3.5 font-mono text-12 text-shell-muted hover:text-shell-fg"
      >
        +
      </button>

      <div className="flex-1" />

      <button
        type="button"
        onClick={onPick}
        className="flex cursor-pointer items-center gap-2.5 px-[15px] font-mono text-105 text-shell-muted hover:text-shell-fg"
      >
        <span>
          {notOpen === 1 ? '1 more workspace, not open' : `${notOpen} more workspaces, not open`}
        </span>
        <span className="text-shell-dim">⌘P</span>
      </button>
    </div>
  )
}

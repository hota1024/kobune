import type { WorkspaceView } from '../lib/types'

/**
 * Every workspace the project has — not only the open ones.
 *
 * That is the whole point of the list, and the premise of Kobune: you
 * keep more worktrees than you can remember. The tabs are what you have
 * open; this is what exists.
 */
export function Sidebar({
  workspaces,
  selected,
  openTabs,
  onOpen,
  onNew,
}: {
  workspaces: WorkspaceView[]
  selected: string | null
  openTabs: string[]
  onOpen: (label: string) => void
  onNew: () => void
}) {
  const notOpen = workspaces.filter((workspace) => !openTabs.includes(workspace.label)).length

  return (
    <div className="flex w-[260px] shrink-0 flex-col border-r border-shell-rule bg-shell-sink">
      <div className="flex items-center gap-2 border-b border-shell-rule px-3.5 pt-[11px] pb-2">
        <div className="font-mono text-11 font-semibold tracking-[.12em] text-ink-heading uppercase">
          workspaces
        </div>
        <div className="flex-1" />
        <div className="font-mono text-11 text-shell-muted">⌥⌘B</div>
        <button
          type="button"
          onClick={onNew}
          className="cursor-pointer border border-shell-rule px-2 py-0.5 font-mono text-11 text-shell-fg hover:bg-shell-high"
        >
          + new
        </button>
      </div>

      <div className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto p-1.5">
        {workspaces.map((workspace) => {
          const isSelected = workspace.label === selected
          const isOpen = openTabs.includes(workspace.label)
          const live = workspace.running > 0
          const shared = workspace.services.some((service) => service.tunnel_url)

          return (
            <button
              key={workspace.label}
              type="button"
              onClick={() => onOpen(workspace.label)}
              className={`flex cursor-pointer flex-col gap-1 border-l-2 px-2.5 py-2 text-left ${
                isSelected
                  ? 'border-l-bright bg-shell-sel'
                  : 'border-l-transparent hover:bg-shell-hover'
              }`}
            >
              <div className="flex items-center gap-2">
                <span className={`font-mono text-12 ${live ? 'text-ink-good' : 'text-shell-muted'}`}>
                  {live ? '●' : '○'}
                </span>
                <span
                  className={`min-w-0 flex-1 truncate font-mono text-13 ${
                    isSelected ? 'font-semibold text-shell-fg' : 'text-shell-dim'
                  }`}
                >
                  {workspace.label}
                </span>
                {shared && <span className="font-mono text-10 text-ink-warn">▲</span>}
                <span
                  className={`font-mono text-11 ${live ? 'text-ink-good' : 'text-shell-muted'}`}
                >
                  {workspace.running}/{workspace.total}
                </span>
              </div>

              <div className="flex gap-2 overflow-hidden pl-5 font-mono text-105 whitespace-nowrap text-shell-muted">
                {isOpen && (
                  <span className="border border-shell-rule px-1 text-shell-badge">
                    open
                  </span>
                )}
                <span className="truncate">{workspace.branch}</span>
              </div>
            </button>
          )
        })}
      </div>

      <div className="border-t border-shell-rule px-3.5 py-[9px]">
        <div className="font-mono text-105 text-shell-muted">
          {notOpen === 1 ? '1 more workspace, not open' : `${notOpen} more workspaces, not open`}
        </div>
        <div className="mt-0.5 font-mono text-10 text-shell-muted">
          <span className="text-shell-dim">↵</span> opens it as a tab
        </div>
      </div>
    </div>
  )
}

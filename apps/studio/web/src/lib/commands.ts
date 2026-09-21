import type { StateView, WorkspaceView } from './types'
import { address } from './service'

/** One row in the palette. */
export interface Command {
  id: string
  label: string
  detail: string
  keys?: string
  /** What ⌘↵ puts on the clipboard when this row is highlighted. */
  copy?: string
  run: () => void
}

export interface CommandGroup {
  /** The section heading — `act`, `go to`, `workspace · <name>`. */
  value: string
  items: Command[]
  /** Base UI's `Group` carries an index signature; this satisfies it. */
  [key: string]: unknown
}

export interface Handlers {
  openUrl: (url: string) => void
  copy: (text: string) => void
  act: (action: import('./types').Action) => void
  followLogs: (service: string) => void
  openTab: (label: string) => void
}

/**
 * What ⌘K can do, given what is on the screen.
 *
 * Two jobs in one list, which is the point of the merge: the top groups
 * act on the workspace in front of you, and `go to` opens one that has no
 * tab. Picking from `go to` **adds** a tab rather than replacing the
 * current one — switching away from what you were watching is not what
 * you asked for by naming somewhere else.
 */
export function commandsFor(
  state: StateView,
  current: WorkspaceView | null,
  handlers: Handlers,
): CommandGroup[] {
  const groups: CommandGroup[] = []
  const tunnelRunning = state.tunnel.running

  if (current) {
    // The path, not the label — see `WorkspaceDetail`.
    const target = current.path
    const reachable = current.services.filter((service) => address(service, tunnelRunning))

    groups.push({
      value: `workspace · ${current.label}`,
      items: reachable.flatMap((service) => {
        const url = address(service, tunnelRunning) as string
        return [
          {
            id: `open:${service.name}`,
            label: `Open ${service.name}`,
            detail: url.replace(/^https?:\/\//, ''),
            keys: '↵',
            copy: url,
            run: () => handlers.openUrl(url),
          },
          {
            id: `copy:${service.name}`,
            label: `Copy ${service.name} URL`,
            detail: 'to the clipboard',
            keys: '↵',
            copy: url,
            run: () => handlers.copy(url),
          },
        ]
      }),
    })

    groups.push({
      value: 'act',
      items: [
        {
          id: 'up',
          label: `Start ${current.label}`,
          detail: 'every service in the workspace',
          run: () => handlers.act({ kind: 'up', path: target }),
        },
        {
          id: 'down',
          label: `Stop ${current.label}`,
          detail: 'every service in the workspace',
          run: () => handlers.act({ kind: 'down', path: target }),
        },
        ...current.services.flatMap((service) => [
          {
            id: `restart:${service.name}`,
            label: `Restart ${service.name}`,
            detail: 'down, then up',
            run: () =>
              handlers.act({ kind: 'restart', path: target, services: [service.name] }),
          },
          {
            id: `logs:${service.name}`,
            label: `Follow logs: ${service.name}`,
            detail: 'stream into the pane',
            run: () => handlers.followLogs(service.name),
          },
        ]),
      ],
    })
  }

  const elsewhere = state.workspaces.filter(
    (workspace) => workspace.label !== current?.label,
  )

  if (elsewhere.length > 0) {
    groups.push({
      value: 'go to',
      items: elsewhere.map((workspace) => ({
        id: `goto:${workspace.label}`,
        label: workspace.label,
        detail:
          workspace.running > 0
            ? `${workspace.running}/${workspace.total} running · ${workspace.branch}`
            : `stopped — wakes on request · ${workspace.branch}`,
        run: () => handlers.openTab(workspace.label),
      })),
    })
  }

  return groups.filter((group) => group.items.length > 0)
}

/**
 * Narrows the list to what was typed, keeping the grouping.
 *
 * Done here rather than with Base UI's own filter because the match has
 * to look at the label and the detail — "release-1-2" is typed at the
 * branch as often as at the workspace name.
 */
export function narrow(groups: CommandGroup[], query: string): CommandGroup[] {
  const needle = query.trim().toLowerCase()
  if (!needle) return groups

  return groups
    .map((group) => ({
      value: group.value,
      items: group.items.filter(
        (command) =>
          command.label.toLowerCase().includes(needle) ||
          command.detail.toLowerCase().includes(needle),
      ),
    }))
    .filter((group) => group.items.length > 0)
}

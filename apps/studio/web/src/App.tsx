import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { TopBar } from './components/TopBar'
import { Sidebar } from './components/Sidebar'
import { TabStrip } from './components/TabStrip'
import { WorkspaceDetail } from './components/WorkspaceDetail'
import { LogPane, type Stamped } from './components/LogPane'
import { StatusBar } from './components/StatusBar'
import { CommandPalette } from './components/CommandPalette'
import { DoctorOverlay, EnvOverlay } from './components/Overlay'
import { act, fetchDoctor, fetchEnv, followLogs } from './lib/api'
import { commandsFor } from './lib/commands'
import { useStudioState } from './lib/state'
import { hasToken, wantedWorkspace } from './lib/session'
import type { Action, DoctorView, EnvView, WorkspaceView } from './lib/types'

/**
 * Tabs survive a reload, in `localStorage`.
 *
 * The designer's note leans towards keeping them with the daemon's state,
 * which is the better answer and the one that would make them the same
 * tabs in a second browser. It also means new API, so this is the cut:
 * the browser remembers its own tabs, and nothing was added to the socket
 * for a list of strings.
 */
const TABS_KEY = 'kobune.studio.tabs'
const THEME_KEY = 'kobune.studio.theme'

/** How many lines the pane keeps. A follow is unbounded; a browser is not. */
const LOG_LIMIT = 5000

export function App() {
  const { state, error, loading, refresh } = useStudioState()

  const [tabs, setTabs] = useState<string[]>(() => {
    try {
      const stored = JSON.parse(localStorage.getItem(TABS_KEY) ?? '[]')
      return Array.isArray(stored) ? (stored as string[]) : []
    } catch {
      return []
    }
  })
  const [selected, setSelected] = useState<string | null>(null)
  const [theme, setTheme] = useState<'dark' | 'light'>(
    () => (localStorage.getItem(THEME_KEY) as 'dark' | 'light' | null) ?? 'dark',
  )

  const [palette, setPalette] = useState(false)
  /** ⌘P is the same palette scoped to workspaces alone. */
  const [paletteScope, setPaletteScope] = useState<'all' | 'goto'>('all')
  const [doctor, setDoctor] = useState<DoctorView | null>(null)
  const [doctorOpen, setDoctorOpen] = useState(false)
  const [env, setEnv] = useState<EnvView | null>(null)
  const [envOpen, setEnvOpen] = useState(false)

  const [logLines, setLogLines] = useState<Stamped[]>([])
  const [logFilter, setLogFilter] = useState<string | null>(null)
  const [logHeight, setLogHeight] = useState(168)
  const [logOpen, setLogOpen] = useState(true)
  const [logExpanded, setLogExpanded] = useState(false)
  const [following, setFollowing] = useState(false)
  const [sidebarOpen, setSidebarOpen] = useState(true)
  const seq = useRef(0)
  /// The current tabs, for callbacks that must not be rebuilt on every change.
  const tabsRef = useRef<string[]>([])

  const workspaces = useMemo(() => state?.workspaces ?? [], [state])

  useEffect(() => {
    tabsRef.current = tabs
    localStorage.setItem(TABS_KEY, JSON.stringify(tabs))
  }, [tabs])

  useEffect(() => {
    localStorage.setItem(THEME_KEY, theme)
    document.documentElement.dataset.theme = theme
  }, [theme])

  // A tab for a workspace that no longer exists is a dead tab. `rm`
  // happens in this very screen, so this is the normal case rather than a
  // repair.
  useEffect(() => {
    if (workspaces.length === 0) return
    const live = new Set(workspaces.map((workspace) => workspace.label))
    setTabs((current) => {
      const kept = current.filter((label) => live.has(label))
      return kept.length === current.length ? current : kept
    })

    // **And the selection with it.** Dropping only the tab would leave
    // `selected` naming a workspace that no longer exists, which is not
    // nothing — it is truthy, so the effect below that opens something
    // sees a selection and does not act. The pane then stays empty next
    // to a row of perfectly good tabs.
    setSelected((current) => (current && !live.has(current) ? null : current))
  }, [workspaces])

  // Open something on the first listing, so arriving at the dashboard does
  // not mean a blank pane beside a full sidebar. **On the first listing
  // only** — closing the last tab is a deliberate act, and reopening one
  // underneath somebody would make the ✕ look broken.
  useEffect(() => {
    // `''` is "the last tab was closed on purpose"; `null` is "nothing has
    // been chosen yet".
    if (!state || selected !== null) return
    // `kobune studio -w` named one, and an explicit ask outranks both the
    // remembered tabs and the guess below.
    const asked = wantedWorkspace()
    const named = asked && workspaces.some((w) => w.label === asked) ? asked : null

    const first = tabs.find((label) => workspaces.some((w) => w.label === label))
    // A running workspace before a stopped one, and a worktree before the
    // main checkout: what somebody opening the dashboard wants to look at
    // is whatever is already up.
    const fallback =
      workspaces.find((w) => w.running > 0 && !w.is_main) ??
      workspaces.find((w) => w.running > 0) ??
      workspaces.find((w) => !w.is_main) ??
      workspaces[0]
    const label = named ?? first ?? fallback?.label ?? null
    if (label) {
      setSelected(label)
      setTabs((current) => (current.includes(label) ? current : [...current, label]))
    }
  }, [state, selected, tabs, workspaces])

  const current = useMemo(
    () => workspaces.find((workspace) => workspace.label === selected) ?? null,
    [workspaces, selected],
  )

  const openTab = useCallback((label: string) => {
    setTabs((current) => (current.includes(label) ? current : [...current, label]))
    setSelected(label)
  }, [])

  const closeTab = useCallback((label: string) => {
    // `setSelected` outside the `setTabs` updater: an updater has to be
    // pure, and StrictMode calls it twice.
    setTabs((current) => current.filter((one) => one !== label))
    setSelected((current) => {
      if (current !== label) return current
      const remaining = tabsRef.current.filter((one) => one !== label)
      // `''` rather than `null` when nothing is left: null means "nothing
      // has been chosen yet" and the effect below would helpfully open a
      // workspace again, so closing the last tab would not close it.
      return remaining[remaining.length - 1] ?? ''
    })
  }, [])

  const run = useCallback(
    async (action: Action) => {
      // `rm` deletes a worktree, and the daemon will not ask — it never
      // prompts (§3), so the question has to be asked here or nowhere.
      if (action.kind === 'rm') {
        // **From the action, never from the selection.** The context menu
        // can remove a workspace that is not the open one, and a dialog
        // that named the open tab while deleting another would be worse
        // than no dialog at all. The path is the fallback because it is
        // exactly what will go.
        const target = workspaces.find((workspace) => workspace.path === action.path)
        const label = target?.label || action.path || 'this workspace'
        const sure = window.confirm(
          `Remove ${label}?\n\nThis deletes the worktree and its containers. Uncommitted work in it goes too.`,
        )
        if (!sure) return
      }

      try {
        await act(action)
      } catch (caught) {
        window.alert(caught instanceof Error ? caught.message : String(caught))
      } finally {
        refresh()
      }
    },
    [refresh, workspaces],
  )

  // The log follow. Restarted when the workspace or the filter changes,
  // and aborted on the way out — which is what makes the daemon let go of
  // the runtime's stream rather than leaving a follower behind per pane.
  useEffect(() => {
    if (!logOpen || !current) {
      setFollowing(false)
      return
    }

    setLogLines([])
    setFollowing(true)
    seq.current = 0

    const stop = followLogs(
      current.path,
      logFilter ?? undefined,
      (line) => {
        setLogLines((lines) => {
          const stamped: Stamped = {
            ...line,
            at: new Date().toLocaleTimeString(undefined, { hour12: false }),
            seq: seq.current++,
          }
          const next = [...lines, stamped]
          return next.length > LOG_LIMIT ? next.slice(next.length - LOG_LIMIT) : next
        })
      },
      // Clean end or error, the pane stops claiming to follow.
      () => setFollowing(false),
    )

    return () => {
      stop()
      setFollowing(false)
    }
  }, [current?.label, current?.path, logFilter, logOpen])

  const openDoctor = useCallback(async () => {
    setDoctorOpen(true)
    setDoctor(null)
    try {
      setDoctor(await fetchDoctor())
    } catch {
      setDoctorOpen(false)
    }
  }, [])

  /** The workspace the env overlay is describing, for its subtitle. */
  const [envOf, setEnvOf] = useState<string | null>(null)

  // Which env listing is wanted. A slow one that resolves after a second
  // was asked for would otherwise land under the second one's heading.
  const envRequest = useRef(0)

  const openEnv = useCallback(
    async (workspace?: WorkspaceView) => {
      const target = workspace ?? current
      const ticket = ++envRequest.current

      setEnvOf(target?.label ?? null)
      setEnvOpen(true)
      setEnv(null)

      try {
        const entries = await fetchEnv(target?.path ?? null)
        if (ticket === envRequest.current) setEnv(entries)
      } catch {
        if (ticket === envRequest.current) setEnvOpen(false)
      }
    },
    [current],
  )

  /**
   * Show a workspace's logs, opening it first if it is not open.
   *
   * The pane follows whatever is in front of you, so there is no way to
   * watch a workspace without having it open — and quietly following one
   * that is not on screen would be worse than opening it.
   */
  const showLogs = useCallback(
    (workspace: WorkspaceView) => {
      openTab(workspace.label)
      setLogFilter(null)
      setLogOpen(true)
    },
    [openTab],
  )

  const commands = useMemo(
    () =>
      state
        ? commandsFor(state, current, {
            openUrl: (url) => window.open(url, '_blank', 'noreferrer'),
            copy: (text) => void navigator.clipboard.writeText(text).catch(() => {}),
            act: (action) => void run(action),
            followLogs: (service) => {
              setLogOpen(true)
              setLogFilter(service)
            },
            openTab,
          })
        : [],
    [state, current, run, openTab],
  )

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const meta = event.metaKey || event.ctrlKey

      // **`code`, not `key`.** Holding Option on macOS composes the
      // character — ⌥⌘B arrives with `key` of `'∫'`, so matching on the
      // letter silently loses every shortcut with Option in it.
      if (meta && event.altKey && event.code === 'KeyB') {
        event.preventDefault()
        setSidebarOpen((open) => !open)
        return
      }
      if (event.altKey) return

      if (meta && event.code === 'KeyK') {
        event.preventDefault()
        setPaletteScope('all')
        setPalette((open) => !open)
        return
      }
      if (meta && event.code === 'KeyP') {
        event.preventDefault()
        setPaletteScope('goto')
        setPalette(true)
        return
      }
      if (meta && event.code === 'KeyJ') {
        event.preventDefault()
        setLogOpen((open) => !open)
        return
      }
      if (meta && /^Digit[1-9]$/.test(event.code)) {
        const index = Number(event.code.slice(-1)) - 1
        if (tabs[index]) {
          event.preventDefault()
          setSelected(tabs[index])
        }
      }
    }

    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [tabs])

  if (!hasToken()) {
    return (
      <Shell>
        <div className="m-auto max-w-[520px] p-8 font-mono text-13 text-shell-muted">
          <div className="mb-2 text-15 text-shell-fg">No session token.</div>
          This page authenticates with a token that `kobune studio` puts in the URL fragment
          when it opens your browser. Open the URL it printed, rather than this one.
        </div>
      </Shell>
    )
  }

  return (
    <Shell>
      <TopBar
        state={state}
        connected={!error}
        theme={theme}
        onToggleTheme={() => setTheme((now) => (now === 'dark' ? 'light' : 'dark'))}
        onPalette={() => {
          setPaletteScope('all')
          setPalette(true)
        }}
        onDoctor={() => void openDoctor()}
      />

      {error && (
        <div className="flex items-center gap-3 border-b border-shell-rule bg-danger-bg px-4 py-2 font-mono text-12 text-ink-bad">
          <span>✗</span>
          <span>{error.message}</span>
          {error.hint && <span className="text-shell-muted">— {error.hint}</span>}
        </div>
      )}

      <div className="flex min-h-0 flex-1">
        {sidebarOpen && (
          <Sidebar
            workspaces={workspaces}
            selected={selected}
            openTabs={tabs}
            onOpen={openTab}
            onAct={(action) => void run(action)}
            onEnv={(workspace) => void openEnv(workspace)}
            onLogs={showLogs}
            onNew={() => {
              const branch = window.prompt('New workspace — branch name')
              if (branch) void run({ kind: 'new', branch, start: true })
            }}
          />
        )}

        <div className="flex min-w-0 flex-1 flex-col">
          <TabStrip
            tabs={tabs}
            workspaces={workspaces}
            selected={selected}
            onSelect={setSelected}
            onClose={closeTab}
            onPick={() => {
              setPaletteScope('goto')
              setPalette(true)
            }}
            onAct={(action) => void run(action)}
            onEnv={(workspace) => void openEnv(workspace)}
            onLogs={showLogs}
          />

          {current ? (
            <WorkspaceDetail
              workspace={current}
              tunnelRunning={state?.tunnel.running ?? false}
              showQr={!logExpanded}
              onAct={(action) => void run(action)}
              onEnv={() => void openEnv()}
              onLogs={(service) => {
                setLogOpen(true)
                setLogFilter(service ?? null)
              }}
            />
          ) : (
            <div className="flex min-h-0 flex-1 items-center justify-center font-mono text-12 text-shell-muted">
              {loading ? 'reading the daemon…' : 'No workspace open. ⌘K, or pick one on the left.'}
            </div>
          )}
        </div>
      </div>

      {logOpen && current && (
        <LogPane
          lines={logLines}
          workspace={current.label}
          services={current.services.map((service) => service.name)}
          filter={logFilter}
          following={following}
          height={logExpanded ? Math.max(logHeight, 420) : logHeight}
          expanded={logExpanded}
          onFilter={setLogFilter}
          onResize={setLogHeight}
          onExpand={() => setLogExpanded((now) => !now)}
        />
      )}

      <StatusBar workspace={current} workspaces={workspaces} />

      <CommandPalette
        open={palette}
        groups={commands}
        only={paletteScope}
        scope={current?.label ?? null}
        onOpenChange={setPalette}
      />
      <DoctorOverlay open={doctorOpen} doctor={doctor} onOpenChange={setDoctorOpen} />
      <EnvOverlay open={envOpen} env={env} workspace={envOf} onOpenChange={setEnvOpen} />
    </Shell>
  )
}

function Shell({ children }: { children: React.ReactNode }) {
  return <div className="flex h-full flex-col bg-shell-bg text-shell-fg">{children}</div>
}

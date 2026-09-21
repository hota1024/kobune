import { Mark } from './Mark'
import type { StateView } from '../lib/types'

/**
 * The chrome across the top: who you are talking to, and the two ways in.
 *
 * The daemon's dot and the runtime's name sit here rather than in a
 * status bar because "is the daemon up" is the question behind every
 * other thing on the screen being empty.
 */
export function TopBar({
  state,
  connected,
  theme,
  onToggleTheme,
  onPalette,
  onDoctor,
}: {
  state: StateView | null
  connected: boolean
  theme: 'dark' | 'light'
  onToggleTheme: () => void
  onPalette: () => void
  onDoctor: () => void
}) {
  const tunnel = state?.tunnel

  return (
    <div className="flex h-10 shrink-0 items-center gap-3.5 border-b border-shell-rule bg-hull px-4">
      <div className="flex shrink-0 items-center gap-[9px]">
        <Mark className="h-5 w-5 text-shell-fg" />
        <div className="font-mono text-14 font-semibold text-shell-fg">kobune</div>
        <div className="font-mono text-12 text-shell-muted">studio</div>
      </div>

      <div className="h-5 w-px bg-shell-rule" />

      <div className="flex items-center gap-[7px] border border-shell-rule bg-shell-chip px-[9px] py-1 font-mono text-12 text-shell-fg">
        <span className="text-shell-muted">project</span>
        <span className="font-semibold">{state?.project ?? '—'}</span>
        <span className="text-10 text-shell-muted">▾</span>
      </div>

      <div className="flex items-center gap-[7px] font-mono text-12 text-shell-muted">
        <span className={connected ? 'text-ink-good' : 'text-ink-bad'}>
          {connected ? '●' : '✗'}
        </span>
        <span>{state ? `kobuned ${state.daemon.version}` : 'kobuned unreachable'}</span>
        {state && (
          <>
            <span className="text-shell-rule">·</span>
            <span>{state.daemon.runtime}</span>
          </>
        )}
      </div>

      <div className="flex-1" />

      <button
        type="button"
        onClick={onPalette}
        className="flex min-w-0 shrink cursor-pointer items-center gap-2 border border-shell-rule bg-shell-chip px-3 py-1 font-mono text-115 text-shell-fg hover:border-bright"
      >
        <span className="shrink-0 text-shell-muted">⌘K</span>
        {/* The first thing to go when the bar runs out of room. The
            shortcut beside it still says what the control is. */}
        <span className="truncate">go to a workspace, or run anything</span>
      </button>

      {/* Only when it is carrying traffic. A chip that said `tunnel ·
          stopped` would be a second place to read a state the sidebar's
          ▲ already reports per workspace. */}
      {tunnel?.running && (
        <div className="flex shrink-0 items-center gap-[7px] border border-shell-wire px-[9px] py-1 font-mono text-11 text-ink-warn">
          <span>▲</span>
          <span>
            tunnel · {tunnel.domain ?? 'cloudflare'} · {tunnel.public ? 'public' : 'restricted'}
          </span>
        </div>
      )}

      <div className="flex shrink-0 gap-1.5">
        <button
          type="button"
          onClick={onDoctor}
          className="cursor-pointer border border-shell-rule px-[9px] py-1 font-mono text-11 text-shell-muted hover:text-shell-fg"
        >
          doctor
        </button>
        <button
          type="button"
          onClick={onToggleTheme}
          aria-label={theme === 'dark' ? 'Switch to the light shell' : 'Switch to the dark shell'}
          className="cursor-pointer border border-shell-rule px-[9px] py-1 font-mono text-11 text-shell-muted hover:text-shell-fg"
        >
          {theme === 'dark' ? '☾' : '☀'}
        </button>
      </div>
    </div>
  )
}

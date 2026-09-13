import type { ServiceInfo, ServiceState } from './types'

/**
 * The state glyphs, which are the CLI's.
 *
 * Someone who has read `kobune status` in a terminal should not have to
 * learn a second vocabulary to read the same thing in a browser.
 */
export const SYMBOL: Record<ServiceState, string> = {
  ready: '●',
  starting: '◐',
  idle: '◑',
  stopped: '○',
  failed: '✗',
  unknown: '○',
}

/** State reads by brightness. Only a failure has any colour in it. */
export const INK: Record<ServiceState, string> = {
  ready: 'text-ink-good',
  starting: 'text-ink-warn',
  idle: 'text-ink-idle',
  stopped: 'text-shell-muted',
  failed: 'text-ink-bad',
  unknown: 'text-shell-muted',
}

export const CHIP: Record<ServiceState, string> = {
  ready: 'bg-chip-ready',
  starting: 'bg-chip-starting',
  idle: 'bg-chip-idle',
  stopped: 'bg-chip-stopped',
  failed: 'bg-chip-failed',
  unknown: 'bg-chip-stopped',
}

/**
 * `ServiceState::is_running()` — starting, ready or idle.
 *
 * The counts on screen come from the server, which uses the Rust
 * predicate. This is only for deciding whether a button says `stop` or
 * `start`, and it is written out here so the two cannot silently disagree
 * about what running means.
 */
export function isRunning(state: ServiceState): boolean {
  return state === 'starting' || state === 'ready' || state === 'idle'
}

/** What a service card's `stop` / `start` button should say. */
export function toggleLabel(service: ServiceInfo): 'stop' | 'start' {
  return isRunning(service.state) ? 'stop' : 'start'
}

/**
 * The address to show, preferring the public one when the tunnel is up.
 *
 * A service keeps its URL when it stops — a request is what starts it, so
 * a URL that came and went with the state would take the way to wake it
 * along (`docs/DESIGN.md`, what M2 turned up).
 */
export function address(service: ServiceInfo, tunnelRunning: boolean): string | null {
  if (tunnelRunning && service.tunnel_url) return service.tunnel_url
  return service.url ?? null
}

/** `vite · :5173 · workspace` — the line under a service's name. */
export function meta(service: ServiceInfo): string {
  const parts: string[] = []
  if (service.image) parts.push(service.image)
  if (service.port != null) parts.push(`:${service.port}`)
  parts.push(service.scope)
  return parts.join(' · ')
}

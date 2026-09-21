import { useState } from 'react'
import { Qr } from './Qr'
import { CHIP, INK, SYMBOL, address, meta, toggleLabel } from '../lib/service'
import type { ServiceInfo } from '../lib/types'

/**
 * One service: what it is doing, where it answers, and the three things
 * worth doing to it.
 *
 * Full width rather than a grid of tiles. The address is the reason the
 * card exists and a tile is not wide enough to show one without
 * truncating it — and a truncated URL is a URL nobody can read off a
 * screen.
 */
export function ServiceCard({
  service,
  tunnelRunning,
  showQr,
  onAct,
  onLogs,
}: {
  service: ServiceInfo
  tunnelRunning: boolean
  showQr: boolean
  onAct: (what: 'restart' | 'up' | 'down') => void
  onLogs: () => void
}) {
  const url = address(service, tunnelRunning)
  const toggle = toggleLabel(service)
  const [copied, setCopied] = useState(false)

  const copy = async () => {
    if (!url) return
    try {
      await navigator.clipboard.writeText(url)
      setCopied(true)
      window.setTimeout(() => setCopied(false), 1200)
    } catch {
      // A denied clipboard is not worth an error state; the URL is on
      // screen and selectable.
    }
  }

  return (
    <div className="flex gap-4 border border-shell-rule bg-shell-raise px-4 py-3.5">
      <div className="flex min-w-0 flex-1 flex-col gap-2.5">
        <div className="flex items-center gap-2.5">
          <span
            className={`font-mono text-13 ${INK[service.state]} ${
              service.state === 'starting' ? 'kb-pulse' : ''
            }`}
          >
            {SYMBOL[service.state]}
          </span>
          <span className="font-mono text-15 font-semibold text-shell-fg">{service.name}</span>
          <span
            className={`font-mono text-11 ${INK[service.state]} ${CHIP[service.state]} px-2 py-0.5`}
          >
            {service.state}
          </span>
          <span className="truncate font-mono text-11 text-shell-muted">{meta(service)}</span>

          <div className="flex-1" />

          <div className="flex gap-1.5">
            <button
              type="button"
              onClick={() => onAct('restart')}
              className="cursor-pointer border border-shell-rule px-2 py-[3px] font-mono text-11 text-shell-muted hover:text-shell-fg"
            >
              restart
            </button>
            <button
              type="button"
              onClick={() => onAct(toggle === 'stop' ? 'down' : 'up')}
              className="cursor-pointer border border-shell-rule px-2 py-[3px] font-mono text-11 text-shell-muted hover:text-shell-fg"
            >
              {toggle}
            </button>
            <button
              type="button"
              onClick={onLogs}
              className="cursor-pointer border border-shell-rule px-2 py-[3px] font-mono text-11 text-ink-command hover:text-shell-fg"
            >
              logs
            </button>
          </div>
        </div>

        {url && (
          <div className="flex items-center gap-2 border border-shell-rule bg-shell-floor px-2.5 py-2">
            <span className="min-w-0 flex-1 truncate font-mono text-13 text-ink-link">{url}</span>
            <button
              type="button"
              onClick={() => void copy()}
              className="cursor-pointer font-mono text-11 text-shell-muted hover:text-shell-fg"
            >
              {copied ? '✓ copied' : '⧉ copy'}
            </button>
            <a
              href={url}
              target="_blank"
              rel="noreferrer"
              className="cursor-pointer font-mono text-11 text-shell-muted hover:text-shell-fg"
            >
              ↗ open
            </a>
          </div>
        )}

        {/* A service with no port is not broken — `scope = "project"`
            databases are reachable from inside the network and nowhere
            else. Saying so is what stops it reading as a missing URL. */}
        {!url && (
          <div className="font-mono text-12 text-shell-muted">
            internal only — reachable from inside the network
          </div>
        )}

        {service.state === 'failed' && service.reason && (
          <div className="font-mono text-12 text-ink-bad">{service.reason}</div>
        )}
      </div>

      {showQr && url && (
        <Qr value={url} size={76} label={tunnelRunning && service.tunnel_url ? 'TUNNEL' : 'LOCAL'} />
      )}
    </div>
  )
}

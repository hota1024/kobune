import { useEffect, useRef } from 'react'
import { parseAnsi } from '../lib/ansi'
import type { LogLine } from '../lib/types'

/**
 * Service tag colours, assigned by position.
 *
 * The design gives every service its own colour so a mixed stream can be
 * read down the left edge. Which colour is not meaningful, so it comes
 * from the order the services are defined in rather than from a table of
 * names that would only ever match one example project.
 */
const TAGS = ['text-ink-link', 'text-ink-command', 'text-ink-idle', 'text-ink-warn']

/**
 * `Event::Output` carries no timestamp — it is the container's line, byte
 * for byte. This is when the line reached the page, which for a follow is
 * within a few milliseconds of when it was written, and is the only
 * honest thing to put in the column.
 */
export interface Stamped extends LogLine {
  at: string
  seq: number
}

export function LogPane({
  lines,
  workspace,
  services,
  filter,
  following,
  height,
  onFilter,
  onResize,
  onExpand,
  expanded,
}: {
  lines: Stamped[]
  workspace: string
  services: string[]
  filter: string | null
  following: boolean
  height: number
  onFilter: (service: string | null) => void
  onResize: (height: number) => void
  onExpand: () => void
  expanded: boolean
}) {
  const body = useRef<HTMLDivElement>(null)
  const dragging = useRef(false)

  // Follow the tail. Only when already at the bottom, so reading back
  // through a burst is not fought by every line that arrives.
  useEffect(() => {
    const element = body.current
    if (!element) return
    const atBottom = element.scrollHeight - element.scrollTop - element.clientHeight < 40
    if (atBottom) element.scrollTop = element.scrollHeight
  }, [lines])

  useEffect(() => {
    const move = (event: MouseEvent) => {
      if (!dragging.current) return
      const next = window.innerHeight - event.clientY - 28
      onResize(Math.max(80, Math.min(next, window.innerHeight - 260)))
    }
    const up = () => {
      dragging.current = false
      document.body.style.userSelect = ''
    }

    window.addEventListener('mousemove', move)
    window.addEventListener('mouseup', up)
    return () => {
      window.removeEventListener('mousemove', move)
      window.removeEventListener('mouseup', up)
    }
  }, [onResize])

  const tagOf = (service?: string) =>
    service ? TAGS[Math.max(0, services.indexOf(service)) % TAGS.length] : 'text-shell-muted'

  return (
    <div className="flex shrink-0 flex-col border-t border-shell-rule bg-shell-floor">
      <div
        onMouseDown={() => {
          dragging.current = true
          document.body.style.userSelect = 'none'
        }}
        className="flex h-[5px] cursor-ns-resize items-center justify-center bg-shell-gutter"
      >
        <div className="h-0.5 w-11 bg-shell-rule" />
      </div>

      <div className="flex h-[30px] shrink-0 items-center gap-2.5 px-4">
        <span className="font-mono text-11 font-semibold tracking-[.12em] text-ink-heading uppercase">
          logs
        </span>
        <span className="font-mono text-12 font-semibold text-shell-fg">{workspace}</span>

        <div className="flex gap-1">
          <button
            type="button"
            onClick={() => onFilter(null)}
            className={`cursor-pointer px-[9px] py-[3px] font-mono text-11 ${
              filter === null
                ? 'bg-primary text-on-primary'
                : 'text-shell-muted hover:text-shell-fg'
            }`}
          >
            all
          </button>
          {services.map((service) => (
            <button
              key={service}
              type="button"
              onClick={() => onFilter(service)}
              className={`cursor-pointer px-[9px] py-[3px] font-mono text-11 ${
                filter === service
                  ? 'bg-primary text-on-primary'
                  : 'text-shell-muted hover:text-shell-fg'
              }`}
            >
              {service}
            </button>
          ))}
        </div>

        <div className="flex-1" />

        <span
          className={`font-mono text-11 ${
            following ? 'kb-pulse text-ink-good' : 'text-shell-muted'
          }`}
        >
          {following ? '● following' : '○ not following'}
        </span>
        <span className="font-mono text-11 text-shell-muted">
          {lines.length.toLocaleString()} lines
        </span>
        <button
          type="button"
          onClick={onExpand}
          className="cursor-pointer border border-shell-rule px-[7px] py-0.5 font-mono text-11 text-shell-muted hover:text-shell-fg"
        >
          {expanded ? '⤡ shrink' : '⤢ full'}
        </button>
      </div>

      <div
        ref={body}
        style={{ height }}
        className="flex flex-col gap-0.5 overflow-y-auto px-4 pb-2.5"
      >
        {lines.length === 0 && (
          <div className="font-mono text-12 text-shell-muted">
            Nothing yet. A stopped service has no stream to follow — it wakes on a request.
          </div>
        )}
        {lines.map((line) => (
          <div key={line.seq} className="flex gap-3 font-mono text-12 leading-[1.55]">
            <span className={`w-[88px] shrink-0 truncate ${tagOf(line.service)}`}>
              {line.service ?? ''}
            </span>
            <span className="w-[62px] shrink-0 text-shell-muted">{line.at}</span>
            <span
              className={`min-w-0 flex-1 break-all whitespace-pre-wrap ${
                line.stream === 'stderr'
                  ? 'text-ink-warn'
                  : line.stream === 'log'
                    ? 'text-shell-muted'
                    : 'text-shell-fg'
              }`}
            >
              {/* The line's own escape sequences win over the stream's
                  default: a tool that said "this is an error" knows more
                  than the fact that it came down stderr. */}
              {parseAnsi(line.text).map((segment, index) => (
                <span key={index} className={segment.className}>
                  {segment.text}
                </span>
              ))}
            </span>
          </div>
        ))}
      </div>
    </div>
  )
}

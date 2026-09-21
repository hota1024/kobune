import { Dialog } from '@base-ui/react/dialog'
import type { CheckStatus, DoctorView, EnvView } from '../lib/types'

const CHECK_SYMBOL: Record<CheckStatus, string> = { ok: '✓', warn: '!', fail: '✗' }
const CHECK_INK: Record<CheckStatus, string> = {
  ok: 'text-ink-good',
  warn: 'text-ink-warn',
  fail: 'text-ink-bad',
}

/**
 * `doctor` and `env`, over the top of the screen.
 *
 * Overlays rather than panes, for the reason §10 gives the TUI: each is
 * one request and one key, and neither is something you watch — you open
 * it, read it, and it goes away.
 */
function Frame({
  open,
  title,
  subtitle,
  onOpenChange,
  children,
}: {
  open: boolean
  title: string
  subtitle?: string
  onOpenChange: (open: boolean) => void
  children: React.ReactNode
}) {
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Backdrop className="fixed inset-0 z-40 bg-scrim" />
        <Dialog.Popup className="fixed top-[92px] left-1/2 z-50 flex max-h-[70vh] w-[640px] max-w-[calc(100vw-32px)] -translate-x-1/2 flex-col border border-shell-edge bg-shell-palette kb-float outline-none">
          <div className="flex items-center gap-2.5 border-b border-shell-rule px-[15px] py-[13px]">
            <Dialog.Title className="font-mono text-11 font-semibold tracking-[.12em] text-ink-heading uppercase">
              {title}
            </Dialog.Title>
            {subtitle && <span className="font-mono text-105 text-shell-muted">{subtitle}</span>}
            <div className="flex-1" />
            <Dialog.Close className="cursor-pointer font-mono text-105 text-shell-muted hover:text-shell-fg">
              esc
            </Dialog.Close>
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto">{children}</div>
        </Dialog.Popup>
      </Dialog.Portal>
    </Dialog.Root>
  )
}

export function DoctorOverlay({
  open,
  doctor,
  onOpenChange,
}: {
  open: boolean
  doctor: DoctorView | null
  onOpenChange: (open: boolean) => void
}) {
  return (
    <Frame open={open} title="doctor" onOpenChange={onOpenChange}>
      {!doctor && <div className="px-[15px] py-4 font-mono text-12 text-shell-muted">asking…</div>}
      {doctor?.checks.map((check) => (
        <div
          key={check.id}
          className="flex items-baseline gap-3 border-b border-shell-rule/50 px-[15px] py-2.5 last:border-b-0"
        >
          {/* The CLI's symbols, so the two screens read the same. */}
          <span className={`font-mono text-12 ${CHECK_INK[check.status]}`}>
            {CHECK_SYMBOL[check.status]}
          </span>
          <span className="w-[180px] shrink-0 font-mono text-12 text-shell-fg">{check.title}</span>
          <span className="min-w-0 flex-1 font-mono text-115 text-shell-muted">
            {check.detail}
            {/* The command that fixes it is the actionable half, so it is
                selectable text rather than prose about it. */}
            {check.fix && (
              <code className="mt-1 block text-ink-command select-all">{check.fix}</code>
            )}
          </span>
        </div>
      ))}
    </Frame>
  )
}

export function EnvOverlay({
  open,
  env,
  workspace,
  onOpenChange,
}: {
  open: boolean
  env: EnvView | null
  workspace: string | null
  onOpenChange: (open: boolean) => void
}) {
  return (
    <Frame
      open={open}
      title="env"
      subtitle={workspace ?? undefined}
      onOpenChange={onOpenChange}
    >
      {/* Masked, and there is no button to unmask. See `api.rs`. */}
      {!env && <div className="px-[15px] py-4 font-mono text-12 text-shell-muted">asking…</div>}
      {env?.entries.length === 0 && (
        <div className="px-[15px] py-4 font-mono text-12 text-shell-muted">
          Nothing set beyond what the runtime injects.
        </div>
      )}
      {env?.entries.map((entry) => (
        <div
          key={entry.key}
          className="flex items-baseline gap-3 border-b border-shell-rule/50 px-[15px] py-2 last:border-b-0"
        >
          <span className="w-[200px] shrink-0 truncate font-mono text-12 text-shell-fg">
            {entry.key}
          </span>
          <span
            className={`min-w-0 flex-1 truncate font-mono text-115 ${
              entry.secret ? 'text-ink-idle' : 'text-shell-muted'
            }`}
          >
            {entry.value}
          </span>
          <span className="shrink-0 font-mono text-10 tracking-[.1em] text-shell-muted uppercase">
            {entry.secret ? 'secret' : entry.scope}
          </span>
        </div>
      ))}
    </Frame>
  )
}

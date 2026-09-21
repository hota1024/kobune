import { useEffect, useMemo, useRef, useState } from 'react'
import { Autocomplete } from '@base-ui/react/autocomplete'
import { Dialog } from '@base-ui/react/dialog'
import { narrow, type Command, type CommandGroup } from '../lib/commands'

/**
 * ⌘K — the navigation model, over whatever is already on screen.
 *
 * Base UI's `Dialog` supplies the modal half: the scrim, the focus trap,
 * escape, and the aria wiring that makes a floating list announce itself.
 * `Autocomplete` in `inline` mode supplies the list half — highlight,
 * typeahead and `Enter` on the highlighted row — without a popup of its
 * own, because the panel it would float in is the dialog.
 *
 * Filtering is `filteredItems` rather than Base UI's own `filter`, so a
 * query can match a row's detail as well as its label.
 */
export function CommandPalette({
  open,
  groups,
  scope,
  only,
  onOpenChange,
}: {
  open: boolean
  groups: CommandGroup[]
  /**
   * `goto` is ⌘P — the same list scoped to workspaces alone, for when you
   * already know you are switching. A control that were a synonym for ⌘K
   * while being advertised separately would read as broken.
   */
  only: 'all' | 'goto'
  /** The workspace the top groups act on, shown at the end of the prompt. */
  scope: string | null
  onOpenChange: (open: boolean) => void
}) {
  const [query, setQuery] = useState('')
  const input = useRef<HTMLInputElement>(null)

  const [highlighted, setHighlighted] = useState<Command | null>(null)

  const scoped = useMemo(
    () => (only === 'goto' ? groups.filter((group) => group.value === 'go to') : groups),
    [groups, only],
  )
  const filtered = useMemo(() => narrow(scoped, query), [scoped, query])

  // Put the highlight on the first row as soon as the palette opens.
  //
  // `autoHighlight` only covers *filtering* — it starts highlighting once
  // something has been typed, so a palette opened and immediately given
  // `Enter` would do nothing while its own footer said `↵ run`. One
  // ArrowDown through the component's own key handling is what a person
  // would otherwise have to press, and it leaves Base UI to decide which
  // row is first rather than reaching past it into the list.
  useEffect(() => {
    if (!open) return
    const timer = window.setTimeout(() => {
      input.current?.dispatchEvent(
        new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }),
      )
    }, 0)
    return () => window.clearTimeout(timer)
  }, [open])

  const run = (command: Command) => {
    onOpenChange(false)
    setQuery('')
    command.run()
  }

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(next) => {
        onOpenChange(next)
        if (!next) setQuery('')
      }}
    >
      <Dialog.Portal>
        <Dialog.Backdrop className="fixed inset-0 z-40 bg-scrim" />
        <Dialog.Popup
          aria-label="Command palette"
          onKeyDown={(event) => {
            // ⌘↵ copies the highlighted row's address. The rows advertise
            // it, so it has to exist.
            if ((event.metaKey || event.ctrlKey) && event.key === 'Enter') {
              event.preventDefault()
              const url = highlighted?.copy
              if (url) {
                void navigator.clipboard.writeText(url).catch(() => {})
                onOpenChange(false)
                setQuery('')
              }
            }
          }}
          className="fixed top-[92px] left-1/2 z-50 w-[600px] max-w-[calc(100vw-32px)] -translate-x-1/2 border border-shell-edge bg-shell-palette kb-float outline-none"
        >
          <Autocomplete.Root
            inline
            open
            // The footer promises `↵ run`, so something has to be under it
            // before a key is pressed. Without this the first Enter does
            // nothing, which reads as the palette being broken.
            autoHighlight
            items={scoped}
            filteredItems={filtered}
            value={query}
            onValueChange={setQuery}
            onItemHighlighted={(command) => setHighlighted((command as Command) ?? null)}
          >
            <div className="flex items-center gap-2.5 border-b border-shell-rule px-[15px] py-[13px]">
              <span className="font-mono text-14 text-shell-muted">&gt;</span>
              <Autocomplete.Input
                ref={input}
                autoFocus
                placeholder={
                  only === 'goto' ? 'go to a workspace' : 'go to a workspace, or run anything'
                }
                className="min-w-0 flex-1 bg-transparent font-mono text-14 text-shell-fg outline-none placeholder:text-shell-muted"
              />
              {scope && <span className="font-mono text-105 text-shell-muted">{scope}</span>}
            </div>

            <Autocomplete.Empty>
              <div className="px-[15px] py-4 font-mono text-12 text-shell-muted">
                Nothing matches. `esc` dismisses.
              </div>
            </Autocomplete.Empty>

            <Autocomplete.List className="max-h-[46vh] overflow-y-auto outline-none">
              {(group: CommandGroup) => (
                <Autocomplete.Group key={group.value} items={group.items} className="block">
                  <Autocomplete.GroupLabel className="px-[15px] pt-[9px] pb-[3px] font-mono text-10 tracking-[.12em] text-shell-muted uppercase select-none">
                    {group.value}
                  </Autocomplete.GroupLabel>
                  <Autocomplete.Collection>
                    {(command: Command) => (
                      <Autocomplete.Item
                        key={command.id}
                        value={command}
                        onClick={() => run(command)}
                        className="group flex cursor-pointer items-center gap-3 border-l-2 border-l-transparent px-[15px] py-[7px] outline-none select-none data-highlighted:border-l-bright data-highlighted:bg-shell-high"
                      >
                        {/* `group-data-highlighted:`, not `data-highlighted:`.
                            Base UI marks the Item, and Tailwind's bare
                            variant compiles to `&[data-highlighted]` — a
                            selector on this span, which never carries it. */}
                        <span className="font-mono text-125 text-shell-soft group-data-highlighted:text-shell-fg">
                          {command.label}
                        </span>
                        <span className="min-w-0 flex-1 truncate font-mono text-105 text-shell-muted">
                          {command.detail}
                        </span>
                        {command.keys && (
                          <span className="shrink-0 font-mono text-105 text-ink-command">
                            {command.keys}
                          </span>
                        )}
                      </Autocomplete.Item>
                    )}
                  </Autocomplete.Collection>
                </Autocomplete.Group>
              )}
            </Autocomplete.List>

            <div className="flex items-center gap-3.5 border-t border-shell-rule px-[15px] py-[7px] font-mono text-105 text-shell-muted">
              <span>
                <span className="text-ink-command">↵</span> run
              </span>
              <span>
                <span className="text-ink-command">⌘↵</span> copy
              </span>
              <span>
                <span className="text-ink-command">esc</span> dismiss
              </span>
              <div className="flex-1" />
              <span>a match opens the workspace as a new tab</span>
            </div>
          </Autocomplete.Root>
        </Dialog.Popup>
      </Dialog.Portal>
    </Dialog.Root>
  )
}

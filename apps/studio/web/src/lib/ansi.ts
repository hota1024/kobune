// Container output is a terminal's, byte for byte — `Event::Output` passes
// it through untouched, escape sequences and all. `docs/DESIGN.md` §10
// settled this for the TUI's log pane: the sequences have to be *read*,
// not printed, or every Vite banner arrives as bracket-m noise.
//
// **The colours are mapped into the palette rather than reproduced.** The
// screen reads by brightness and keeps one chromatic colour for a failure;
// sixteen terminal colours dropped into it would undo that. So red becomes
// the failure ink, yellow the warning ink, and everything else lands on a
// grey — the emphasis the tool intended survives, the palette does not
// break.

export interface Segment {
  text: string
  className: string
}

const ESC = String.fromCharCode(27)
const BEL = String.fromCharCode(7)

/** SGR parameter to the class it turns on. */
const FOREGROUND: Record<number, string> = {
  30: 'text-shell-muted',
  31: 'text-ink-bad',
  32: 'text-ink-good',
  33: 'text-ink-warn',
  34: 'text-ink-link',
  35: 'text-ink-link',
  36: 'text-ink-link',
  37: 'text-shell-fg',
  39: '',
  90: 'text-shell-muted',
  91: 'text-ink-bad',
  92: 'text-ink-good',
  93: 'text-ink-warn',
  94: 'text-ink-link',
  95: 'text-ink-link',
  96: 'text-ink-link',
  97: 'text-shell-fg',
}

// CSI (colour and cursor), OSC (window titles, hyperlinks), and the stray
// single-character escapes a progress spinner emits.
const ANSI = new RegExp(
  ESC + '(?:\\[[0-9;?]*[ -/]*[@-~]|\\][\\s\\S]*?(?:' + BEL + '|' + ESC + '\\\\)|[@-Z\\\\-_])',
  'g',
)

interface Pen {
  colour: string
  bold: boolean
  dim: boolean
}

const BLANK: Pen = { colour: '', bold: false, dim: false }

function apply(pen: Pen, params: number[]): Pen {
  let next = { ...pen }

  for (let i = 0; i < params.length; i++) {
    const code = params[i]

    if (code === 0) next = { ...BLANK }
    else if (code === 1) next.bold = true
    else if (code === 2) next.dim = true
    else if (code === 22) {
      next.bold = false
      next.dim = false
    } else if (code in FOREGROUND) next.colour = FOREGROUND[code]
    // 38;5;n and 38;2;r;g;b — 256-colour and truecolour. There is nowhere
    // in a greyscale palette to put them, so their parameters are stepped
    // over rather than misread as separate codes.
    else if (code === 38 || code === 48) {
      if (params[i + 1] === 5) i += 2
      else if (params[i + 1] === 2) i += 4
    }
  }

  return next
}

function classOf(pen: Pen): string {
  const parts: string[] = []
  if (pen.dim) parts.push('text-shell-muted')
  else if (pen.colour) parts.push(pen.colour)
  if (pen.bold) {
    parts.push('font-semibold')
    if (!pen.colour && !pen.dim) parts.push('text-shell-fg')
  }
  return parts.join(' ')
}

/**
 * Splits a line into styled runs.
 *
 * A single run with an empty class is the common case — most log lines
 * carry no escapes at all — and is returned as such so the renderer can
 * skip the spans entirely.
 */
export function parseAnsi(line: string): Segment[] {
  if (!line.includes(ESC)) return [{ text: line, className: '' }]

  const segments: Segment[] = []
  let pen = BLANK
  let cursor = 0

  ANSI.lastIndex = 0
  let match: RegExpExecArray | null

  while ((match = ANSI.exec(line)) !== null) {
    if (match.index > cursor) {
      segments.push({ text: line.slice(cursor, match.index), className: classOf(pen) })
    }

    const sequence = match[0]
    // Only SGR (ESC-bracket ... m) moves the pen. Everything else — cursor
    // moves, erases, window titles — is dropped, which is the right
    // reading for a pane that scrolls rather than addresses cells.
    if (sequence.startsWith(ESC + '[') && sequence.endsWith('m')) {
      const params = sequence
        .slice(2, -1)
        .split(';')
        .map((part) => (part === '' ? 0 : Number(part)))
        .filter((value) => Number.isFinite(value))
      pen = apply(pen, params)
    }

    cursor = match.index + sequence.length
  }

  if (cursor < line.length) {
    segments.push({ text: line.slice(cursor), className: classOf(pen) })
  }

  return segments.filter((segment) => segment.text.length > 0)
}

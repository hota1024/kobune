/**
 * The session the home page plays.
 *
 * Every command and every line of output here is what the CLI prints. The
 * strings were taken from `apps/cli/src/ui/views.rs`, `progress.rs` and
 * `theme.rs` — including which style each run of characters is drawn in — and
 * the panels are not typed out at all: `panel()` and `grid()` below lay them
 * out the way `panel.rs` does, from the content, so a panel cannot come out
 * one column short of its own border.
 *
 * The one thing here the tool did not print is the two `│`-prefixed lines in
 * act two and three. Those are the demo application's own output, arriving
 * through `kobune logs`. `myapp` is fictional throughout the documentation
 * already; its log lines are as fictional as its name.
 */

/**
 * The nine styles `apps/cli/src/ui/theme.rs` hands out.
 *
 * Named for intent rather than for colour, because the CLI is: it asks for
 * `theme::link()` and the palette decides what that means. This site is one
 * more terminal theme, and `styles/stage.css` is where it decides.
 */
export type Ink =
  | 'plain'
  | 'muted' // DarkGray — labels, paths, punctuation
  | 'subject' // bold — the name of a thing
  | 'heading' // DarkGray + bold — column headings
  | 'link' // Cyan — URLs
  | 'command' // Magenta — something to type
  | 'good' // Green — ✓, ready
  | 'warn' // Yellow — the spinner, warnings
  | 'bad' // Red — ✗, failed

/** A run of characters in one style. */
export type Span = readonly [ink: Ink, text: string]

/** One printed row. Joining the spans gives the line back byte for byte. */
export type Row = readonly Span[]

export type SurfaceId = 'shell-a' | 'shell-b' | 'web-a' | 'web-b'

/** Which page the browser draws. A shape, not a screenshot. */
export type PageId = 'shop' | 'shop-auth' | 'shop-cart'

export type ActId = 'init' | 'branch' | 'parallel' | 'preview'

export type Beat =
  /** A command, one character at a time, after the `$ `. */
  | { readonly beat: 'type'; readonly command: string }
  /** Output that is simply there: a panel, a bare line. */
  | { readonly beat: 'print'; readonly rows: readonly Row[] }
  /** `  ⠙ label` while it runs, then `  ✓ label` once it is done. */
  | { readonly beat: 'step'; readonly label: string; readonly ms: number }
  /** The shell moves, and the window title with it. */
  | { readonly beat: 'chdir'; readonly to: string }
  /** Nothing, for as long as it takes to read what is on screen. */
  | { readonly beat: 'hold'; readonly ms: number }
  /** A browser goes somewhere. */
  | { readonly beat: 'visit'; readonly url: string; readonly page: PageId; readonly ms: number }

/**
 * One surface's run of beats.
 *
 * Lanes inside an act run together, and that is the whole of the parallelism:
 * the second worktree needs no scheduler, it is a second lane that opens part
 * of the way into the same act.
 */
export interface Lane {
  readonly on: SurfaceId
  /** Milliseconds after the act opens. */
  readonly from?: number
  readonly beats: readonly Beat[]
}

export interface Act {
  readonly id: ActId
  readonly lanes: readonly Lane[]
}

// ---------------------------------------------------------------------------
// Drawing, the way `apps/cli/src/ui/panel.rs` draws
// ---------------------------------------------------------------------------

const BLANK: Row = []

/** Columns a row occupies. Console output is never translated, so one code point is one column. */
export function width(row: Row): number {
  let n = 0
  for (const [, text] of row) n += [...text].length
  return n
}

function pad(n: number): string {
  return ' '.repeat(Math.max(0, n))
}

/**
 * A framed panel, sized to its content.
 *
 * Rounded corners and a title padded with a space either side, matching
 * `theme::frame()` — a title that is not padded comes out welded to the
 * corner. The border is muted, as it is in the terminal.
 */
export function panel(title: Row, rows: readonly Row[]): Row[] {
  const inner = Math.max(width(title), ...rows.map(width))
  const dashes = inner - width(title)

  return [
    [['muted', '╭ '], ...title, ['muted', ` ${'─'.repeat(dashes)}╮`]],
    ...rows.map((row): Row => [['muted', '│ '], ...row, ['plain', pad(inner - width(row))], ['muted', ' │']]),
    [['muted', `╰${'─'.repeat(inner + 2)}╯`]],
  ]
}

/**
 * Columns aligned to their widest cell, two spaces apart.
 *
 * `panel.rs` spaces its grids by two and sizes every column to its content;
 * writing that out by hand is how a table ends up half a space off.
 */
export function grid(rows: readonly (readonly Row[])[], spacing = 2): Row[] {
  const columns = Math.max(...rows.map((cells) => cells.length))
  const widths = Array.from({ length: columns }, (_, i) => Math.max(...rows.map((cells) => (cells[i] ? width(cells[i]) : 0))))

  return rows.map((cells): Row => {
    const out: Span[] = []
    cells.forEach((cell, i) => {
      out.push(...cell)
      if (i < cells.length - 1) out.push(['plain', pad(widths[i] - width(cell) + spacing)])
    })
    return out
  })
}

/** `● web  ready  https://…` — the row `views::workspace` builds for a service. */
function service(name: string, url: string): Row[] {
  return [[['good', '●'], ['plain', ' '], ['subject', name]], [['good', 'ready']], [['link', url]]]
}

/** `› <text> <command>` — `views::hint`, with the command in magenta. */
function hint(text: string, command: string): Row {
  return [
    ['muted', `› ${text} `],
    ['command', command],
  ]
}

const HOME = '~/myapp'
const WT = '~/myapp.wt'

// ---------------------------------------------------------------------------
// The panels
// ---------------------------------------------------------------------------

const INIT = panel(
  [['plain', 'init']],
  [
    ...grid([
      [[['muted', 'created']], [['plain', `${HOME}/kobune.toml`]]],
      [[['muted', 'project']], [['plain', 'myapp']]],
    ]),
    BLANK,
    hint('bring the environment up with', 'kobune up'),
  ],
)

/** `views::workspace` — the title is `project / workspace`, the project bold. */
function workspace(name: string, branch: string, path: string, url: string): Row[] {
  return panel([['subject', 'myapp'], ['muted', ' / '], ['subject', name]], [
    [['muted', `${branch}  ${path}`]],
    BLANK,
    ...grid([service('web', url)]),
  ])
}

const MAIN_UP = workspace('(main)', 'main', HOME, 'https://web.myapp.localhost')

const AUTH_UP = workspace(
  'feature-user-auth',
  'feature/user-auth',
  `${WT}/feature-user-auth`,
  'https://web.feature-user-auth.myapp.localhost',
)

const CART_UP = workspace(
  'fix-checkout-total',
  'fix/checkout-total',
  `${WT}/fix-checkout-total`,
  'https://web.fix-checkout-total.myapp.localhost',
)

/** `views::workspaces` — `PROJECT` is left out, because only one project is listed. */
const LS = panel(
  [['plain', 'workspaces']],
  grid([
    [[['heading', 'WORKSPACE']], [['heading', 'SERVICES']], [['heading', 'BRANCH']]],
    [[['subject', '(main)']], [['good', '1/1']], [['muted', 'main']]],
    [[['subject', 'feature-user-auth']], [['good', '1/1']], [['muted', 'feature/user-auth']]],
    [[['subject', 'fix-checkout-total']], [['good', '1/1']], [['muted', 'fix/checkout-total']]],
  ]),
)

/** `  │ ` in front of a line a container wrote — `progress.rs`. */
function log(line: string): Row {
  return [
    ['muted', '  │ '],
    ['plain', line],
  ]
}

// ---------------------------------------------------------------------------
// The session
// ---------------------------------------------------------------------------

export const SCRIPT: readonly Act[] = [
  {
    id: 'init',
    lanes: [
      {
        on: 'shell-a',
        beats: [
          { beat: 'hold', ms: 500 },
          { beat: 'type', command: 'kobune init' },
          { beat: 'print', rows: INIT },
          { beat: 'hold', ms: 1000 },
          { beat: 'type', command: 'kobune up' },
          // The order `kobune up` reports, from `docker.rs` and `health.rs`.
          { beat: 'step', label: 'preparing the network', ms: 700 },
          { beat: 'step', label: 'pulling image node:22', ms: 1100 },
          { beat: 'step', label: 'starting web', ms: 800 },
          { beat: 'step', label: 'waiting for web', ms: 900 },
          { beat: 'print', rows: MAIN_UP },
          { beat: 'hold', ms: 1200 },
        ],
      },
    ],
  },

  {
    id: 'branch',
    lanes: [
      {
        on: 'shell-a',
        beats: [
          { beat: 'type', command: 'kobune new feature/user-auth' },
          // Three steps, not four: `kobune up` pulled node:22 a moment ago,
          // and a step that has nothing to do is not reported.
          { beat: 'step', label: 'creating worktree feature/user-auth', ms: 1000 },
          { beat: 'step', label: 'starting web', ms: 800 },
          { beat: 'step', label: 'waiting for web', ms: 900 },
          { beat: 'print', rows: AUTH_UP },
          { beat: 'hold', ms: 1200 },
          { beat: 'type', command: `cd ${WT}/feature-user-auth` },
          { beat: 'chdir', to: `${WT}/feature-user-auth` },
          { beat: 'type', command: 'kobune logs -f web' },
          { beat: 'hold', ms: 400 },
          { beat: 'print', rows: [log('ready in 412 ms')] },
          { beat: 'hold', ms: 700 },
          { beat: 'print', rows: [log('GET /sign-in 200')] },
          { beat: 'hold', ms: 700 },
        ],
      },
      {
        on: 'web-a',
        from: 4800,
        beats: [{ beat: 'visit', url: 'web.feature-user-auth.myapp.localhost', page: 'shop-auth', ms: 700 }],
      },
    ],
  },

  {
    id: 'parallel',
    lanes: [
      // The first worktree is not stopped, paused or waited on. It is still
      // tailing its logs while the second one is built, which is the whole
      // point of the act.
      {
        on: 'shell-a',
        from: 2600,
        beats: [
          { beat: 'print', rows: [log('GET /assets/app.js 200')] },
          { beat: 'hold', ms: 2600 },
          { beat: 'print', rows: [log('POST /sign-in 302')] },
        ],
      },
      {
        on: 'shell-b',
        from: 700,
        beats: [
          { beat: 'type', command: 'kobune new fix/checkout-total' },
          { beat: 'step', label: 'creating worktree fix/checkout-total', ms: 1000 },
          { beat: 'step', label: 'starting web', ms: 800 },
          { beat: 'step', label: 'waiting for web', ms: 900 },
          { beat: 'print', rows: CART_UP },
          { beat: 'hold', ms: 1800 },
          { beat: 'type', command: 'kobune ls' },
          { beat: 'print', rows: LS },
          { beat: 'hold', ms: 1600 },
        ],
      },
      {
        on: 'web-b',
        from: 5200,
        beats: [{ beat: 'visit', url: 'web.fix-checkout-total.myapp.localhost', page: 'shop-cart', ms: 700 }],
      },
    ],
  },

  {
    id: 'preview',
    lanes: [{ on: 'shell-a', beats: [{ beat: 'hold', ms: 4600 }] }],
  },
]

/** Where each shell starts. The window title is the directory, as a terminal's is. */
export const CWD: Record<Extract<SurfaceId, 'shell-a' | 'shell-b'>, string> = {
  'shell-a': HOME,
  'shell-b': HOME,
}

/** Joining a row's spans gives the line back exactly as it was printed. */
export function text(row: Row): string {
  return row.map(([, cell]) => cell).join('')
}

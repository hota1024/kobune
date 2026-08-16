/**
 * The clock behind the demo.
 *
 * The script is compiled once into cues with absolute times, and after that
 * what is on screen is a function of `t` and nothing else. That is what makes
 * replay, the chapter buttons, a backgrounded tab and reduced motion the same
 * mechanism rather than four: they all only decide what `t` is.
 *
 * There is one `requestAnimationFrame` loop on the page, and it does one
 * thing — move `t`. No timers, no per-line callbacks, nothing to drift.
 */
import { computed, onUnmounted, ref, type ComputedRef, type Ref } from 'vue'
import { type Act, type ActId, type Beat, type PageId, type Row, type SurfaceId, CWD, SCRIPT, text } from './script'

/** How fast the command appears. Fast enough to not be waited on, slow enough to read. */
const MS_PER_CHAR = 42

/** The beat between the last character and the first line of output. */
const ENTER = 260

/** Output arrives at once, as it does in a terminal. This is only dwell. */
const PRINT = 150

const CHDIR = 220

/**
 * `apps/cli/src/ui/progress.rs` ticks at 120ms and advances the glyph every
 * second tick, so a frame lasts 240ms. The same number here, because the
 * point is that this is the tool and not an impression of it.
 */
const SPINNER = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'] as const
const SPINNER_MS = 240

const CURSOR_MS = 530

export interface Cue {
  readonly on: SurfaceId
  readonly at: number
  readonly until: number
  readonly beat: Beat
}

export interface Chapter {
  readonly id: ActId
  readonly at: number
}

export interface Timeline {
  readonly cues: readonly Cue[]
  readonly chapters: readonly Chapter[]
  readonly duration: number
}

function span(beat: Beat): number {
  switch (beat.beat) {
    case 'type':
      return beat.command.length * MS_PER_CHAR + ENTER
    case 'print':
      return PRINT
    case 'step':
      return beat.ms
    case 'chdir':
      return CHDIR
    case 'hold':
      return beat.ms
    case 'visit':
      return beat.ms
  }
}

export function compile(script: readonly Act[] = SCRIPT): Timeline {
  const cues: Cue[] = []
  const chapters: Chapter[] = []
  let start = 0

  for (const act of script) {
    chapters.push({ id: act.id, at: start })
    let end = start

    for (const lane of act.lanes) {
      let at = start + (lane.from ?? 0)
      for (const beat of lane.beats) {
        const until = at + span(beat)
        cues.push({ on: lane.on, at, until, beat })
        at = until
      }
      end = Math.max(end, at)
    }

    start = end
  }

  cues.sort((a, b) => a.at - b.at)

  return { cues, chapters, duration: start }
}

// ---------------------------------------------------------------------------
// Reading a frame off the clock
// ---------------------------------------------------------------------------

/**
 * How many of a surface's cues have finished.
 *
 * Everything settled is a prefix of the cue list, so this one number is the
 * whole of the scrollback — and because it is a number, the rows below it are
 * rebuilt when a line lands rather than sixty times a second.
 */
export function settled(timeline: Timeline, on: SurfaceId, t: number): number {
  let n = 0
  for (const cue of timeline.cues) {
    if (cue.on !== on) continue
    if (t >= cue.until) n += 1
  }
  return n
}

export function scrollback(timeline: Timeline, on: SurfaceId, count: number): Row[] {
  const rows: Row[] = []
  let seen = 0

  for (const cue of timeline.cues) {
    if (cue.on !== on) continue
    if (seen >= count) break
    seen += 1

    if (cue.beat.beat === 'type') rows.push(prompt(cue.beat.command))
    if (cue.beat.beat === 'step') rows.push(done(cue.beat.label))
    if (cue.beat.beat === 'print') rows.push(...cue.beat.rows)
  }

  return rows
}

/** The row still being written: a half-typed command, or a step that is running. */
export function live(timeline: Timeline, on: SurfaceId, t: number): Row | null {
  for (const cue of timeline.cues) {
    if (cue.on !== on || t < cue.at || t >= cue.until) continue

    if (cue.beat.beat === 'type') {
      const typed = Math.floor((t - cue.at) / MS_PER_CHAR)
      const shown = cue.beat.command.slice(0, Math.min(cue.beat.command.length, typed))
      const caret = Math.floor(t / CURSOR_MS) % 2 === 0
      return [['muted', '$ '], ['plain', shown], ['plain', caret ? '▌' : ' ']]
    }

    if (cue.beat.beat === 'step') {
      const glyph = SPINNER[Math.floor((t - cue.at) / SPINNER_MS) % SPINNER.length]
      return [['warn', `  ${glyph} `], ['subject', cue.beat.label]]
    }
  }

  return null
}

/** Where the shell is. `chdir` moves it, and the window title follows. */
export function cwd(timeline: Timeline, on: SurfaceId, t: number): string {
  let where = CWD[on as 'shell-a' | 'shell-b'] ?? ''
  for (const cue of timeline.cues) {
    if (cue.on === on && cue.beat.beat === 'chdir' && t >= cue.until) where = cue.beat.to
  }
  return where
}

export interface WebFrame {
  readonly url: string
  /** 0 before the browser has been anywhere, 1 once the page has painted. */
  readonly loaded: number
  readonly page: PageId | null
}

export function web(timeline: Timeline, on: SurfaceId, t: number): WebFrame {
  let frame: WebFrame = { url: '', loaded: 0, page: null }

  for (const cue of timeline.cues) {
    if (cue.on !== on || cue.beat.beat !== 'visit' || t < cue.at) continue
    const loaded = Math.min(1, (t - cue.at) / (cue.until - cue.at))
    frame = { url: cue.beat.url, loaded, page: loaded < 1 ? null : cue.beat.page }
  }

  return frame
}

/**
 * The whole session as plain text.
 *
 * In the order it happened, rather than window by window — the two terminals
 * overlap, and a transcript that hid that would be describing a different
 * session from the one on screen. Each window says its name when the
 * narrative moves to it; those names are the surface ids, which are Latin in
 * both languages for the same reason `worktree` is.
 */
export function transcript(timeline: Timeline): string {
  const lines: string[] = []
  let last: SurfaceId | null = null

  for (const cue of timeline.cues) {
    const said: string[] = []

    if (cue.beat.beat === 'type') said.push(`$ ${cue.beat.command}`)
    if (cue.beat.beat === 'step') said.push(`  ✓ ${cue.beat.label}`)
    if (cue.beat.beat === 'print') said.push(...cue.beat.rows.map(text))
    if (cue.beat.beat === 'visit') said.push(`https://${cue.beat.url}`)
    if (!said.length) continue

    if (cue.on !== last) {
      if (last) lines.push('')
      lines.push(`── ${cue.on} ──`)
      last = cue.on
    }

    lines.push(...said)
  }

  return lines.join('\n')
}

function prompt(command: string): Row {
  return [
    ['muted', '$ '],
    ['plain', command],
  ]
}

function done(label: string): Row {
  return [
    ['good', '  ✓ '],
    ['plain', label],
  ]
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

export interface Player {
  readonly t: Ref<number>
  readonly chapter: ComputedRef<number>
  readonly playing: Ref<boolean>
  /** True when the reader asked for less motion: the clock never runs. */
  readonly still: Ref<boolean>
  readonly duration: number
  readonly chapters: readonly Chapter[]
  start(el: HTMLElement): void
  replay(): void
  seek(chapter: number): void
}

export function usePlayer(timeline: Timeline): Player {
  // The last frame, on the server and on the first client render alike, so
  // hydration matches and a reader with no JavaScript sees the finished
  // screens rather than an empty box. `start()` decides whether to rewind.
  const t = ref(timeline.duration)
  const playing = ref(false)
  const still = ref(true)

  let raf = 0
  let origin = 0
  let paused = false

  const chapter = computed(() => {
    let i = 0
    timeline.chapters.forEach((c, n) => {
      if (t.value >= c.at) i = n
    })
    return i
  })

  function tick() {
    t.value = Math.min(timeline.duration, performance.now() - origin)
    if (t.value >= timeline.duration) {
      playing.value = false
      return
    }
    raf = requestAnimationFrame(tick)
  }

  function play() {
    if (still.value || playing.value) return
    origin = performance.now() - t.value
    playing.value = true
    raf = requestAnimationFrame(tick)
  }

  function pause() {
    cancelAnimationFrame(raf)
    playing.value = false
  }

  function onVisibility() {
    if (document.hidden && playing.value) {
      paused = true
      pause()
    } else if (!document.hidden && paused) {
      paused = false
      play()
    }
  }

  function start(el: HTMLElement) {
    still.value = window.matchMedia('(prefers-reduced-motion: reduce)').matches
    if (still.value) return

    t.value = 0

    const seen = new IntersectionObserver(
      ([entry]) => {
        if (!entry.isIntersecting) return
        seen.disconnect()
        play()
      },
      { threshold: 0.35 },
    )
    seen.observe(el)

    document.addEventListener('visibilitychange', onVisibility)

    onUnmounted(() => {
      seen.disconnect()
      document.removeEventListener('visibilitychange', onVisibility)
      cancelAnimationFrame(raf)
    })
  }

  function replay() {
    still.value = false
    t.value = 0
    pause()
    play()
  }

  function seek(index: number) {
    still.value = false
    t.value = timeline.chapters[index].at
    pause()
    play()
  }

  return { t, chapter, playing, still, duration: timeline.duration, chapters: timeline.chapters, start, replay, seek }
}

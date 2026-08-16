<script setup lang="ts">
/**
 * The session, playing.
 *
 * The stage is a fixed-size scene scaled to whatever room the page has, so
 * the arithmetic the panels depend on is the same at every width: a column is
 * a column, and only the whole picture gets smaller. There are two scenes —
 * one wide, one narrow — because a scaled-down copy of the wide one is
 * unreadable on a phone, and the narrow one stacks the two terminals instead
 * of standing them side by side.
 *
 * Where each window sits is CSS, keyed on `data-act`. The player decides
 * which act it is; the browser does the moving.
 */
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useData } from 'vitepress'
import ShellWindow from './ShellWindow.vue'
import BrowserWindow from './BrowserWindow.vue'
import { copyFor } from '../copy'
import { compile, cwd, live, scrollback, settled, transcript, usePlayer, web } from '../demo/player'

const props = withDefaults(
  defineProps<{
    /** Run the stage edge to edge rather than inside the text column. */
    full?: boolean
    /** How far the scene may be scaled up when there is room for it. */
    maxScale?: number
  }>(),
  { full: false, maxScale: 1 },
)

const { lang } = useData()
const copy = computed(() => copyFor(lang.value))

const timeline = compile()
const player = usePlayer(timeline)
const { t } = player

const act = computed(() => timeline.chapters[player.chapter.value].id)

/** How far through one chapter the clock is, 0 to 1. The four rails are the progress bar. */
function through(i: number): number {
  const from = timeline.chapters[i].at
  const to = timeline.chapters[i + 1]?.at ?? timeline.duration
  return Math.max(0, Math.min(1, (t.value - from) / (to - from)))
}

/** Rows only change when a cue lands, so they are keyed on the count and not on the clock. */
function useShell(on: 'shell-a' | 'shell-b') {
  const landed = computed(() => settled(timeline, on, t.value))
  return {
    title: computed(() => cwd(timeline, on, t.value)),
    rows: computed(() => scrollback(timeline, on, landed.value)),
    live: computed(() => live(timeline, on, t.value)),
  }
}

const shellA = useShell('shell-a')
const shellB = useShell('shell-b')
const webA = computed(() => web(timeline, 'web-a', t.value))
const webB = computed(() => web(timeline, 'web-b', t.value))

const root = ref<HTMLElement | null>(null)
const stage = ref<HTMLElement | null>(null)
const fieldEl = ref<HTMLElement | null>(null)

// Both are what the server renders. `onMounted` corrects them, which is an
// update rather than a mismatch.
const narrow = ref(false)
const scale = ref(1)

// Two rows of 340 with 32 between them, and the narrow one two of 270.
const scene = computed(() => (narrow.value ? { w: 380, h: 572 } : { w: 1120, h: 712 }))

/*
 * Only the height is set. The field runs the width of the text column like
 * every other section, and the scene is scaled to fill it — so the grey is a
 * field the windows sit on rather than a margin around a smaller picture.
 *
 * The padding is read rather than assumed: on a phone the field loses its
 * side padding to give the scene the whole screen, and a constant here would
 * shrink the terminal to nothing at exactly the width where it is already
 * smallest.
 */
const field = ref({ height: '0px' })

function fit() {
  if (!stage.value || !fieldEl.value) return

  const pad = getComputedStyle(fieldEl.value)
  const padX = parseFloat(pad.paddingLeft) + parseFloat(pad.paddingRight)
  const padY = parseFloat(pad.paddingTop) + parseFloat(pad.paddingBottom)

  const room = stage.value.clientWidth - padX
  if (room <= 0) return

  narrow.value = room < 860
  const { w, h } = scene.value
  scale.value = Math.min(narrow.value ? 1.6 : props.maxScale, room / w)
  field.value = { height: `${h * scale.value + padY}px` }
}

let watching: ResizeObserver | undefined

onMounted(() => {
  fit()
  watching = new ResizeObserver(fit)
  watching.observe(stage.value!)
  player.start(root.value!)
})

onUnmounted(() => watching?.disconnect())
</script>

<template>
  <section ref="root" class="demo" :class="{ 'demo-full': full }">
    <div class="demo-head">
      <h2>{{ copy.heading }}</h2>
      <p class="lead">{{ copy.lead }}</p>
    </div>

    <!--
      Two boxes on purpose. `.stage` is measured and never painted, so the
      room it reports does not depend on the scale that room decides;
      `.stage-field` is the grey panel the windows sit on.
    -->
    <div ref="stage" class="stage" aria-hidden="true">
      <div ref="fieldEl" class="stage-field" :style="field">
        <div
          class="scene"
          :data-act="act"
          :data-narrow="narrow"
          :style="{ width: `${scene.w}px`, height: `${scene.h}px`, transform: `scale(${scale})` }"
        >
          <div class="surface" data-surface="web-a">
            <BrowserWindow v-bind="webA" branch="feature/user-auth" />
          </div>
          <div class="surface" data-surface="web-b">
            <BrowserWindow v-bind="webB" branch="fix/checkout-total" />
          </div>
          <div class="surface" data-surface="shell-a">
            <ShellWindow :title="shellA.title.value" :rows="shellA.rows.value" :live="shellA.live.value" />
          </div>
          <div class="surface" data-surface="shell-b">
            <ShellWindow :title="shellB.title.value" :rows="shellB.rows.value" :live="shellB.live.value" />
          </div>
        </div>
      </div>
    </div>

    <p class="caption">{{ copy.acts[act] }}</p>

    <div class="controls">
      <div class="chapters" role="group" :aria-label="copy.chapters">
        <button
          v-for="(chapter, i) in player.chapters"
          :key="chapter.id"
          type="button"
          class="chapter"
          :class="{ on: i === player.chapter.value }"
          :aria-current="i === player.chapter.value"
          @click="player.seek(i)"
        >
          <span class="rail"><span class="fill" :style="{ transform: `scaleX(${through(i)})` }" /></span>
          {{ chapter.id }}
        </button>
      </div>

      <button type="button" class="replay" @click="player.replay()">{{ copy.replay }}</button>
    </div>

    <details class="transcript">
      <summary>{{ copy.transcript }}</summary>
      <pre>{{ transcript(timeline) }}</pre>
    </details>
  </section>
</template>

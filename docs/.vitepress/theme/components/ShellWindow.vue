<script setup lang="ts">
/**
 * A terminal window.
 *
 * The title is the working directory, because that is what a terminal's is,
 * and it is how the two windows in the parallel act say which worktree they
 * are standing in. Everything below the title bar is text: the same rows the
 * CLI printed, in the same styles it asked for.
 */
import { computed } from 'vue'
import { Line } from '../demo/Line'
import type { Row } from '../demo/script'

const props = defineProps<{
  title: string
  rows: readonly Row[]
  live: Row | null
  /** Only the last few rows fit; the rest have scrolled off, as they would. */
  visibleRows?: number
}>()

const shown = computed(() => {
  const limit = props.visibleRows ?? 16
  const all = props.live ? [...props.rows, props.live] : [...props.rows]
  return all.slice(Math.max(0, all.length - limit))
})
</script>

<template>
  <div class="window shell">
    <div class="bar">
      <span class="lights" aria-hidden="true"><i class="close" /><i class="min" /><i class="zoom" /></span>
      <span class="title">{{ title }}</span>
    </div>
    <div class="body">
      <Line v-for="(row, i) in shown" :key="i" :row="row" />
    </div>
  </div>
</template>

<style scoped>
.shell {
  background: var(--kb-shell-bg);
  color: var(--kb-shell-fg);
}

.shell .bar {
  border-bottom: 1px solid var(--kb-shell-rule);
  color: var(--kb-shell-muted);
}

.body {
  padding: 10px 12px 12px;
  font-family: var(--kb-font-mono);
  font-size: var(--kb-shell-size);
  line-height: 1.5;
  white-space: pre;
  font-variant-ligatures: none;
  overflow: hidden;
  display: flex;
  flex-direction: column;
  justify-content: flex-end;
  height: calc(100% - var(--kb-bar-h));
  /* What has scrolled off the top fades rather than being sliced through the
     middle of a letter. */
  mask-image: linear-gradient(to bottom, transparent 0, #000 28px);
}
</style>

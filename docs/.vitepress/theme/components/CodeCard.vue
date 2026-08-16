<script setup lang="ts">
/**
 * A small dark card holding a file or a response.
 *
 * It draws through the same spans and the same palette the terminal uses, so
 * a `kobune.toml` on this page is coloured the way the tool colours one.
 */
import { Line } from '../demo/Line'
import type { Row } from '../demo/script'

defineProps<{
  label?: string
  rows: readonly Row[]
  /** A light card instead, for the thing Kobune is being compared against. */
  quiet?: boolean
}>()
</script>

<template>
  <div class="kb-code" :class="{ quiet }">
    <p v-if="label" class="kb-code-label">{{ label }}</p>
    <div class="kb-code-body"><Line v-for="(row, i) in rows" :key="i" :row="row" /></div>
  </div>
</template>

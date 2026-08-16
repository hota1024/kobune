<script setup lang="ts">
/**
 * A browser window.
 *
 * The address is the reason this window is drawn at all: it is the thing
 * Kobune produces, and it is what tells you which worktree you are looking
 * at. The page under it is a shape rather than a screenshot — enough of an
 * application to see that the two branches differ, and nothing more.
 */
import type { PageId } from '../demo/script'

defineProps<{
  url: string
  page: PageId | null
  /** 0 while the address bar fills, 1 once the page has painted. */
  loaded: number
  /** The worktree this window belongs to, shown the way a dev build shows it. */
  branch: string
}>()
</script>

<template>
  <div class="window browser">
    <div class="bar">
      <span class="lights" aria-hidden="true"><i class="close" /><i class="min" /><i class="zoom" /></span>
      <span class="nav" aria-hidden="true">
        <svg viewBox="0 0 16 16" width="13" height="13"><path d="M10 3 5 8l5 5" /></svg>
        <svg viewBox="0 0 16 16" width="13" height="13"><path d="m6 3 5 5-5 5" /></svg>
      </span>
      <span class="address">
        <svg class="lock" viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
          <rect x="3.5" y="7" width="9" height="6.5" rx="1.2" />
          <path d="M5.75 7V5.25a2.25 2.25 0 0 1 4.5 0V7" />
        </svg>
        <span class="url"><span class="scheme">https://</span>{{ url }}</span>
      </span>
    </div>
    <!-- Gone once the page has painted, the way a browser's is. -->
    <div
      class="progress"
      :style="{ transform: `scaleX(${loaded})`, opacity: loaded < 1 ? 1 : 0 }"
      aria-hidden="true"
    />

    <div class="page" :class="{ blank: !page }">
      <div class="head">
        <span class="wordmark">myapp</span>
        <span class="branch">{{ branch }}</span>
      </div>

      <div v-if="page === 'shop-auth'" class="pane">
        <p class="pane-title">Sign in</p>
        <span class="field" /><span class="field" />
        <span class="submit">Continue</span>
      </div>

      <div v-else-if="page === 'shop-cart'" class="pane">
        <p class="pane-title">Cart</p>
        <p class="row"><span>Deck chair</span><span class="n">¥1,800</span></p>
        <p class="row"><span>Rope, 10 m</span><span class="n">¥1,800</span></p>
        <p class="row total"><span>Total</span><span class="n">¥3,600</span></p>
      </div>

      <div v-else class="pane">
        <span class="block wide" /><span class="block" />
      </div>
    </div>
  </div>
</template>

<style scoped>
.browser {
  background: var(--kb-page-bg);
  color: var(--kb-page-fg);
}

.browser .bar {
  border-bottom: 1px solid var(--kb-page-rule);
  gap: 10px;
}

.nav {
  display: flex;
  gap: 2px;
  color: var(--kb-page-muted);
}

.nav svg {
  fill: none;
  stroke: currentColor;
  stroke-width: 1.6;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.address {
  flex: 1;
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 5px;
  height: 22px;
  padding: 0 9px;
  border-radius: 11px;
  background: var(--kb-page-well);
  font-family: var(--kb-font-mono);
  font-size: 11px;
  color: var(--kb-page-fg);
  overflow: hidden;
}

/* The ellipsis has to live on the item, not on the flex container it is in:
   a flex container clips its children rather than trimming their text. */
.url {
  min-width: 0;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}

.lock {
  flex: none;
  fill: none;
  stroke: var(--kb-page-muted);
  stroke-width: 1.3;
}

.scheme {
  color: var(--kb-page-muted);
}

.progress {
  height: 2px;
  background: var(--kb-accent);
  transform-origin: left;
  transition:
    transform 90ms linear,
    opacity var(--kb-dur-short) var(--kb-ease-out);
}

.page {
  padding: 14px 16px;
  height: calc(100% - var(--kb-bar-h) - 2px);
  transition: opacity 220ms var(--kb-ease-out);
}

.page.blank {
  opacity: 0;
}

.head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  padding-bottom: 9px;
  border-bottom: 1px solid var(--kb-page-rule);
}

.wordmark {
  font-family: var(--kb-font-mono);
  font-size: 13px;
  font-weight: 600;
  letter-spacing: -0.01em;
}

.branch {
  font-family: var(--kb-font-mono);
  font-size: 10px;
  color: var(--kb-page-muted);
}

.pane {
  padding-top: 14px;
}

.pane-title {
  margin: 0 0 10px;
  font-size: 13px;
  font-weight: 600;
}

.field {
  display: block;
  height: 22px;
  margin-bottom: 7px;
  border: 1px solid var(--kb-page-rule);
  border-radius: 4px;
  background: var(--kb-page-well);
}

.submit {
  display: inline-block;
  margin-top: 4px;
  padding: 4px 12px;
  border-radius: 4px;
  background: var(--kb-page-fg);
  color: var(--kb-page-bg);
  font-size: 11px;
}

.row {
  display: flex;
  justify-content: space-between;
  margin: 0 0 6px;
  font-size: 12px;
  color: var(--kb-page-muted);
}

.row .n {
  font-variant-numeric: tabular-nums;
}

.row.total {
  margin-top: 10px;
  padding-top: 8px;
  border-top: 1px solid var(--kb-page-rule);
  color: var(--kb-page-fg);
  font-weight: 600;
}

.block {
  display: block;
  height: 12px;
  width: 45%;
  margin-bottom: 8px;
  border-radius: 3px;
  background: var(--kb-page-well);
}

.block.wide {
  width: 72%;
}
</style>

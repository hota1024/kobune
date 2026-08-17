<script setup lang="ts">
/**
 * The bar above the nav, on every page.
 *
 * Nothing has been released: `nightly` is a rolling build of `main` and
 * carries no version. A reader who lands on a guide page from a search has
 * no other way to learn that, and the documentation's own rule is to say
 * what does not work before somebody finds out.
 *
 * **It is fixed, and it reports its own height.** The default theme reserves
 * `--vp-layout-top-height` above the page — `VPContent` takes it as a top
 * margin at every width, and `VPNav` adds it to `top` once it goes
 * `position: fixed` at 960px. A bar left in the normal flow is counted twice
 * below 960 and leaves an empty band above the nav above it, so it has to
 * come out of the flow; and a bar whose height is a constant somewhere else
 * clips its own text the first time a translation grows. Measuring is what
 * keeps the two the same fact.
 *
 * **It renders from `layout-bottom`, not `layout-top`.** The default layout
 * emits `layout-top` before `VPSkipLink`, which would put this link ahead of
 * *skip to content* in the tab order on every page — the one thing that link
 * exists to be in front of. `position: fixed` puts it back at the top of the
 * page without putting it back at the top of the document.
 */
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useData } from 'vitepress'
import { copyFor } from '../copy'

const { lang, page } = useData()
const banner = computed(() => copyFor(lang.value).banner)

/**
 * The locale and version this page lives under — `''`, `/ja`, `/v0.1`,
 * `/v0.1/ja` — taken from the path the way `config.ts` builds every other
 * link. A hardcoded `/guide/installation` would eject a reader out of a
 * snapshot and into the current documentation.
 */
const base = computed(() => {
  const [, prefix] = /^((?:v\d+(?:\.\d+)*\/)?(?:ja\/)?)/.exec(page.value.relativePath) ?? []
  return prefix ? `/${prefix.slice(0, -1)}` : ''
})

/** A snapshot is a release. Saying "nothing is released" on one is a lie. */
const frozen = computed(() => /^v\d/.test(page.value.relativePath))

const el = ref<HTMLElement | null>(null)
let watching: ResizeObserver | undefined

onMounted(() => {
  if (!el.value) return
  const report = () => {
    document.documentElement.style.setProperty('--vp-layout-top-height', `${el.value!.offsetHeight}px`)
  }
  report()
  watching = new ResizeObserver(report)
  watching.observe(el.value)
})

onUnmounted(() => {
  watching?.disconnect()
  document.documentElement.style.removeProperty('--vp-layout-top-height')
})
</script>

<template>
  <div v-if="!frozen" ref="el" class="kb-banner">
    <p>
      {{ banner.text }}
      <a :href="`${base}/guide/installation#${banner.anchor}`">{{ banner.linkText }}</a>
    </p>
  </div>
</template>

<style scoped>
.kb-banner {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  z-index: var(--vp-z-index-layout-top);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 9px 24px;
  border-bottom: 1px solid var(--vp-c-divider);
  background: var(--vp-c-bg-alt);
}

.kb-banner p {
  margin: 0;
  font-family: var(--kb-font-mono);
  font-size: 12.5px;
  line-height: 1.4;
  font-variant-ligatures: none;
  color: var(--vp-c-text-2);
  text-align: center;
}

.kb-banner a {
  color: var(--vp-c-text-1);
  border-bottom: 1px solid var(--vp-c-divider);
  transition: border-color var(--kb-dur-micro) var(--kb-ease-out);
}

.kb-banner a:hover {
  border-bottom-color: var(--kb-accent);
}

/* Narrow enough that the sentence wraps; the height follows on its own. */
@media (max-width: 639px) {
  .kb-banner {
    padding: 8px 16px;
  }

  .kb-banner p {
    font-size: 11.5px;
    line-height: 1.45;
  }
}
</style>

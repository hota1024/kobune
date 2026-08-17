<script setup lang="ts">
/**
 * The bar above the nav, on every page.
 *
 * Nothing has been released: `nightly` is a rolling build of `main` and
 * carries no version. A reader who lands on a guide page from a search has
 * no other way to learn that, and the documentation's own rule is to say
 * what does not work before somebody finds out.
 *
 * It is fixed, and `--vp-layout-top-height` is set to what it measures.
 *
 * That pairing is the theme's contract, not a choice: `VPNav` is
 * `position: fixed; top: var(--vp-layout-top-height)`, so the room above the
 * nav is reserved whether or not anything is still in it. Left in the normal
 * flow the bar scrolls away and leaves that reserved strip empty, which is a
 * band of nothing sitting above the nav for the rest of the page.
 */
import { computed } from 'vue'
import { useData } from 'vitepress'
import { copyFor } from '../copy'

const { lang } = useData()
const banner = computed(() => copyFor(lang.value).banner)
</script>

<template>
  <div class="kb-banner">
    <p>
      {{ banner.text }}
      <a :href="banner.link">{{ banner.linkText }}</a>
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
  height: var(--vp-layout-top-height);
  padding: 0 24px;
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

/* The sentence stops being one line here; `--vp-layout-top-height` makes
   room for the second, and the gutter narrows to put off the wrap. */
@media (max-width: 639px) {
  .kb-banner {
    padding: 0 16px;
  }

  .kb-banner p {
    font-size: 11.5px;
    line-height: 1.45;
  }
}
</style>

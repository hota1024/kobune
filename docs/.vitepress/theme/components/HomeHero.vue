<script setup lang="ts">
/**
 * The hero, replacing the default theme's.
 *
 * The copy is still `hero:` in the page's frontmatter, so both languages sit
 * side by side in Markdown the way every other page's do. What changes is the
 * markup: the name is a name rather than a gradient-filled span, and the mark
 * is small and above the title instead of large and beside it.
 */
import { computed } from 'vue'
import { useData } from 'vitepress'

const { frontmatter } = useData()
const hero = computed(() => frontmatter.value.hero ?? {})
</script>

<template>
  <div class="kb-hero">
    <img class="kb-mark" src="/logo/kobune-mark.svg" alt="" width="40" height="40" fetchpriority="high" />

    <h1 class="kb-title">
      <span class="kb-name">{{ hero.name }}</span>
      <span class="kb-text">{{ hero.text }}</span>
    </h1>

    <p class="kb-lede">{{ hero.tagline }}</p>
  </div>
</template>

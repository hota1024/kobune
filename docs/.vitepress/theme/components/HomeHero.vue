<script setup lang="ts">
/**
 * The hero, replacing the default theme's.
 *
 * The copy is still `hero:` in the page's frontmatter, so both languages sit
 * side by side in Markdown the way every other page's do. What changes is the
 * markup: nothing precedes the claim, and the mark is set inline after its
 * last word, the size of the type it follows.
 *
 * The name is not repeated here. The nav carries it two lines above, and a
 * mark, a wordmark and a headline stacked in a column are three things
 * announcing the same one.
 */
import { computed } from 'vue'
import { useData } from 'vitepress'

const { frontmatter } = useData()
const hero = computed(() => frontmatter.value.hero ?? {})
</script>

<template>
  <div class="kb-hero">
    <h1 class="kb-title">
      <!-- No space before the mark: the gap is its margin, and a text node
           here would put a second one in front of it. -->
      <span class="kb-text">{{ hero.text
        }}<img class="kb-mark" src="/logo/kobune-mark.svg" alt="" width="40" height="40" fetchpriority="high"
      /></span>
    </h1>

    <p class="kb-lede">{{ hero.tagline }}</p>
  </div>
</template>

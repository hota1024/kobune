<script setup lang="ts">
/**
 * What you get, as a sheet rather than as cards.
 *
 * Six things, each a label and a sentence, ruled off from one another. The
 * label is the name the documentation uses for the thing, set in the same
 * face the tool prints in, so the left column reads as an index into the
 * guide rather than as decoration.
 *
 * Each pair is wrapped in a `div` — which a `dl` allows — so the two shapes
 * are one CSS change rather than two templates: rows of label-then-sentence,
 * or a grid of cells with the label above its sentence.
 */
import { computed } from 'vue'
import { useData } from 'vitepress'

/** Six rows down the page, or two columns of three where space is tighter. */
defineProps<{ grid?: boolean }>()

const { frontmatter } = useData()
const specs = computed(() => frontmatter.value.specs)
</script>

<template>
  <section v-if="specs" class="kb-specs" :class="{ 'kb-specs-grid': grid }">
    <h2>{{ specs.title }}</h2>

    <dl>
      <div v-for="item in specs.items" :key="item.label">
        <dt>{{ item.label }}</dt>
        <dd>{{ item.body }}</dd>
      </div>
    </dl>
  </section>
</template>

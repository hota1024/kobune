<script setup lang="ts">
/**
 * What you get, as a sheet rather than as cards.
 *
 * Six things, each a label and a sentence, ruled off from one another. The
 * label is the name the documentation uses for the thing, set in the same
 * face the tool prints in, so the left column reads as an index into the
 * guide rather than as decoration.
 */
import { computed } from 'vue'
import { useData } from 'vitepress'

const { frontmatter } = useData()
const specs = computed(() => frontmatter.value.specs)
</script>

<template>
  <section v-if="specs" class="kb-specs">
    <h2>{{ specs.title }}</h2>

    <dl>
      <template v-for="item in specs.items" :key="item.label">
        <dt>{{ item.label }}</dt>
        <dd>{{ item.body }}</dd>
      </template>
    </dl>
  </section>
</template>

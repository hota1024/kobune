<script setup lang="ts">
/**
 * The shortest description of the product: three things you type, and the
 * URL that comes out of them.
 *
 * The third row is not a command. `kobune open` does not exist — `DESIGN.md`
 * sketched it and the CLI never grew it — so the step is the address itself,
 * which is what a reader actually does next.
 */
import { computed } from 'vue'
import { useData } from 'vitepress'

const { frontmatter } = useData()
const steps = computed(() => frontmatter.value.steps)
</script>

<template>
  <section v-if="steps" class="kb-steps">
    <h2>{{ steps.title }}</h2>

    <ol>
      <li v-for="(step, i) in steps.items" :key="step.command">
        <span class="n">{{ i + 1 }}</span>
        <code :class="{ url: step.url }">{{ step.command }}</code>
        <p>{{ step.body }}</p>
      </li>
    </ol>
  </section>
</template>

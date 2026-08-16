<script setup lang="ts">
/**
 * What actually runs the containers.
 *
 * Firecracker is in the list and marked as not implemented, because it is
 * the thing a reader is most likely to be looking for and least likely to
 * find out about otherwise. `docs/README.md` asks for exactly that.
 */
import { computed } from 'vue'
import { useData } from 'vitepress'

const { frontmatter } = useData()
const runtimes = computed(() => frontmatter.value.runtimes)
</script>

<template>
  <section v-if="runtimes" class="kb-runtimes">
    <h2>{{ runtimes.title }}</h2>
    <p class="kb-runtimes-lead">{{ runtimes.lead }}</p>

    <ul>
      <li v-for="runtime in runtimes.items" :key="runtime.key" :class="{ absent: !runtime.ready }">
        <code>{{ runtime.key }}</code>
        <span class="state">{{ runtime.state }}</span>
        <span class="name">{{ runtime.name }}</span>
      </li>
    </ul>
  </section>
</template>

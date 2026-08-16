<script setup lang="ts">
/**
 * What actually runs the containers.
 *
 * Firecracker is in the list and marked unsupported, because it is the thing
 * a reader is most likely to be looking for and least likely to find out
 * about otherwise. Why it is unsupported is `guide/runtimes.md`'s business,
 * not the home page's — a row that stops at "no" is the honest length.
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
        <!-- A backend that is not there has nothing to say about itself. -->
        <span v-if="runtime.name" class="name">{{ runtime.name }}</span>
      </li>
    </ul>
  </section>
</template>

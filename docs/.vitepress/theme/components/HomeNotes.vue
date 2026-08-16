<script setup lang="ts">
/**
 * What it is not.
 *
 * The guide has said this since it was written, and it belongs on the page a
 * reader lands on first: someone whose problem is one branch and one stack is
 * better served by being told so here than by finding out three pages in.
 */
import { computed } from 'vue'
import { useData } from 'vitepress'

const { frontmatter } = useData()
const notes = computed(() => frontmatter.value.notes)
</script>

<template>
  <section v-if="notes" class="kb-notes">
    <h2>{{ notes.title }}</h2>

    <ul>
      <li v-for="note in notes.items" :key="note.lead">
        <b>{{ note.lead }}</b> {{ note.body }}
      </li>
    </ul>

    <a class="kb-more" :href="notes.link">{{ notes.linkText }}</a>
  </section>
</template>

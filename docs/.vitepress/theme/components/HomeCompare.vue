<script setup lang="ts">
/**
 * The same stack, twice.
 *
 * A reader arriving from compose wants to know what changes, and the answer
 * is: almost nothing, plus one key compose has no word for. `scope` is what
 * says a database is shared by every worktree in the project rather than
 * started once per branch.
 */
import { computed } from 'vue'
import { useData } from 'vitepress'
import CodeCard from './CodeCard.vue'
import { COMPOSE, KOBUNE_PAIR } from '../demo/toml'

const { frontmatter } = useData()
const compare = computed(() => frontmatter.value.compare)
</script>

<template>
  <section v-if="compare" class="kb-compare">
    <h2>{{ compare.title }}</h2>
    <p class="kb-compare-lead">{{ compare.body }}</p>

    <div class="kb-compare-pair">
      <CodeCard label="docker-compose.yml" :rows="COMPOSE" quiet />
      <CodeCard label="kobune.toml" :rows="KOBUNE_PAIR" />
    </div>

    <p class="kb-compare-note">{{ compare.note }}</p>
  </section>
</template>

<script setup lang="ts">
/**
 * The file, in the hero, beside the claim.
 *
 * This is the whole input: one committed file, and every worktree the
 * repository grows reads it. Putting it in the fold answers "what do I have
 * to write" before the reader has to scroll to find out.
 */
import { computed } from 'vue'
import { useData } from 'vitepress'
import CodeCard from './CodeCard.vue'
import { KOBUNE_TOML } from '../demo/toml'

const { frontmatter } = useData()
const config = computed(() => frontmatter.value.config)
</script>

<template>
  <div v-if="config" class="kb-config">
    <CodeCard label="kobune.toml" :rows="KOBUNE_TOML" />
    <p class="kb-config-note">{{ config.note }}</p>
  </div>
</template>

<script setup lang="ts">
/**
 * The line that installs it, under the buttons.
 *
 * The README opens with this command and the site serves the script it pipes
 * into a shell, so the home page is the one place a reader should not have to
 * go looking for it.
 */
import { computed, ref } from 'vue'
import { useData } from 'vitepress'

const { frontmatter } = useData()
const install = computed(() => frontmatter.value.hero?.install)

const copied = ref(false)
let clearing: ReturnType<typeof setTimeout> | undefined

async function copy() {
  if (!install.value?.command) return
  await navigator.clipboard.writeText(install.value.command)
  copied.value = true
  clearTimeout(clearing)
  clearing = setTimeout(() => (copied.value = false), 2000)
}
</script>

<template>
  <div v-if="install" class="kb-install">
    <code>{{ install.command }}</code>
    <button type="button" @click="copy">{{ copied ? install.copied : install.copy }}</button>
  </div>
</template>

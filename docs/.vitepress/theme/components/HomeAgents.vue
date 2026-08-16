<script setup lang="ts">
/**
 * The agent half of the pitch, given its own paragraph rather than threaded
 * through every other claim.
 *
 * It sits low on the page deliberately. #50 moved the agent line out of the
 * lead because it read like every other tool's, and what is actually
 * different is the worktree; this is where the qualifier gets its say.
 *
 * The JSON beside it is an excerpt of a real response — the field names are
 * `WorkspaceInfo` and `ServiceInfo` in `crates/kobune-api/src/response.rs`,
 * and the object has more keys than are shown.
 */
import { computed } from 'vue'
import { useData } from 'vitepress'
import { Line } from '../demo/Line'
import type { Row } from '../demo/script'

const { frontmatter } = useData()
const agents = computed(() => frontmatter.value.agents)

function key(name: string): Row {
  return [['link', `"${name}"`], ['muted', ': ']]
}

const RESPONSE: Row[] = [
  [['muted', '$ '], ['plain', 'kobune status --json']],
  [['muted', '{']],
  [['plain', '  '], ...key('project'), ['good', '"myapp"'], ['muted', ',']],
  [['plain', '  '], ...key('workspace'), ['good', '"feature-user-auth"'], ['muted', ',']],
  [['plain', '  '], ...key('branch'), ['good', '"feature/user-auth"'], ['muted', ',']],
  [['plain', '  '], ...key('services'), ['muted', '[{']],
  [['plain', '    '], ...key('name'), ['good', '"web"'], ['muted', ',']],
  [['plain', '    '], ...key('state'), ['good', '"ready"'], ['muted', ',']],
  [['plain', '    '], ...key('url'), ['good', '"https://web.feature-user-auth.myapp.localhost"']],
  [['muted', '  }]']],
  [['muted', '}']],
]
</script>

<template>
  <section v-if="agents" class="kb-agents">
    <div class="kb-agents-body">
      <h2>{{ agents.title }}</h2>
      <p>{{ agents.body }}</p>
      <a class="kb-more" :href="agents.link">{{ agents.linkText }}</a>
    </div>

    <div class="kb-agents-proof" aria-hidden="true">
      <Line v-for="(row, i) in RESPONSE" :key="i" :row="row" />
    </div>
  </section>
</template>

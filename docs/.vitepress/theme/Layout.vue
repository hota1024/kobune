<script setup lang="ts">
/**
 * Layout 1a — the demo is the hero.
 *
 * The claim is two lines and the install line, and then the session runs
 * edge to edge before the first scroll. Everything under it is caption:
 * three commands, the sheet, the agent paragraph, and what the tool is not.
 *
 * Slots are lazy and `VPHome` is the only thing that renders these, so none
 * of it is instantiated on a documentation page.
 */
import DefaultTheme from 'vitepress/theme'
import HomeHero from './components/HomeHero.vue'
import HomeInstall from './components/HomeInstall.vue'
import HomeSteps from './components/HomeSteps.vue'
import HomeSpecs from './components/HomeSpecs.vue'
import HomeAgents from './components/HomeAgents.vue'
import HomeNotes from './components/HomeNotes.vue'
import DemoStage from './components/DemoStage.vue'

const { Layout } = DefaultTheme
</script>

<template>
  <Layout>
    <template #home-hero-info><HomeHero /></template>
    <template #home-hero-actions-after><HomeInstall /></template>
    <template #home-hero-after><DemoStage full :max-scale="1.28" /></template>
    <template #home-features-after>
      <HomeSteps />
      <HomeSpecs />
      <HomeAgents />
      <HomeNotes />
    </template>
  </Layout>
</template>

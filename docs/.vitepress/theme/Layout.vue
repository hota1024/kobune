<script setup lang="ts">
/**
 * Layout 1b — answer in the fold.
 *
 * The claim and the file that produces it stand side by side above the fold,
 * then three panels say what happens, so the model is proved before a
 * scroll. The session drops to evidence in the middle of the page: by the
 * time a reader reaches it they know what they are looking at, and it is
 * confirming rather than explaining.
 *
 * Denser than 1a on purpose — this one is for somebody evaluating the tool
 * against something they already use.
 */
import DefaultTheme from 'vitepress/theme'
import HomeHero from './components/HomeHero.vue'
import HomeInstall from './components/HomeInstall.vue'
import HomeConfig from './components/HomeConfig.vue'
import HomeSteps from './components/HomeSteps.vue'
import HomeCompare from './components/HomeCompare.vue'
import HomeSpecs from './components/HomeSpecs.vue'
import HomeAgents from './components/HomeAgents.vue'
import HomeRuntimes from './components/HomeRuntimes.vue'
import HomeNotes from './components/HomeNotes.vue'
import DemoStage from './components/DemoStage.vue'

const { Layout } = DefaultTheme
</script>

<template>
  <Layout>
    <template #home-hero-info><HomeHero /></template>
    <template #home-hero-actions-after><HomeInstall /></template>
    <!-- Filling this slot is what puts VPHero into two columns. -->
    <template #home-hero-image><HomeConfig /></template>
    <template #home-hero-after><HomeSteps /></template>
    <template #home-features-after>
      <HomeCompare />
      <DemoStage />
      <HomeSpecs grid />
      <div class="kb-pair">
        <HomeAgents />
        <HomeRuntimes />
      </div>
      <HomeNotes />
    </template>
  </Layout>
</template>

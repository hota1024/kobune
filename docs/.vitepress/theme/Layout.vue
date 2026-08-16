<script setup lang="ts">
/**
 * The default theme, with the home page's five slots filled.
 *
 * Slots are lazy, and `VPHome` is the only thing that renders these, so none
 * of it is instantiated on a documentation page. `home-hero-after` is a
 * direct child of `.VPHome`, which has no width and no padding of its own —
 * which is why the demo can run the width of the page without a hack.
 */
import DefaultTheme from 'vitepress/theme'
import HomeHero from './components/HomeHero.vue'
import HomeInstall from './components/HomeInstall.vue'
import HomeSpecs from './components/HomeSpecs.vue'
import HomeNotes from './components/HomeNotes.vue'
import DemoStage from './components/DemoStage.vue'

const { Layout } = DefaultTheme
</script>

<template>
  <Layout>
    <template #home-hero-info><HomeHero /></template>
    <template #home-hero-actions-after><HomeInstall /></template>
    <template #home-hero-after><DemoStage /></template>
    <template #home-features-after>
      <HomeSpecs />
      <HomeNotes />
    </template>
  </Layout>
</template>

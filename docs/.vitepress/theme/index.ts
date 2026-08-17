/**
 * The theme.
 *
 * The default theme, extended rather than replaced. The nav, the sidebar and
 * the search are VitePress's own, and a documentation page is styled entirely
 * by them — what is added is the home page: its hero, the session it plays,
 * and the sections under it.
 *
 * The one thing every page carries is the bar above the nav, which says that
 * this is a nightly build. It brings `SiteBanner`'s styles and the
 * `--vp-layout-top-height` the default theme reserves for it.
 */
import type { Theme } from 'vitepress'
import DefaultTheme from 'vitepress/theme'
import Layout from './Layout.vue'

import './styles/tokens.css'
import './styles/home.css'
import './styles/stage.css'

export default {
  extends: DefaultTheme,
  Layout,
} satisfies Theme

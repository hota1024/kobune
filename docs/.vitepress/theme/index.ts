/**
 * The theme.
 *
 * The default theme, extended rather than replaced: the nav, the sidebar, the
 * search and every documentation page are VitePress's, untouched. What is
 * added is the home page — its hero, the session it plays, and the two
 * sections under it.
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

import { defineConfig, type DefaultTheme } from 'vitepress'
import versions from './versions.json'

/**
 * The version the docs at the root describe.
 *
 * Bump this when you start writing for the next release. `cargo xtask docs
 * snapshot` copies the current tree to `/vX.Y/` and adds it to
 * `versions.json`, so what lives at the root is always the unreleased one.
 */
const CURRENT = '0.1'

/**
 * Where each locale lives.
 *
 * English at the root and Japanese under `/ja/`, matching the README and
 * the code. A snapshot nests underneath: `/v0.1/` and `/v0.1/ja/`.
 */
const LOCALES = ['', '/ja'] as const

type Lang = 'en' | 'ja'

/** Everything that differs between locales, in one place. */
const TEXT = {
  en: {
    label: 'English',
    description: 'A development environment manager for AI agents',
    tagline: 'Create a git worktree, and its preview environment is up',
    guide: 'Guide',
    reference: 'Reference',
    tutorials: 'Tutorials',
    started: 'Getting started',
    usingIt: 'Using it',
    goingFurther: 'Going further',
    editLink: 'Suggest a change to this page',
    lastUpdated: 'Last updated',
    outline: 'On this page',
    darkModeSwitch: 'Switch to dark theme',
    returnToTop: 'Return to top',
    notFound: "This page doesn't exist",
    versionArchived: (v: string) =>
      `You are reading the documentation for ${v}, which is not the latest release.`,
  },
  ja: {
    label: '日本語',
    description: 'AI エージェント向けの開発環境管理ツール',
    tagline: 'git worktree を作れば、プレビュー環境が立ち上がっている',
    guide: 'ガイド',
    reference: 'リファレンス',
    tutorials: 'チュートリアル',
    started: 'はじめる',
    usingIt: '使い方',
    goingFurther: 'さらに詳しく',
    editLink: 'このページを修正する',
    lastUpdated: '最終更新',
    outline: 'このページの内容',
    darkModeSwitch: 'ダークテーマに切り替える',
    returnToTop: 'トップへ戻る',
    notFound: 'ページが見つかりません',
    versionArchived: (v: string) =>
      `これは ${v} のドキュメントです。最新のリリースではありません。`,
  },
} satisfies Record<Lang, Record<string, unknown>>

/**
 * The page tree, written once.
 *
 * Both locales and every snapshotted version are generated from this, so a
 * page added here appears everywhere it should. Titles are per-locale;
 * paths are not.
 */
const PAGES = {
  started: [
    ['guide/', { en: 'What is Minato?', ja: 'Minato とは' }],
    ['guide/installation', { en: 'Installation', ja: 'インストール' }],
    ['guide/getting-started', { en: 'Your first environment', ja: '最初の環境を作る' }],
    ['guide/configuration', { en: 'Configuration', ja: '設定' }],
  ],
  usingIt: [
    ['guide/workflow', { en: 'Everyday workflow', ja: '基本操作' }],
    ['guide/environment-variables', { en: 'Environment variables', ja: '環境変数' }],
    ['guide/agents', { en: 'Working with AI agents', ja: 'AI エージェントと使う' }],
    ['guide/gui', { en: 'The desktop app', ja: 'デスクトップアプリ' }],
  ],
  goingFurther: [
    ['guide/runtimes', { en: 'Runtimes', ja: 'ランタイム' }],
    ['guide/tunnel', {
      en: 'Sharing over Cloudflare Tunnel',
      ja: 'Cloudflare Tunnel で共有する',
    }],
    ['guide/how-it-works', { en: 'How it works', ja: '仕組み' }],
    ['guide/troubleshooting', { en: 'Troubleshooting', ja: '困ったときは' }],
  ],
  reference: [
    ['reference/cli', { en: 'CLI commands', ja: 'CLI コマンド' }],
    ['reference/minato-toml', { en: 'minato.toml', ja: 'minato.toml' }],
    ['reference/exit-codes', { en: 'Exit codes', ja: '終了コード' }],
  ],
  tutorials: [
    ['tutorials/first-preview', { en: 'A preview per branch', ja: 'ブランチごとのプレビュー' }],
    ['tutorials/multi-service', { en: 'A web app and a database', ja: 'Web アプリとデータベース' }],
    ['tutorials/sharing', { en: 'Sharing a preview', ja: 'プレビューを共有する' }],
  ],
} as const

/** `/ja/guide/installation` and friends, for a given locale and version. */
function root(version: string, locale: string): string {
  return `${version}${locale}`
}

function items(
  group: readonly (readonly [string, Record<Lang, string>])[],
  base: string,
  lang: Lang,
): DefaultTheme.SidebarItem[] {
  return group.map(([path, title]) => ({
    text: title[lang],
    link: `${base}/${path}`,
  }))
}

function sidebar(base: string, lang: Lang): DefaultTheme.SidebarItem[] {
  const t = TEXT[lang]

  return [
    { text: t.started, items: items(PAGES.started, base, lang) },
    { text: t.usingIt, items: items(PAGES.usingIt, base, lang) },
    { text: t.goingFurther, items: items(PAGES.goingFurther, base, lang) },
    { text: t.reference, items: items(PAGES.reference, base, lang) },
    { text: t.tutorials, items: items(PAGES.tutorials, base, lang) },
  ]
}

/**
 * The version switcher.
 *
 * Absent until something has been snapshotted — a dropdown offering one
 * choice is furniture, not navigation.
 */
function versionNav(base: string, lang: Lang): DefaultTheme.NavItem[] {
  if (versions.length === 0) {
    return []
  }

  const suffix = lang === 'ja' ? '/ja' : ''

  return [
    {
      text: `v${CURRENT}`,
      items: [
        { text: `v${CURRENT} (latest)`, link: `${suffix}/` },
        ...versions.map((v) => ({ text: `v${v}`, link: `/v${v}${suffix}/` })),
      ],
    },
  ]
}

function nav(base: string, lang: Lang): DefaultTheme.NavItem[] {
  const t = TEXT[lang]

  return [
    { text: t.guide, link: `${base}/guide/`, activeMatch: `${base}/guide/` },
    {
      text: t.reference,
      link: `${base}/reference/cli`,
      activeMatch: `${base}/reference/`,
    },
    {
      text: t.tutorials,
      link: `${base}/tutorials/first-preview`,
      activeMatch: `${base}/tutorials/`,
    },
    ...versionNav(base, lang),
  ]
}

function themeConfig(base: string, lang: Lang): DefaultTheme.Config {
  const t = TEXT[lang]

  return {
    nav: nav(base, lang),
    sidebar: { [`${base}/`]: sidebar(base, lang) },
    outline: { level: [2, 3], label: t.outline },
    editLink: {
      pattern: 'https://github.com/hota1024/minato/edit/main/docs/:path',
      text: t.editLink,
    },
    lastUpdatedText: t.lastUpdated,
    darkModeSwitchLabel: t.darkModeSwitch,
    returnToTopLabel: t.returnToTop,
    docFooter:
      lang === 'ja' ? { prev: '前のページ', next: '次のページ' } : undefined,
  }
}

/**
 * One locale entry per (version, language) pair.
 *
 * VitePress keys locales by directory, so a snapshot at `/v0.1/ja/` is just
 * another locale as far as it is concerned. That is what keeps a snapshot
 * from needing any configuration of its own.
 */
function locales(): DefaultTheme.Config extends never ? never : Record<string, any> {
  const entries: Record<string, any> = {}
  const allVersions = ['', ...versions.map((v) => `/v${v}`)]

  for (const version of allVersions) {
    for (const locale of LOCALES) {
      const lang: Lang = locale === '/ja' ? 'ja' : 'en'
      const base = root(version, locale)
      const t = TEXT[lang]

      // The root English locale is the site default and must be keyed
      // `root`; everything else is keyed by its directory.
      const key = base === '' ? 'root' : base.slice(1).replace(/\//g, '-')

      entries[key] = {
        label: t.label,
        lang: lang === 'ja' ? 'ja-JP' : 'en-US',
        dir: base === '' ? undefined : base.slice(1),
        link: `${base}/`,
        description: t.description,
        themeConfig: themeConfig(base, lang),
      }
    }
  }

  return entries
}

export default defineConfig({
  title: 'Minato',
  description: TEXT.en.description,
  cleanUrls: true,
  lastUpdated: true,

  // DESIGN.md predates this site and is an internal record — the decisions
  // and the ones that were reversed — not a page for readers. It stays in
  // the repository and is linked to on GitHub.
  srcExclude: ['DESIGN.md'],

  head: [
    ['meta', { name: 'theme-color', content: '#3451b2' }],
    ['meta', { property: 'og:type', content: 'website' }],
    ['meta', { property: 'og:title', content: 'Minato' }],
    ['meta', { property: 'og:description', content: TEXT.en.description }],
  ],

  themeConfig: {
    logo: undefined,
    socialLinks: [{ icon: 'github', link: 'https://github.com/hota1024/minato' }],
    search: { provider: 'local' },
    footer: {
      message: 'Released under the MIT License.',
      copyright: 'Copyright © 2026 hota1024',
    },
  },

  locales: locales(),
})

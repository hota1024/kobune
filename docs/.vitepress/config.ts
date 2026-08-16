import { defineConfig, type DefaultTheme } from 'vitepress'
import llmstxt from 'vitepress-plugin-llms'
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
 * The blue the logo is drawn in.
 *
 * Only the browser's own chrome is tinted with it — the site still uses
 * VitePress's default accent, which is a darker indigo and passes contrast
 * on white where this does not.
 */
const BRAND = '#00B4DB'

/** Where the site is served. The sitemap and the social card need it spelled out. */
const HOSTNAME = 'https://minato.1024.works'

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
    description: 'A development environment manager built around git worktrees. Agent-friendly by design.',
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
    description: 'git worktree を軸にした開発環境管理ツール。エージェントから扱いやすい設計です。',
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
    ['guide/', { en: 'What is Kobune?', ja: 'Kobune とは' }],
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
      en: 'Sharing over a tunnel',
      ja: 'トンネルで共有する',
    }],
    ['guide/how-it-works', { en: 'How it works', ja: '仕組み' }],
    ['guide/troubleshooting', { en: 'Troubleshooting', ja: '困ったときは' }],
  ],
  reference: [
    ['reference/cli', { en: 'CLI commands', ja: 'CLI コマンド' }],
    ['reference/kobune-toml', { en: 'kobune.toml', ja: 'kobune.toml' }],
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
      pattern: 'https://github.com/hota1024/kobune/edit/main/docs/:path',
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
  title: 'Kobune',
  description: TEXT.en.description,
  cleanUrls: true,
  lastUpdated: true,

  // Absolute, because a sitemap has to be. Preview deployments get their
  // own hostname and therefore a sitemap pointing at production; nothing
  // crawls a preview, so that is the harmless direction to be wrong in.
  // The social card below is absolute for the same reason and shares the
  // hostname, so a move is one edit.
  sitemap: { hostname: HOSTNAME },

  // Neither is a page for readers. DESIGN.md is the internal record —
  // including the decisions that were reversed — AGENT-RUN.md is a
  // transcript kept for reference, and README.md is how to work on this
  // site. All three stay in the repository and are read on GitHub.
  //
  // This is also what keeps them out of the files below. The plugin takes
  // its pages from Vite's module graph rather than from the directory, so a
  // file excluded here is never compiled and never reaches it.
  srcExclude: ['DESIGN.md', 'AGENT-RUN.md', 'README.md'],

  // The agent-facing half of the site: `/llms.txt`, `/llms-full.txt`, and a
  // `.md` for every page in the sidebar — `/guide/installation` is the page
  // and `/guide/installation.md` is the same page as Markdown, which
  // `cleanUrls` leaves the extension free for.
  //
  // Two of the plugin's habits are worth knowing before reading its output.
  // The home page is not among the pages: `excludeIndexPage` defaults on,
  // and `index.md` is a `layout: home` with nothing in its body to serve.
  // And a directory index moves up a level, so `/guide/` is written to
  // `/guide.md` — the URL `llms.txt` gives for it, but one the page's own
  // `./installation` links no longer resolve against. Links are left as
  // VitePress wrote them throughout, so following one leads to the HTML
  // page rather than to its `.md`.
  vite: {
    plugins: [
      llmstxt({
        // Absolute, for the reason the sitemap is: `llms.txt` is read away
        // from the site, where a relative link has nothing to resolve
        // against. Unlike the sitemap it costs something on a preview
        // deployment, where these links point at the live site instead of
        // at the branch being reviewed.
        domain: HOSTNAME,

        // English only, by decision — these files are not translated — and
        // no superseded documentation, which is the last thing to hand an
        // agent. Both lists are the ones the rest of the file is built
        // from, so a new locale or a new snapshot cannot appear without
        // this following it.
        ignoreFiles: [
          ...LOCALES.filter((locale) => locale !== '').map((locale) => `${locale.slice(1)}/**`),
          ...versions.map((version) => `v${version}/**`),
        ],

        // The plugin orders and titles the index from `themeConfig.sidebar`,
        // and there is nothing at that key here: every sidebar lives under
        // `locales`, because each locale and version has its own. Handing it
        // the root English one is what makes `llms.txt` read in the order of
        // the sidebar rather than the order of the directory tree.
        sidebar: sidebar('', 'en'),
      }),
    ],
  },

  head: [
    // The icon rather than the mark: a favicon is drawn at 16px against
    // whatever colour the browser's chrome happens to be, and the mark
    // alone would be competing with it.
    //
    // SVG only, with no .ico beside it. Every browser that has shipped in
    // the last five years takes one, and the fallback would be a raster to
    // regenerate by hand every time the logo changes.
    ['link', { rel: 'icon', type: 'image/svg+xml', href: '/logo/kobune-icon.svg' }],
    ['link', { rel: 'apple-touch-icon', href: '/logo/kobune-icon.svg' }],
    ['meta', { name: 'theme-color', content: BRAND }],
    ['meta', { property: 'og:type', content: 'website' }],
    ['meta', { property: 'og:title', content: 'Kobune' }],
    ['meta', { property: 'og:description', content: TEXT.en.description }],

    // Drawn from the logo by `scripts/og.mjs` on every build, so it is
    // never a copy to remember to redo. SVG is not an option here: no
    // crawler renders one, and a card that does not appear is worse than
    // none at all.
    ['meta', { property: 'og:image', content: `${HOSTNAME}/og.png` }],
    ['meta', { property: 'og:image:width', content: '1200' }],
    ['meta', { property: 'og:image:height', content: '630' }],
    ['meta', { property: 'og:image:alt', content: 'Kobune' }],
    ['meta', { name: 'twitter:card', content: 'summary_large_image' }],
  ],

  themeConfig: {
    // Copied out of `assets/logo/` by `pnpm sync`, so the repository holds
    // one copy of it. See `assets/README.md`.
    logo: { src: '/logo/kobune-mark.svg', alt: 'Kobune' },
    socialLinks: [{ icon: 'github', link: 'https://github.com/hota1024/kobune' }],
    search: { provider: 'local' },
    footer: {
      message: 'Released under the Apache License 2.0.',
      copyright: 'Copyright © 2026 hota1024',
    },
  },

  locales: locales(),
})

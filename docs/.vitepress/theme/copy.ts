/**
 * Everything this theme says in words.
 *
 * Shaped like `TEXT` in `.vitepress/config.ts`, and for the same reason:
 * everything that differs between locales in one place, so a missing
 * translation is a hole you can see rather than a file you have to remember
 * to open.
 *
 * Nothing the CLI printed is in here. Console output is not translated — see
 * `.claude/skills/prose/SKILL.md` — and `demo/script.ts` is the only place it
 * lives. The chapter names are Latin in both languages for the same reason
 * `worktree` is.
 */
import type { ActId } from './demo/script'

interface Copy {
  /**
   * The bar above the nav, on every page.
   *
   * `SiteBanner` derives the path — it belongs to the locale and the version,
   * not to the words. The anchor is here because it is made out of the
   * heading it points at, which is one of the words.
   */
  readonly banner: {
    readonly text: string
    readonly linkText: string
    readonly anchor: string
  }
  readonly heading: string
  readonly lead: string
  readonly replay: string
  readonly chapters: string
  readonly transcript: string
  readonly acts: Record<ActId, string>
}

export const COPY = {
  en: {
    banner: {
      text: 'Nightly build — not a release, and not stable yet.',
      linkText: 'What that means',
      anchor: 'what-you-are-installing',
    },
    heading: 'A session, from nothing to two previews',
    lead: 'Nothing here is a recording. It is the output the commands print, drawn as text.',
    replay: 'Play again',
    chapters: 'Jump to a part of the session',
    transcript: 'The session as text',
    acts: {
      init: 'One file describes the project, and the main worktree comes up.',
      branch: 'A worktree, and its environment came up with it — at a URL of its own.',
      parallel: 'A second branch. The first one was not stopped to make room for it.',
      preview: 'Two branches, two URLs, both still running. Nobody picked a port.',
    },
  },
  ja: {
    banner: {
      text: 'nightly ビルドです。リリース版ではなく、まだ安定していません。',
      linkText: '詳しく',
      anchor: '何をインストールすることになるか',
    },
    heading: '何もない状態から 2 つのプレビューまで',
    lead: '録画ではありません。コマンドが実際に出力する内容を、そのまま文字として描いています。',
    replay: 'もう一度再生する',
    chapters: '見たい場面に移動する',
    transcript: 'セッションの内容をテキストで読む',
    acts: {
      init: '1 つのファイルにプロジェクトを書くと、main の環境が立ち上がります。',
      branch: 'worktree を作ると、環境も一緒に立ち上がります。URL も worktree ごとに割り当てられます。',
      parallel: '2 本目のブランチを作ります。1 本目を止める必要はありません。',
      preview: 'ブランチが 2 つ、URL も 2 つ。どちらも動いたままで、ポート番号は誰も選んでいません。',
    },
  },
} satisfies Record<'en' | 'ja', Copy>

/**
 * `useData().lang` is `en-US` or `ja-JP` — and stays that way inside a
 * snapshot, where the locale key gains a `v0.1-` prefix but the language
 * does not.
 */
export function copyFor(lang: string): Copy {
  return lang.startsWith('ja') ? COPY.ja : COPY.en
}

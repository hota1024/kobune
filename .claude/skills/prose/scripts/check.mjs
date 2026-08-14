// Mechanical checks for the repository's prose. The rules and the counts behind
// them are in ../references/; this file only enforces the ones that need no
// judgement. Nothing here can tell whether a page is any good.
//
//   node .claude/skills/prose/scripts/check.mjs             # everything
//   node .claude/skills/prose/scripts/check.mjs docs/guide  # a subtree
//   node .claude/skills/prose/scripts/check.mjs --json      # for an agent
//
// Exits 1 when it finds anything, 2 when it cannot run.

import { readFileSync, readdirSync, existsSync, statSync } from 'node:fs'
import { dirname, join, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const HERE = dirname(fileURLToPath(import.meta.url))
const ROOT = resolve(HERE, '..', '..', '..', '..')

/** Prose written for a reader. `references/` in the skill says why each is here. */
const FILES = ['README.md', 'SECURITY.md', 'assets/README.md', 'docs/index.md',
  'docs/README.md', 'docs/DESIGN.md', 'docs/AGENT-RUN.md', 'skills/minato/SKILL.md']
const DIRS = ['docs/guide', 'docs/reference', 'docs/tutorials', 'docs/ja']

/** Never ours: generated from history, frozen at release, or vendored. */
const SKIP = /(^|\/)(node_modules|\.git|target|\.agents)(\/|$)|(^|\/)docs\/v\d|(^|\/)CHANGELOG\.md$/

const MAX_WIDTH = 80

// ── the corpus ───────────────────────────────────────────────────────────────

function walk(dir, out = []) {
  for (const name of readdirSync(dir).sort()) {
    const path = join(dir, name)
    if (SKIP.test(relative(ROOT, path))) continue
    const stat = statSync(path)
    if (stat.isDirectory()) walk(path, out)
    else if (name.endsWith('.md')) out.push(path)
  }
  return out
}

/** A page's opposite number, so naming one language still checks both. */
function counterpart(rel) {
  if (rel.startsWith('docs/ja/')) return `docs/${rel.slice('docs/ja/'.length)}`
  if (rel.startsWith('docs/')) return `docs/ja/${rel.slice('docs/'.length)}`
  return null
}

function inventory(args) {
  const paths = []
  if (args.length === 0) {
    for (const f of FILES) if (existsSync(join(ROOT, f))) paths.push(join(ROOT, f))
    for (const d of DIRS) if (existsSync(join(ROOT, d))) walk(join(ROOT, d), paths)
  } else {
    for (const arg of args) {
      const path = resolve(arg)
      if (!existsSync(path)) fail(`no such path: ${arg}`)
      if (statSync(path).isDirectory()) walk(path, paths)
      else paths.push(path)
    }
    for (const path of [...paths]) {
      const other = counterpart(relative(ROOT, path))
      if (other && existsSync(join(ROOT, other))) paths.push(join(ROOT, other))
    }
  }
  return [...new Set(paths)].filter((p) => !SKIP.test(relative(ROOT, p)))
}

// ── reading a file ───────────────────────────────────────────────────────────

/**
 * One entry per line: whether it is inside a fence, frontmatter or a table, and
 * the fence's info string on the line that opened it.
 */
function parse(text) {
  const lines = text.split('\n')
  const marks = lines.map(() => ({ code: false, front: false, table: false, info: null }))
  let fence = null
  let front = false
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]
    if (i === 0 && line.trim() === '---') { front = true; marks[i].front = true; continue }
    if (front) {
      marks[i].front = true
      if (line.trim() === '---') front = false
      continue
    }
    const opener = line.match(/^\s*(`{3,}|~{3,})\s*(\S*)/)
    if (fence === null && opener) {
      // The length matters: a ```` block may hold ``` blocks of its own.
      fence = { char: opener[1][0], length: opener[1].length }
      marks[i].code = true
      marks[i].info = opener[2]
      continue
    }
    if (fence !== null) {
      marks[i].code = true
      const closes = opener && opener[1][0] === fence.char &&
        opener[1].length >= fence.length && !opener[2]
      if (closes) fence = null
      continue
    }
    marks[i].table = line.trimStart().startsWith('|')
  }
  return { lines, marks }
}

/** Prose lines only: no frontmatter, no code, nothing blank. */
function* prose({ lines, marks }) {
  for (let i = 0; i < lines.length; i++) {
    if (marks[i].code || marks[i].front || !lines[i].trim()) continue
    yield [i + 1, lines[i], marks[i]]
  }
}

// ── width ────────────────────────────────────────────────────────────────────

function wide(c) {
  return (c >= 0x1100 && c <= 0x115f) || (c >= 0x2e80 && c <= 0x303e) ||
    (c >= 0x3041 && c <= 0x33ff) || (c >= 0x3400 && c <= 0x4dbf) ||
    (c >= 0x4e00 && c <= 0x9fff) || (c >= 0xa000 && c <= 0xa4cf) ||
    (c >= 0xac00 && c <= 0xd7a3) || (c >= 0xf900 && c <= 0xfaff) ||
    (c >= 0xfe30 && c <= 0xfe6f) || (c >= 0xff00 && c <= 0xff60) ||
    (c >= 0xffe0 && c <= 0xffe6) || (c >= 0x20000 && c <= 0x3fffd)
}

function width(s) {
  let w = 0
  for (const ch of s) w += wide(ch.codePointAt(0)) ? 2 : 1
  return w
}

// ── masking, so a rule sees prose and not markup ─────────────────────────────

const LINK_TARGET = /\]\([^)\s]*(?:\s+"[^"]*")?\)/g
const URL = /<?\bhttps?:\/\/[^\s>)`]+>?/g
const CODE_SPAN = /(`+)[^`]*?\1/g

/** Drops link targets and bare URLs. What is left is what a reader reads. */
function unlink(line) {
  return line.replace(LINK_TARGET, ']').replace(URL, 'URL')
}

/** Also drops code spans, so a flag is never mistaken for a word. */
function unmark(line) {
  return unlink(line).replace(CODE_SPAN, 'x')
}

// ── the rules ────────────────────────────────────────────────────────────────

const JA = '\\u3041-\\u3096\\u30a1-\\u30fa\\u30fc\\u4e00-\\u9fff\\u3005'
const JA_LATIN = new RegExp(`[${JA}][\`\\[]?[A-Za-z0-9]`)
const LATIN_JA = new RegExp(`[A-Za-z0-9][\`\\]]?[${JA}]`)
const JA_PAREN = new RegExp(`[${JA}]\\s?\\(|\\)\\s?[${JA}]`)
// Only the markers that cannot occur in a ですます sentence. `〜している。` in a
// list of conditions is a judgement call, and this file does not make those.
const PLAIN_FORM = /(である|であった|だった|ではない|であろう)。/

const CONTRACTIONS =
  /\b(\w+n't|it's|that's|there's|here's|what's|let's|you're|we're|they're|i've|you've|we've|they've|i'll|you'll|we'll|it'll|i'd|you'd|we'd)\b/i
const FIRST_PERSON = /\b(we|our|ours|us)\b/i
const AMERICAN = /\b(color|colors|colored|behavior|behaviors|favorite|analyze|catalog|\w*iz(?:e|es|ed|ing|ation|ations))\b/i
const NOT_AMERICAN = /^(size|sizes|resize|resizes|resized|resizing|seize|seizes|prize|prizes|capsize|maize)$/i

/** A line is over the limit only if it is still over it once URLs cannot help. */
function overWide(line) {
  return width(line) > MAX_WIDTH && width(line.replace(URL, 'URL')) > MAX_WIDTH
}

/**
 * `**term** — gloss` and `- [page](x) — gloss` separate a label from what it
 * means. That is layout, shared with the English page, and not the sentence
 * dash japanese.md is about. A table's `—` means "no default" and is not a dash
 * at all. Everything else counts.
 */
const LABEL_DASH = /^\s*(?:[-*]\s+)?(?:\*\*.+?\*\*|\[.+?\]|x)\s—\s/

function loneDash(read, isTable) {
  if (isTable) return false
  return /(?<!—)—(?!—)/.test(read.replace(LABEL_DASH, ''))
}

function checkFile(path, add) {
  const rel = relative(ROOT, path)
  const text = readFileSync(path, 'utf8')
  const doc = parse(text)
  const japanese = rel.startsWith('docs/ja/')

  if (!text.endsWith('\n')) add(rel, doc.lines.length, 'file/newline', 'no newline at end of file')
  else if (text.endsWith('\n\n')) add(rel, doc.lines.length, 'file/newline', 'blank line at end of file')

  for (const [n, line, mark] of prose(doc)) {
    if (/[ \t]+$/.test(line)) add(rel, n, 'file/trailing', 'trailing whitespace')
    if (!mark.table && overWide(line)) {
      add(rel, n, 'layout/width', `${width(line)} columns, over ${MAX_WIDTH}`)
    }

    const read = unmark(line)
    if (japanese) {
      const linked = unlink(line)
      if (JA_LATIN.test(linked)) add(rel, n, 'ja/space', 'no space before half-width text')
      if (LATIN_JA.test(linked)) add(rel, n, 'ja/space', 'no space after half-width text')
      if (loneDash(read, mark.table)) {
        add(rel, n, 'ja/dash', 'a lone — in a sentence; recast it, see japanese.md')
      }
      if (JA_PAREN.test(read)) add(rel, n, 'ja/paren', 'half-width ( ) in Japanese text; use （ ）')
      if (/[Ａ-Ｚａ-ｚ０-９　]/.test(line)) add(rel, n, 'ja/fullwidth', 'full-width alphanumeric or space')
      if (/[｡-ﾟ]/.test(line)) add(rel, n, 'ja/kana', 'half-width katakana')
      if (/[，．！？]/.test(read)) add(rel, n, 'ja/punctuation', 'use 、。 and no ！？')
      if (!mark.table && !line.startsWith('#') && PLAIN_FORM.test(read)) {
        add(rel, n, 'ja/style', 'plain form in a ですます page')
      }
    } else {
      const found = read.match(CONTRACTIONS)
      if (found) add(rel, n, 'en/contraction', `${found[0]}: write it out`)
      if (FIRST_PERSON.test(read)) add(rel, n, 'en/person', 'we/our/us: name the thing instead')
      const spelling = read.match(AMERICAN)
      if (spelling && !NOT_AMERICAN.test(spelling[0])) {
        add(rel, n, 'en/spelling', `${spelling[0]}: the docs are in British English`)
      }
      if (/\s--\s|–/.test(read)) add(rel, n, 'en/dash', 'use — with a space either side')
      if (/\S—|—\S/.test(read)) add(rel, n, 'en/dash', '— needs a space either side')
      if (!mark.table && /\S {2,}\S/.test(read)) add(rel, n, 'en/space', 'double space')
    }
  }
  return doc
}

// ── English against Japanese ─────────────────────────────────────────────────

function headings({ lines, marks }) {
  const out = []
  for (let i = 0; i < lines.length; i++) {
    if (marks[i].code || marks[i].front) continue
    const m = lines[i].match(/^(#{1,6})\s+(.*)$/)
    if (m) out.push({ level: m[1].length, text: m[2].trim(), line: i + 1 })
  }
  return out
}

function fences({ lines, marks }) {
  const out = []
  for (let i = 0; i < lines.length; i++) if (marks[i].info !== null) out.push(marks[i].info)
  return out
}

function containers({ lines, marks }) {
  const out = []
  for (let i = 0; i < lines.length; i++) {
    if (marks[i].code || marks[i].front) continue
    const m = lines[i].match(/^:::\s*(\w+)/)
    if (m) out.push(m[1])
  }
  return out
}

function tables({ lines, marks }) {
  let count = 0
  let inside = false
  for (let i = 0; i < lines.length; i++) {
    const table = marks[i].table && !marks[i].code
    if (table && !inside) count++
    inside = table
  }
  return count
}

function checkPair(rel, en, ja, add) {
  const jaRel = `docs/ja/${rel}`
  const a = headings(en)
  const b = headings(ja)
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    if (a[i] && b[i] && a[i].level === b[i].level) continue
    const at = b[i]?.line ?? b[b.length - 1]?.line ?? 1
    const mine = b[i] ? `${'#'.repeat(b[i].level)} ${b[i].text}` : '(nothing)'
    const theirs = a[i] ? `${'#'.repeat(a[i].level)} ${a[i].text}` : '(nothing)'
    add(jaRel, at, 'parity/headings',
      `heading ${i + 1} of ${a.length}: English has "${theirs}", Japanese has "${mine}"`)
    break
  }

  const fa = fences(en)
  const fb = fences(ja)
  if (fa.length !== fb.length) {
    add(jaRel, 1, 'parity/code', `${fa.length} code blocks in English, ${fb.length} in Japanese`)
  } else {
    for (let i = 0; i < fa.length; i++) {
      if (fa[i] === fb[i]) continue
      add(jaRel, 1, 'parity/code', `code block ${i + 1} is \`\`\`${fa[i]} in English, \`\`\`${fb[i]} in Japanese`)
      break
    }
  }

  const ca = containers(en).join(',')
  const cb = containers(ja).join(',')
  if (ca !== cb) add(jaRel, 1, 'parity/container', `::: blocks are [${ca}] in English, [${cb}] in Japanese`)

  const ta = tables(en)
  const tb = tables(ja)
  if (ta !== tb) add(jaRel, 1, 'parity/table', `${ta} tables in English, ${tb} in Japanese`)
}

// ── the sidebar against the pages ────────────────────────────────────────────

function sidebar(add, docs) {
  const config = join(ROOT, 'docs/.vitepress/config.ts')
  if (!existsSync(config)) return
  const text = readFileSync(config, 'utf8')
  const start = text.indexOf('const PAGES')
  if (start < 0) return
  const block = text.slice(start, text.indexOf('} as const', start))
  const entries = [...block.matchAll(
    /\['([^']+)',\s*\{\s*en:\s*'([^']*)',\s*ja:\s*'([^']*)'/g,
  )]
  for (const [, page, en, ja] of entries) {
    const file = page.endsWith('/') ? `${page}index.md` : `${page}.md`
    for (const [lang, title, rel] of [['en', en, `docs/${file}`], ['ja', ja, `docs/ja/${file}`]]) {
      if (!existsSync(join(ROOT, rel))) {
        add('docs/.vitepress/config.ts', 1, 'sidebar/missing', `PAGES names ${rel}, which does not exist`)
        continue
      }
      // Titles are only compared for pages this run was asked about.
      const doc = docs.get(rel)
      if (!doc) continue
      const h1 = headings(doc).find((h) => h.level === 1)
      if (!h1) {
        add(rel, 1, 'sidebar/h1', 'no H1')
        continue
      }
      const plain = (s) => s.replace(/`/g, '').trim()
      if (plain(h1.text) !== plain(title)) {
        add(rel, h1.line, 'sidebar/h1',
          `PAGES (${lang}) says "${title}", the H1 says "${h1.text}"`)
      }
    }
  }
}

// ── links ────────────────────────────────────────────────────────────────────

/**
 * Two renderers, two slug rules, and the difference is not cosmetic.
 *
 * `srcExclude` in `config.ts` keeps `DESIGN.md`, `AGENT-RUN.md` and
 * `docs/README.md` out of the site, so those are read on GitHub and take
 * GitHub's rule. Everything else under `docs/` is a page and takes
 * `@mdit-vue/shared`'s.
 *
 * They disagree on a leading digit — `## 3. Architecture` is `#3-architecture`
 * on GitHub and `#_3-architecture` on the site — and on `_`, which GitHub keeps
 * and mdit-vue turns into `-`.
 */
function servedByVitePress(rel) {
  return rel.startsWith('docs/') &&
    !['docs/README.md', 'docs/DESIGN.md', 'docs/AGENT-RUN.md'].includes(rel)
}

function stripMarkup(text) {
  return text
    .replace(/`([^`]*)`/g, '$1')
    .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/\*\*?([^*]*)\*\*?/g, '$1')
    .trim()
    .toLowerCase()
}

/**
 * Japanese is kept as it is rather than transliterated or percent-encoded, so
 * `## ファイルに書き出す` is reached at `#ファイルに書き出す`. Compared in NFC
 * because the built HTML and the Markdown do not always agree on the form —
 * `プロキシ` composed and decomposed look identical and are not equal.
 *
 * Checked against every heading of every built page: 387 of 387.
 */
function anchor(text, vitepress) {
  const plain = stripMarkup(text)
  const slug = vitepress
    ? plain.replace(/[\s\][!'"#$%&()*+,./:;<=>?@\\^_{|}~`…—-]+/g, '-')
      .replace(/^-+|-+$/g, '')
      .replace(/^(\d)/, '_$1')
    : plain.replace(/[^\p{L}\p{N}\p{M}\p{Pc}\- ]/gu, '').replace(/ /g, '-')
  return slug.normalize('NFC')
}

function checkLinks(rel, doc, add, docs) {
  const { lines, marks } = doc
  for (let i = 0; i < lines.length; i++) {
    if (marks[i].code || marks[i].front) continue
    for (const [, target] of lines[i].matchAll(/\]\(([^)\s]+)\)/g)) {
      if (/^(https?:|mailto:)/.test(target)) continue
      const [path, fragment] = target.split('#')

      let landed = rel
      if (path) {
        const from = dirname(join(ROOT, rel))
        const base = path.startsWith('/') ? join(ROOT, 'docs', path) : resolve(from, path)
        const found = [base, `${base}.md`, join(base, 'index.md')].find((c) => existsSync(c))
        if (!found) {
          add(rel, i + 1, 'link/missing', `${target} resolves to nothing`)
          continue
        }
        landed = relative(ROOT, found)
        if (rel.startsWith('docs/ja/') && !landed.startsWith('docs/ja/')) {
          add(rel, i + 1, 'link/locale', `${target} leaves docs/ja/`)
        }
      }

      // Only for a page this run read, so a narrow run stays narrow.
      const page = docs.get(landed)
      if (!fragment || !page) continue
      const ids = headings(page).map((h) => anchor(h.text, servedByVitePress(landed)))
      if (!ids.includes(fragment.normalize('NFC'))) {
        add(rel, i + 1, 'link/anchor', `#${fragment} is not a heading in ${landed}`)
      }
    }
  }
}

// ── running ──────────────────────────────────────────────────────────────────

function fail(message) {
  process.stderr.write(`check.mjs: ${message}\n`)
  process.exit(2)
}

const args = process.argv.slice(2)
const json = args.includes('--json')
const paths = inventory(args.filter((a) => !a.startsWith('--')))
if (paths.length === 0) fail('nothing to check')

const findings = []
const add = (file, line, rule, message) => findings.push({ file, line, rule, message })

const docs = new Map()
for (const path of paths) docs.set(relative(ROOT, path), checkFile(path, add))
for (const [rel, doc] of docs) checkLinks(rel, doc, add, docs)

for (const [rel, doc] of docs) {
  if (rel.startsWith('docs/ja/') || !rel.startsWith('docs/')) continue
  const ja = docs.get(`docs/ja/${rel.slice('docs/'.length)}`)
  if (ja) checkPair(rel.slice('docs/'.length), doc, ja, add)
}

if ([...docs.keys()].some((rel) => rel.startsWith('docs/'))) sidebar(add, docs)

findings.sort((a, b) => a.file.localeCompare(b.file) || a.line - b.line || a.rule.localeCompare(b.rule))

if (json) {
  process.stdout.write(`${JSON.stringify({ files: docs.size, findings }, null, 2)}\n`)
} else if (findings.length === 0) {
  process.stdout.write(`${docs.size} files, nothing to report\n`)
} else {
  for (const f of findings) {
    process.stdout.write(`${f.file}:${f.line}  ${f.rule.padEnd(18)}  ${f.message}\n`)
  }
  const rules = new Set(findings.map((f) => f.rule))
  const many = (n, one) => `${n} ${one}${n === 1 ? '' : 's'}`
  process.stdout.write(
    `\n${many(findings.length, 'finding')} across ${many(rules.size, 'rule')}, ${many(docs.size, 'file')}\n`,
  )
}

process.exit(findings.length ? 1 : 0)

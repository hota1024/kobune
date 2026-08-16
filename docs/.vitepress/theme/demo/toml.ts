/**
 * The configuration shown on the home page.
 *
 * These are real files. `KOBUNE_TOML` is the shape `kobune init` writes, minus
 * its comments — see `apps/cli/src/init.rs` and `docs/reference/kobune-toml.md`
 * — and `COMPOSE` beside it is the same two services written for compose, which
 * is what `kobune init --from-compose` reads.
 *
 * They are drawn with the terminal's own styles: a table header is a subject,
 * a string is a string, a comment is muted.
 */
import type { Row } from './script'

function table(name: string): Row {
  return [['subject', `[${name}]`]]
}

function pair(key: string, value: string, quoted = true): Row {
  return [
    ['plain', key],
    ['muted', ' = '],
    [quoted ? 'good' : 'link', quoted ? `"${value}"` : value],
  ]
}

const BLANK: Row = []

/** What `kobune init` leaves in the repository root. */
export const KOBUNE_TOML: readonly Row[] = [
  table('project'),
  pair('name', 'myapp'),
  BLANK,
  table('runtime'),
  pair('default', 'docker'),
  BLANK,
  table('services.web'),
  pair('image', 'node:22'),
  pair('port', '3000', false),
  pair('command', 'npm run dev'),
]

/** The same two services, as compose writes them. */
export const COMPOSE: readonly Row[] = [
  [['subject', 'services:']],
  [['plain', '  web:']],
  [['plain', '    image: '], ['good', 'node:22']],
  [['plain', '    command: '], ['good', 'npm run dev']],
  [['plain', '    ports: '], ['muted', '['], ['good', '"3000:3000"'], ['muted', ']']],
  [['plain', '  db:']],
  [['plain', '    image: '], ['good', 'postgres:16']],
]

/** And as Kobune writes them, with the one key compose has no word for. */
export const KOBUNE_PAIR: readonly Row[] = [
  table('services.web'),
  pair('image', 'node:22'),
  pair('command', 'npm run dev'),
  pair('port', '3000', false),
  BLANK,
  table('services.db'),
  pair('image', 'postgres:16'),
  pair('scope', 'project'),
]

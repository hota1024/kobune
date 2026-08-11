// Draws the social card, `public/og.png`.
//
// Composed here rather than kept as an image, so that replacing the logo
// replaces the card: `assets/logo/minato-mark.svg` is embedded as a nested
// <svg>, exactly as exported.
//
// Run by `pnpm sync`, before dev and before build. The result is
// git-ignored, like everything else `sync` puts in `public/`.

import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { Resvg } from '@resvg/resvg-js'
import { decompress } from 'wawoff2'

const here = dirname(fileURLToPath(import.meta.url))
const docs = resolve(here, '..')
const root = resolve(docs, '..')

/** What every crawler crops to. */
const WIDTH = 1200
const HEIGHT = 630

const BACKGROUND = '#020202'
const BRAND = '#00B4DB'
const TEXT = '#ffffff'
const MUTED = '#8b949e'

/**
 * Inter, borrowed from VitePress.
 *
 * The card is then set in the same face as the pages, and there is no font
 * committed here to keep up to date. It is resvg that needs the file:
 * `font-family` alone would have it fall back to whatever the machine has
 * installed, and a runner's card would not match a laptop's.
 *
 * A VitePress upgrade that moved this path fails the build, which is the
 * right way round — an unreadable font renders as *nothing at all*, and a
 * card with the title silently missing is not something anyone notices
 * before it is in a tweet.
 */
const FONT = resolve(
  docs,
  'node_modules/vitepress/dist/client/theme-default/fonts/inter-roman-latin.woff2',
)

/**
 * resvg reads TrueType and OpenType, and the web ships woff2.
 *
 * Decompressing is the whole of the conversion — woff2 is a compressed
 * container around the same tables.
 */
async function font() {
  return Buffer.from(await decompress(readFileSync(FONT)))
}

/**
 * The mark, `size` wide, top-left corner at (`x`, `y`).
 *
 * A nested <svg> leaves the export's own viewBox to do the scaling, so
 * nothing here has to know how the drawing sits inside it.
 */
function mark(x, y, size) {
  const svg = readFileSync(resolve(root, 'assets/logo/minato-mark.svg'), 'utf8')

  return svg
    .replace(/^<svg /, `<svg x="${x}" y="${y}" `)
    .replace(/width="\d+" height="\d+"/, `width="${size}" height="${size}"`)
}

/**
 * Text, thickened.
 *
 * Inter is shipped as a variable font and resvg draws the default instance,
 * so `font-weight` does nothing — a stroke in the fill colour is how the
 * heading gets its weight.
 */
function heading(x, y, size, text) {
  return `<text x="${x}" y="${y}" font-size="${size}" fill="${TEXT}"
      stroke="${TEXT}" stroke-width="${size / 32}" letter-spacing="-2"
    >${text}</text>`
}

const card = `<svg width="${WIDTH}" height="${HEIGHT}" viewBox="0 0 ${WIDTH} ${HEIGHT}"
     xmlns="http://www.w3.org/2000/svg">
  <defs>
    <radialGradient id="glow" cx="0.2" cy="0.5" r="0.7">
      <stop offset="0" stop-color="${BRAND}" stop-opacity="0.2"/>
      <stop offset="1" stop-color="${BRAND}" stop-opacity="0"/>
    </radialGradient>
  </defs>

  <rect width="${WIDTH}" height="${HEIGHT}" fill="${BACKGROUND}"/>
  <rect width="${WIDTH}" height="${HEIGHT}" fill="url(#glow)"/>

  <!-- Laid out from the origin and moved as one, so the margins are
       balanced by adjusting a single pair of numbers. -->
  <g transform="translate(130, -14)">
    ${mark(53, 174, 300)}

    <g font-family="Inter">
      ${heading(400, 272, 96, 'Minato')}
      <text x="400" y="336" font-size="34" fill="${MUTED}">A development environment</text>
      <text x="400" y="382" font-size="34" fill="${MUTED}">manager for git worktrees</text>
      <text x="400" y="462" font-size="26" fill="${BRAND}">minato.1024.works</text>
    </g>
  </g>

  <rect y="${HEIGHT - 8}" width="${WIDTH}" height="8" fill="${BRAND}"/>
</svg>`

const png = new Resvg(card, {
  background: BACKGROUND,
  fitTo: { mode: 'width', value: WIDTH },
  font: {
    loadSystemFonts: false,
    fontBuffers: [await font()],
    defaultFontFamily: 'Inter',
  },
})
  .render()
  .asPng()

mkdirSync(resolve(docs, 'public'), { recursive: true })
writeFileSync(resolve(docs, 'public/og.png'), png)

console.log(`og.png  ${WIDTH}×${HEIGHT}  ${(png.length / 1024).toFixed(0)} KiB`)

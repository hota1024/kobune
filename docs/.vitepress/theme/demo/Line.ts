/**
 * One printed row.
 *
 * The panels are drawn out of box characters, and a monospace font that is
 * missing one falls back per glyph to a proportional face whose advance is
 * not one column — every character to the right of it then sits half a space
 * off, which is the one way this demo can look broken rather than merely
 * plain. So the cell is pinned rather than the font: everything outside ASCII
 * is given exactly `1ch` and centred in it, and whichever font ends up
 * supplying the glyph, the column it lands in is the right one.
 *
 * A render function rather than a template, because the rows are inside
 * `white-space: pre` and a template's indentation would be printed.
 */
import { h, type FunctionalComponent, type VNode } from 'vue'
import type { Row } from './script'

interface Chunk {
  readonly text: string
  readonly pin: boolean
}

/** ASCII stays as text; everything else becomes one chunk per character. */
function chunks(text: string): Chunk[] {
  const out: Chunk[] = []
  let ascii = ''

  for (const ch of text) {
    if (ch.codePointAt(0)! < 0x80) {
      ascii += ch
      continue
    }
    if (ascii) {
      out.push({ text: ascii, pin: false })
      ascii = ''
    }
    out.push({ text: ch, pin: true })
  }

  if (ascii) out.push({ text: ascii, pin: false })
  return out
}

export const Line: FunctionalComponent<{ row: Row }> = (props) => {
  const nodes: VNode[] = []

  for (const [ink, text] of props.row) {
    for (const chunk of chunks(text)) {
      nodes.push(h('span', { class: chunk.pin ? `ink-${ink} cell` : `ink-${ink}` }, chunk.text))
    }
  }

  return h('div', { class: 'line' }, nodes)
}

Line.props = ['row']

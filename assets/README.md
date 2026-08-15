# Brand assets

The logo, kept in one place so that changing it is one commit and nothing
has to be tracked down.

```
assets/logo/
  kobune-mark.svg         the mark alone, transparent
  kobune-icon.svg         the mark on a rounded dark square
  kobune-icon-square.svg  the same on an unrounded square
```

All three are 512×512 and hold the same drawing in the same three colours
— `#B1CEF1` for the outer folds, `#00B4DB` for the sail, `#003357` for the
hull. They differ only in what is behind it. The two icons sit the boat on
a `#00B4DB`→`#003357` radial; the mark has no tile and is drawn straight
onto whatever the page already has.

## Which one to use

- **`kobune-mark.svg`** — anywhere the page already has a background of its
  own: the README, the docs nav, the home page hero. Its own three tones do
  the work of contrast — the pale folds carry it on a dark page and the hull
  on a white one — so it does not need a variant per theme.
- **`kobune-icon.svg`** — anywhere the logo is the whole tile and something
  else draws the frame: the favicon, an app icon, a social profile.
- **`kobune-icon-square.svg`** — the same, for the platforms that round the
  corners themselves (iOS, Android) and would otherwise round an already
  rounded icon.

## Where they are used

| Where | Which | How it gets there |
| --- | --- | --- |
| `README.md` | mark | relative link to this directory |
| Docs nav and favicon | mark, icon | `docs/.vitepress/config.ts` |
| Docs home hero | mark | `docs/index.md`, `docs/ja/index.md` |
| The social card | mark | `docs/scripts/og.mjs` draws it |

The docs do not keep a copy. `pnpm sync` in `docs/` copies this directory
into `docs/public/logo/` before dev and before build — the same arrangement
`install.sh` already has — and the copy is git-ignored. So the files here
are the only ones in the repository, and a page refers to
`/logo/kobune-mark.svg`.

## The social card

`docs/scripts/og.mjs` composes `og.png` — the mark, the name and the
tagline on the dark background — and `pnpm sync` runs it, so the card is
redrawn on every build from whatever `kobune-mark.svg` currently is. There
is no card image in the repository to redo by hand, and none to forget.

It is a PNG because no crawler renders SVG, and 1200×630 because that is
what they all crop to. The type is Inter, taken from the copy VitePress
already installs rather than committed here.

## Replacing the logo

Overwrite the three files, keeping the names. Nothing else here needs
editing: the README links to them directly, the docs copy whatever is
here, and the social card is drawn from it on the next build. The one
thing outside this directory that follows the logo is the brand blue,
spelled out in `docs/.vitepress/config.ts` and `docs/scripts/og.mjs`.

`kobune-mark.svg` is the export, and the two icons are it on a tile: the
same paths, scaled into a 352-wide box and centred on the drawing's own
bounds, which sit low on the canvas. The square one is the rounded one
without the corner radius. Redo them that way rather than tidying any of
the three by hand — a straightened path would have to be straightened
again after the next export, and would be forgotten once. Size and
position them where they are used instead.

`.github/workflows/docs.yml` watches `assets/**`, so a logo change on its
own still deploys the site. `release.yml` ignores it, because a logo has
never been inside a binary.

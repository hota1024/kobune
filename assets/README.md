# Brand assets

The logo, kept in one place so that changing it is one commit and nothing
has to be tracked down.

```
assets/logo/
  minato-mark.svg         the mark alone, transparent
  minato-icon.svg         the mark on a rounded dark square
  minato-icon-square.svg  the same on an unrounded square
```

All three are 1024×1024 exports and hold the same drawing; they differ only
in what is behind it. `#0092FA` on `#020202`.

## Which one to use

- **`minato-mark.svg`** — anywhere the page already has a background of its
  own: the README, the docs nav, the home page hero. It reads on light and
  dark alike, so it does not need a variant per theme.
- **`minato-icon.svg`** — anywhere the logo is the whole tile and something
  else draws the frame: the favicon, an app icon, a social profile.
- **`minato-icon-square.svg`** — the same, for the platforms that round the
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
`/logo/minato-mark.svg`.

## The social card

`docs/scripts/og.mjs` composes `og.png` — the mark, the name and the
tagline on the dark background — and `pnpm sync` runs it, so the card is
redrawn on every build from whatever `minato-mark.svg` currently is. There
is no card image in the repository to redo by hand, and none to forget.

It is a PNG because no crawler renders SVG, and 1200×630 because that is
what they all crop to. The type is Inter, taken from the copy VitePress
already installs rather than committed here.

## Replacing the logo

Overwrite the three files, keeping the names. Nothing else needs editing:
the README links to them directly, the docs copy whatever is here, and the
social card is drawn from it on the next build.

They are exports and are **not edited by hand** — a tightened viewBox or a
tidied path would have to be redone after every re-export, and would be
forgotten once. Size and position them where they are used instead.

`.github/workflows/docs.yml` watches `assets/**`, so a logo change on its
own still deploys the site. `release.yml` ignores it, because a logo has
never been inside a binary.

# The documentation site

VitePress, in English and Japanese, with versioned snapshots.

```console
$ cd docs
$ pnpm install
$ pnpm dev       # http://localhost:5173
$ pnpm build
```

This is the only Node in the repository, and it builds the site — nothing
here ships in the product.

## Layout

```
docs/
  index.md            English home
  guide/              the guide
  reference/          CLI, kobune.toml, exit codes
  tutorials/          worked examples
  ja/                 the same tree in Japanese
  v0.1/               a released version, frozen (created at release time)
  .vitepress/
    config.ts         nav, sidebar and locales, generated from one page list
    versions.json     which versions have been snapshotted
    theme/            the home page, and the bar every page carries
  scripts/og.mjs      draws the social card
  public/             git-ignored; filled by `pnpm sync`, see below
  DESIGN.md           the design record. Not part of the site
```

## What the site serves that is not written here

`pnpm sync` runs before `dev` and before `build`, and fills `public/`:

- `install.sh` from the repository root, so
  <https://kobune.1024.works/install.sh> is the script the README tells
  people to pipe into a shell.
- `assets/logo/` as `public/logo/`, which is where the nav logo, the
  favicon and the home page hero come from. `assets/README.md` says which
  file is which and what to do when the logo changes.
- `og.png`, the social card, drawn by `scripts/og.mjs` from that same logo.

None of it is committed: the sources live outside `docs/`, and the card is
composed rather than stored, so a new logo reaches all three by being
dropped into `assets/logo/`. `.github/workflows/docs.yml` watches both
paths, so changing either deploys the site.

The card is set in Inter, read from the copy VitePress installs. resvg
cannot read woff2, so `wawoff2` decompresses it first — the alternative is
`loadSystemFonts`, which would set the card in whatever the machine
happens to have and make a runner's output differ from a laptop's.

## The home page

`.vitepress/theme/` extends the default theme and fills five of the home
page's slots. Everything it styles is inside `.VPHome`, so a documentation
page looks as it did.

One thing is not the home page's: `SiteBanner`, in `layout-bottom`, is the
bar that says this is a nightly build, and it is on every page. It is fixed
to the top rather than sitting there in the document, so that its link does
not come before *skip to content* in the tab order, and it measures itself
into `--vp-layout-top-height` — the room the default theme reserves above
`VPContent` and `VPNav`. That variable is the only `--vp-` one this theme
sets.

Its centre is a session that plays: `kobune init`, a worktree, a second one
beside it, and the two previews. Two rules govern it.

**The console text is what the commands printed.** It lives in
`theme/demo/script.ts`, taken from `apps/cli/src/ui/`, and it is not
translated — the same rule as every console block on every other page. The
panels are laid out from their content by the same arithmetic `panel.rs` uses,
so a panel cannot come out one column short of its own border.

**The words around it are translated, and no checker sees them.**
`theme/copy.ts` holds everything the theme says in both languages — the bar
above the nav and the demo's captions — shaped like `TEXT` in `config.ts`. The
rest sits in `hero:`, `specs:` and `notes:` in the two `index.md` files.
`.claude/skills/prose/scripts/check.mjs` skips frontmatter and does not read
TypeScript, so both are held to the house style by hand.

## What agents read

`vitepress-plugin-llms`, configured in `config.ts`, adds an English-only
surface for AI agents on every build:

- `/llms.txt`, the index, in the sidebar's order and under its headings.
- `/llms-full.txt`, every English page in one file.
- `<page>.md` beside every page, so `/guide/installation.md` is
  `/guide/installation` as Markdown.

Japanese and the `/vX.Y/` snapshots are left out, derived from `LOCALES` and
`versions.json` rather than written out again. `srcExclude` covers the rest:
the plugin takes its pages from Vite's module graph, so a file that is not a
page never reaches it.

Three limits, all of them the plugin's behaviour rather than configuration.
The home page has no `.md` and is not in either index. Links inside the
generated files are the ones VitePress wrote — extension-less, so following
one lands on the HTML page. And the guide index is written to `/guide.md`
rather than `/guide/index.md`, which leaves its own `./installation` links
resolving a directory too high.

A fourth thing is not the plugin's: a heading given an explicit id with
`{#…}` keeps the syntax in the copied Markdown, and the prose checker cannot
resolve a link to one either. Both are reasons to let a heading make its own
anchor.

Every English page also carries a hidden `Are you an LLM? …` div pointing at
its `.md`. It stays out of the local search index. `injectLLMHint: false`
removes it.

## Adding a page

Add it to `PAGES` in `config.ts`, with a title for each language, then write
`guide/thing.md` and `ja/guide/thing.md`. The sidebar, both locales and every
future snapshot follow from that one entry — and an English page gains its
`.md` and its line in `llms.txt` with no further work.

A page in `PAGES` with no file behind it fails the build, which is deliberate:
half-translated navigation is worse than a missing page.

## Releasing a version

```console
$ cargo xtask docs snapshot 0.1
```

Copies the current tree — both languages — to `docs/v0.1/`, rewrites its
absolute links to point inside itself, and adds it to `versions.json`. The
version switcher appears in the nav once there is more than one version.

Then bump `CURRENT` in `config.ts` to whatever you are now writing for. The
root always holds the unreleased docs; snapshots are history and are not
edited.

## Deployment

Cloudflare Pages, at <https://kobune.1024.works>. Pushing to `main` deploys;
a pull request gets its own preview URL, which is the reason the docs are
hosted here — prose is reviewed by reading it, not by reading its diff.

`.github/workflows/docs.yml` builds on every pull request, including ones
from forks, and deploys only when the secrets are present. A fork cannot see
them, so its pull requests get the build check and no preview.

The credentials are **environment secrets on `Release`**, so the job names
that environment. Without it they are simply absent, which looks identical to
a fork's pull request and skips the deploy without complaining.

Deploying runs `pnpm deploy:pages`, with wrangler as a devDependency.
cloudflare/wrangler-action installs wrangler mid-job instead, and pnpm blocks
workerd's install script unless it is allowed by name — awkward for something
that is not in the lockfile.

### One-time setup

Already done, and recorded here for whoever has to do it again.

1. Create a Pages project named `minato-docs`. It has to exist before the
   first deploy: `wrangler pages deploy` creates one interactively and fails
   in CI.
2. Issue an API token with **Account → Cloudflare Pages → Edit**, and nothing
   else. No zone permissions are needed even though the custom domain is on
   Cloudflare.
3. Add it to the repository as `CLOUDFLARE_API_TOKEN`, alongside
   `CLOUDFLARE_ACCOUNT_ID`.
4. Point the Pages project's custom domain at `kobune.1024.works`. The zone
   is on Cloudflare, so the DNS record and the certificate are handled for
   you.

### The hostname is in two places

`HOSTNAME` in `config.ts` — used by the sitemap, the social card and
`llms.txt` — and the custom domain in Cloudflare. A sitemap has to carry
absolute URLs, so it cannot be derived.

Preview deployments therefore serve a sitemap pointing at production, which
is harmless: nothing crawls a preview. `llms.txt` is not harmless in the same
way. Its links are meant to be followed, so on a preview they read the live
site rather than the branch, and the file cannot be reviewed on the
deployment that built it.

## Conventions

- **Say what a thing is for before saying how to use it.** Someone reading a
  page has usually landed on it from a search.
- **Show real output.** Every console block here was produced by running the
  command, not typed out from memory.
- **Say what does not work.** Firecracker is planned and not usable yet, and
  nothing has been released — a reader is better served knowing that than
  discovering it. `build` and `cmd:` health checks were on this list until
  they shipped; check before repeating it.

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
  reference/          CLI, minato.toml, exit codes
  tutorials/          worked examples
  ja/                 the same tree in Japanese
  v0.1/               a released version, frozen (created at release time)
  .vitepress/
    config.ts         nav, sidebar and locales, generated from one page list
    versions.json     which versions have been snapshotted
  DESIGN.md           the design record. Not part of the site
```

## Adding a page

Add it to `PAGES` in `config.ts`, with a title for each language, then write
`guide/thing.md` and `ja/guide/thing.md`. The sidebar, both locales and every
future snapshot follow from that one entry.

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

Cloudflare Pages, at <https://minato.1024.works>. Pushing to `main` deploys;
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
4. Point the Pages project's custom domain at `minato.1024.works`. The zone
   is on Cloudflare, so the DNS record and the certificate are handled for
   you.

### The hostname is in two places

`sitemap.hostname` in `config.ts` and the custom domain in Cloudflare. A
sitemap has to carry absolute URLs, so it cannot be derived. Preview
deployments therefore serve a sitemap pointing at production, which is
harmless: nothing crawls a preview.

## Conventions

- **Say what a thing is for before saying how to use it.** Someone reading a
  page has usually landed on it from a search.
- **Show real output.** Every console block here was produced by running the
  command, not typed out from memory.
- **Say what does not work.** `build`, `cmd:` health checks and Firecracker
  are all unimplemented, and a reader is better served knowing that than
  discovering it.

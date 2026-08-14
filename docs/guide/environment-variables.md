# Environment variables

Three layers, resolved last-wins, plus a set Kobune injects underneath all of
them.

## The layers

| Layer | Where | Committed? |
| --- | --- | --- |
| **global** | `~/.kobune/env` | No — your machine |
| **project** | `env` in `kobune.toml`, and `.kobune/env` | Yes |
| **workspace** | `.kobune/env.local` | No — gitignore it |

Later wins. A workspace value beats a project value beats a global one.

```console
$ kobune env ls
╭ environment ────────────────────────────────────╮
│ KEY           SCOPE      VALUE                  │
│ DATABASE_URL  project    postgres://db:5432/app │
│ LOG_LEVEL     workspace  debug                  │
│ API_KEY       global     ****                   │
╰─────────────────────────────────────────────────╯
```

**The layer is always shown**, because with three of them the hardest bug is a
value winning from somewhere you were not looking.

## Setting them

```console
$ kobune env set LOG_LEVEL=debug                    # workspace, the default
$ kobune env set DATABASE_URL=… --scope project     # committed, shared
$ kobune env set GITHUB_TOKEN=… --scope global      # every project
$ kobune env unset LOG_LEVEL
```

Write through `kobune env` rather than editing the files. It puts the value in
the layer you meant and keeps the format consistent.

::: warning A change needs a restart
Containers that are already running do not pick up a new value.
`kobune down && kobune up`.
:::

## What Kobune injects

Every service receives these, underneath your own values so you can override
any of them:

```
KOBUNE_PROJECT      = myapp
KOBUNE_WORKSPACE    = feature-user-auth
KOBUNE_SERVICE      = web
KOBUNE_CACHE_DIR    = /var/cache/kobune
KOBUNE_CA_FILE      = /etc/kobune/ca.crt
NODE_EXTRA_CA_CERTS = /etc/kobune/ca.crt
KOBUNE_URL_WEB      = https://web.feature-user-auth.myapp.localhost
KOBUNE_URL_API      = https://api.feature-user-auth.myapp.localhost
KOBUNE_HOSTNAME_WEB = web.feature-user-auth.myapp.localhost
KOBUNE_HOSTNAME_API = api.feature-user-auth.myapp.localhost
```

### `KOBUNE_CA_FILE`

Kobune's own CA certificate, mounted read-only into every service, so a
service can call `KOBUNE_URL_<SERVICE>` over HTTPS and have it verify.

The browser trusts that certificate because `kobune setup` put it in the
host's keychain. A container has its own trust store and does not get it, so
without this the URL connects and then fails on the certificate — and the way
out everyone finds is turning verification off for the whole process.

**Node needs no wiring**: `NODE_EXTRA_CA_CERTS` is set to the same file, so a
`fetch` from a server component or an API route verifies and nothing has to be
turned off. It is set underneath your own values — an image that points it at a
corporate bundle keeps that bundle by saying so:

```toml
[services.web.env]
NODE_EXTRA_CA_CERTS = "/etc/ssl/corporate-and-kobune.pem"
```

Node takes one file there, so trusting both means one file holding both.

**Only Node's is set for you.** `NODE_EXTRA_CA_CERTS` adds to the trust store,
while `SSL_CERT_FILE`, `CURL_CA_BUNDLE` and `REQUESTS_CA_BUNDLE` replace it — a
container told to trust Kobune through one of those trusts nothing else, and an
outbound call to anywhere but Kobune stops working. Point them at a bundle you
built, or use the system store below.

`NODE_EXTRA_CA_CERTS` is the one injected value left out of
[`env_file`](#writing-it-to-a-file): that file is read on the *host*, where
`/etc/kobune/ca.crt` does not exist and Node would warn about it on every
start. `KOBUNE_CA_FILE` is there, for a project that wants to name the path
itself.

### When the certificate does not reach the process

A task runner between the container and your server can drop it. Turborepo's
strict environment mode — the default in Turborepo 2 — passes through only what
its configuration names, so `NODE_EXTRA_CA_CERTS` reaches `turbo` and not the
`next dev` it starts, and the error is the one this section exists to prevent:

```
[cause]: Error: self-signed certificate in certificate chain {
  code: 'SELF_SIGNED_CERT_IN_CHAIN'
}
```

Name it in `turbo.json` and it goes through:

```json
{ "globalPassThroughEnv": ["NODE_EXTRA_CA_CERTS"] }
```

Nothing Kobune injects can reach past a tool that filters the environment,
since Node takes its extra certificate from the environment and nowhere else.
If a variable is set in the container and missing in the process, look for what
is in between.

For a stack that reads the system trust store instead, add the certificate to
it at start:

```toml
[services.api]
command = "sh -c 'cp $KOBUNE_CA_FILE /usr/local/share/ca-certificates/ && update-ca-certificates && ./serve'"
```

It is absent while there is no HTTPS to verify — no proxy on 443, no
certificate to trust. Like every other value here, a container that is already
running does not pick it up: `kobune down && kobune up`.

`/etc/kobune/ca.crt` is Kobune's own mount, so a `volumes` entry on that exact
path is refused the way `/var/cache/kobune` is.

### `KOBUNE_CACHE_DIR`

Somewhere to put what is worth keeping and not worth committing. It is a
volume Kobune manages, mounted into every service.

```toml
[services.web.env]
npm_config_store_dir = "${KOBUNE_CACHE_DIR}/pnpm"
CARGO_HOME = "${KOBUNE_CACHE_DIR}/cargo"
```

::: warning The braces are not optional
`${KOBUNE_CACHE_DIR}` is [a reference](#referring-to-another-variable) and
Kobune expands it. `$KOBUNE_CACHE_DIR` without them is passed through as
written, and Docker does not expand it either, so
`npm_config_store_dir = "$KOBUNE_CACHE_DIR/pnpm"` makes a directory *called*
`$KOBUNE_CACHE_DIR` relative to the workdir, which is the worktree: the
gigabyte-in-the-repository this exists to prevent. `kobune up` warns when a
value does this.

Braces are not needed where a shell does the expanding — a `command`, or a
start-up script:

```toml
command = "sh -c 'pnpm config set store-dir $KOBUNE_CACHE_DIR/pnpm && pnpm dev'"
```
:::

**Point your package manager at it.** Left alone, most of them cache under the
working directory — which is your worktree, bind-mounted from the host, so the
cache lands in the repository. A pnpm store there is a gigabyte of untracked
files in a checkout.

Shared by every worktree of the project, which is the point: a package store is
worth downloading once. For anything a branch changes the shape of — a
`node_modules` against a per-branch lockfile — use a
[`@workspace` volume](../reference/kobune-toml#scope) instead.

::: warning A container that does not run as root
The volume starts empty and owned by root, so a service running as another
user cannot write to it until something creates a directory it owns. `USER
root` for the install step, or `mkdir -p "$KOBUNE_CACHE_DIR/x" && chown` in the
start-up script.
:::

A container keeps the mounts it was created with, so a service that was
already running when you upgraded does not have this until `kobune down &&
kobune up`. Mounting your own volume at `/var/cache/kobune` is refused — two
mounts on one path is an error from the container engine, a long way from the
line that caused it.

`kobune env ls` shows only what every service shares, so `KOBUNE_SERVICE` and
a service's own `env` appear under `kobune env ls --service <name>`.

### `KOBUNE_URL_<SERVICE>`

**The important one.** It is what makes a per-worktree environment hold
together: the frontend cannot hardcode the API's URL, because the URL is
different on every branch.

```js
const api = process.env.KOBUNE_URL_API ?? 'http://localhost:8080'
```

A `-` in a service name becomes `_`: `api-server` gives
`KOBUNE_URL_API_SERVER`.

::: tip No URL means no proxy
The variable is left unset rather than empty when the proxy is not listening.
An empty string would leave it "set, but broken", which is much harder to
diagnose than a missing variable.

Inside the container this surfaces as `KOBUNE_URL_WEB: parameter not set`,
which names nothing that leads back here. `kobune up` warns when it starts
services with no proxy, and `kobune doctor` says how to get one.
:::

### `KOBUNE_HOSTNAME_<SERVICE>`

The same host with nothing around it — no scheme, no port, no trailing slash.

```toml
[services.web.env]
NEXT_ALLOWED_DEV_ORIGIN = "${KOBUNE_HOSTNAME_WEB}"

[services.api.env]
COOKIE_DOMAIN = "${KOBUNE_HOSTNAME_API}"
```

**A CORS origin, `allowedDevOrigins` and a cookie domain all want this rather
than a URL**, and cutting the scheme off `KOBUNE_URL_<SERVICE>` with `sed` is
what a project ends up doing without it.

It appears under the same condition as the URL: while the proxy is listening,
and only for a service that publishes one. A hostname nothing answers on would
be the same "set, but broken" the URL avoids.

::: warning Not `KOBUNE_HOST_<SERVICE>`
That name is Apple Container's, and it carries a peer's IP address — a
different thing entirely. See [Runtimes](./runtimes).
:::

## Referring to another variable

`${NAME}` in a value is replaced with whatever `NAME` resolves to.

```toml
[services.web.env]
NEXT_PUBLIC_WEB_URL = "${KOBUNE_URL_WEB}"
NEXT_PUBLIC_API_URL = "${KOBUNE_URL_API}"
FILE_BASE_URL       = "${KOBUNE_URL_API}/dev/r2"
```

**This is what puts a per-worktree URL under the name your application already
reads.** `KOBUNE_URL_API` arrives under Kobune's name for it; without a way to
say this, every project ends up with a start-up script whose whole job is to
copy one variable onto another.

A reference resolves to the value the container is given, from whichever layer
won — so overriding `KOBUNE_URL_API` in `.kobune/env.local` overrides
everything built out of it too. References may chain. `kobune env ls` shows
what they came to, since a listing of unexpanded values would be a listing of
something nothing runs with.

- **`$NAME` without braces is left alone.** These values have always been
  passed through as written, and expanding them now would change what existing
  configurations mean. Where the name is one that exists, `kobune up` says so
  rather than leaving you to find out from the symptom.
- **`$$` is a literal `$`**, so `$${A}` passes `${A}` through untouched.
- **What is not a variable name is not a reference.** `${PORT:-3000}` is shell
  syntax and reaches the shell unchanged.
- **A name nothing sets is an error**, not an empty string — the same reason
  `KOBUNE_URL_<SERVICE>` is left unset when there is no proxy. So referring to
  `${KOBUNE_URL_API}` makes the service refuse to start while the proxy is
  down, rather than start with the variable missing; `kobune doctor` says how
  to get one back.

`kobune env ls` still lists when something in it will not settle. **Only the
value at fault is shown as written**, with the reason under the listing — this
is the tool for finding it, and it can only be found by looking at the values.
Everything that does settle is shown settled, so the two can be told apart.

A listing of no particular service leaves out `KOBUNE_SERVICE` and every
service's own `env`, so a value built from one of those cannot settle *here*
even though the service starts fine. It says so, and names the service whose
listing can settle it — only that one will.

In `--json`, a value that did not settle carries an `unsettled` object with
the name it refers to and a reason (`undefined`, `only_with_service`,
`needs_proxy`, `secret`, `cycle`). A value that settled has no such field.

::: warning Values written before this existed
`${...}` and `$$` now mean something they did not. A value already holding one
changes: `$$` becomes a single `$`, and `${NAME}` naming a variable that does
not exist stops `kobune up` rather than being passed through. Double the
dollar — `$$` for a literal `$`, `$${` for a literal `${` — for anything meant
as text.
:::

::: warning A secret cannot be built into another value
`DATABASE_URL = "postgres://user:${PASSWORD}@db/app"` is refused when
`PASSWORD` is a `op://` or `keychain://` reference. Those are resolved in
memory when the container starts, and expanding one here would put the secret
into `kobune env ls` and into anything written out of it.

Store the composed value as the secret, or give the application the two
variables and let it join them.
:::

## Writing it to a file

Some tools do not read the environment they are started with. `wrangler dev`
does not pass its own to the Worker; Vite and dotenvx read a file off disk.
`env_file` writes the settled values where they can find them:

```toml
[services.api]
env_file = ".kobune/env.api"
```

```sh
wrangler dev --env-file .env --env-file .kobune/env.api
```

The path is relative to the worktree, and the file is written before the
service starts — on `kobune up` and again whenever scale-to-zero wakes it. It
is left in place afterwards, so `pnpm dev` run from the worktree by hand reads
the same values.

**Only for the services being started.** `kobune up web` writes `web`'s file
and whatever its `depends_on` pulls in, not `api`'s — so a service nobody
asked to start cannot fail the ones that were asked for, and a path only `api`
was pointed at is only written when `api` runs. `kobune exec` writes nothing:
it runs a command, it does not start a service.

**Rewriting it unchanged is not a write**, so a dev server watching the file
does not restart every time the service wakes.

- **A path git tracks is refused.** A generated file leaves the worktree dirty
  for good, and committing it would put one branch's URLs into every other
  checkout. Write somewhere gitignored — `.kobune/` is already there.
- **A file Kobune did not write is never overwritten.** The header line is the
  marker, so an `.env.local` of your own is safe: you get an error naming it,
  not a replacement.
- **Not `.kobune/env` or `.kobune/env.local`.** Kobune reads those two as
  layers of its own, so writing one would feed the generated file straight
  back in — and the workspace layer outranks everything. Write beside them.
- **One path per service.** Two services sharing a file would overwrite each
  other's environment at every start.
- **Not on `scope = "project"`.** A shared service is mounted no worktree, so
  the file would land where that container cannot see it.

::: warning Secrets are left out
Keys whose value is a `op://` or `keychain://` reference are named in a
comment and not written. A resolved secret lives in the daemon's memory and
never touches disk; a file would be handed on to whatever reads it, and that
is the end of the guarantee.

If a tool needs the secret itself, give it a `.env` of your own and pass both
files.
:::

## Secrets

Do not commit secrets. Write a reference and Kobune resolves it when the
container starts:

```
DATABASE_PASSWORD = op://Development/myapp/password    # 1Password CLI
API_KEY           = keychain://kobune/myapp/api-key    # macOS Keychain
STRIPE_KEY        = env://STRIPE_KEY                   # the daemon's environment
```

The resolved value goes to the container in memory and **never touches disk**.
`kobune env ls` shows the reference, not the value — including with `--reveal`,
because printing it would mean resolving it, and that only happens at start.

### When resolution fails

The daemon does not stop. Usually it means nobody is signed in to 1Password,
and letting that keep an entire environment down would be the wrong trade. The
key is dropped and you get a warning:

```
warning: cannot resolve the secret for DATABASE_PASSWORD: cannot reach op
```

Your app then fails on a missing variable, which is a clearer failure than a
wrong one.

## Reading one value

```console
$ kobune env get DATABASE_URL
postgres://db:5432/app
```

One line, no decoration, for scripts. Unlike `env ls` this prints the real
value — you asked for it specifically.

**It fails rather than printing an unsettled one.** Where `env ls` falls back
to showing `${...}` as written, handing that to a script would put the braces
into whatever read it.

## Files, if you prefer

```
~/.kobune/env              global
.kobune/env                project, committed
.kobune/env.local          workspace, gitignored
```

Plain `KEY=value`, one per line, `#` for comments. Add `.kobune/env.local` to
`.gitignore`.

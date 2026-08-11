# Environment variables

Three layers, resolved last-wins, plus a set Minato injects underneath all of
them.

## The layers

| Layer | Where | Committed? |
| --- | --- | --- |
| **global** | `~/.minato/env` | No — your machine |
| **project** | `env` in `minato.toml`, and `.minato/env` | Yes |
| **workspace** | `.minato/env.local` | No — gitignore it |

Later wins. A workspace value beats a project value beats a global one.

```console
$ minato env ls
╭ environment ────────────────────────────────────╮
│ KEY           SCOPE      VALUE                  │
│ DATABASE_URL  project    postgres://db:5432/app │
│ LOG_LEVEL     workspace  debug                  │
│ API_KEY       global     ****                   │
╰─────────────────────────────────────────────────╯
```

**The layer is always shown**, because with three of them the hardest bug is a
value winning from somewhere you weren't looking.

## Setting them

```console
$ minato env set LOG_LEVEL=debug                    # workspace, the default
$ minato env set DATABASE_URL=… --scope project     # committed, shared
$ minato env set GITHUB_TOKEN=… --scope global      # every project
$ minato env unset LOG_LEVEL
```

Write through `minato env` rather than editing the files. It puts the value in
the layer you meant and keeps the format consistent.

::: warning A change needs a restart
Containers that are already running do not pick up a new value.
`minato down && minato up`.
:::

## What Minato injects

Every service receives these, underneath your own values so you can override
any of them:

```
MINATO_PROJECT      = myapp
MINATO_WORKSPACE    = feature-user-auth
MINATO_SERVICE      = web
MINATO_CACHE_DIR    = /var/cache/minato
MINATO_URL_WEB      = https://web.feature-user-auth.myapp.localhost
MINATO_URL_API      = https://api.feature-user-auth.myapp.localhost
```

### `MINATO_CACHE_DIR`

Somewhere to put what is worth keeping and not worth committing. It is a
volume Minato manages, mounted into every service.

```toml
[services.web.env]
npm_config_store_dir = "${MINATO_CACHE_DIR}/pnpm"
CARGO_HOME = "${MINATO_CACHE_DIR}/cargo"
```

::: warning The braces are not optional
`${MINATO_CACHE_DIR}` is [a reference](#referring-to-another-variable) and
Minato expands it. `$MINATO_CACHE_DIR` without them is passed through as
written, and Docker does not expand it either, so
`npm_config_store_dir = "$MINATO_CACHE_DIR/pnpm"` makes a directory *called*
`$MINATO_CACHE_DIR` relative to the workdir, which is the worktree: the
gigabyte-in-the-repository this exists to prevent. `minato up` warns when a
value does this.

Braces are not needed where a shell does the expanding — a `command`, or a
start-up script:

```toml
command = "sh -c 'pnpm config set store-dir $MINATO_CACHE_DIR/pnpm && pnpm dev'"
```
:::

**Point your package manager at it.** Left alone, most of them cache under the
working directory — which is your worktree, bind-mounted from the host, so the
cache lands in the repository. A pnpm store there is a gigabyte of untracked
files in a checkout.

Shared by every worktree of the project, which is the point: a package store is
worth downloading once. For anything a branch changes the shape of — a
`node_modules` against a per-branch lockfile — use a
[`@workspace` volume](../reference/minato-toml#scope) instead.

::: warning A container that does not run as root
The volume starts empty and owned by root, so a service running as another
user cannot write to it until something creates a directory it owns. `USER
root` for the install step, or `mkdir -p "$MINATO_CACHE_DIR/x" && chown` in the
start-up script.
:::

A container keeps the mounts it was created with, so a service that was
already running when you upgraded does not have this until `minato down &&
minato up`. Mounting your own volume at `/var/cache/minato` is refused — two
mounts on one path is an error from the container engine, a long way from the
line that caused it.

`minato env ls` shows only what every service shares, so `MINATO_SERVICE` and
a service's own `env` appear under `minato env ls --service <name>`.

`MINATO_URL_<SERVICE>` is the important one. It is what makes a per-worktree
environment hold together: the frontend cannot hardcode the API's URL, because
the URL is different on every branch.

```js
const api = process.env.MINATO_URL_API ?? 'http://localhost:8080'
```

A `-` in a service name becomes `_`: `api-server` gives
`MINATO_URL_API_SERVER`.

::: tip No URL means no proxy
The variable is left unset rather than empty when the proxy is not listening.
An empty string would leave it "set, but broken", which is much harder to
diagnose than a missing variable.

Inside the container this surfaces as `MINATO_URL_WEB: parameter not set`,
which names nothing that leads back here. `minato up` warns when it starts
services with no proxy, and `minato doctor` says how to get one.
:::

On Apple Container there is also `MINATO_HOST_<SERVICE>`, carrying a peer's IP
address. See [Runtimes](./runtimes).

## Referring to another variable

`${NAME}` in a value is replaced with whatever `NAME` resolves to.

```toml
[services.web.env]
NEXT_PUBLIC_WEB_URL = "${MINATO_URL_WEB}"
NEXT_PUBLIC_API_URL = "${MINATO_URL_API}"
FILE_BASE_URL       = "${MINATO_URL_API}/dev/r2"
```

**This is what puts a per-worktree URL under the name your application already
reads.** `MINATO_URL_API` arrives under Minato's name for it; without a way to
say this, every project ends up with a start-up script whose whole job is to
copy one variable onto another.

A reference resolves to the value the container is given, from whichever layer
won — so overriding `MINATO_URL_API` in `.minato/env.local` overrides
everything built out of it too. References may chain. `minato env ls` shows
what they came to, since a listing of unexpanded values would be a listing of
something nothing runs with.

- **`$NAME` without braces is left alone.** These values have always been
  passed through as written, and expanding them now would change what existing
  configurations mean. Where the name is one that exists, `minato up` says so
  rather than leaving you to find out from the symptom.
- **`$$` is a literal `$`**, so `$${A}` passes `${A}` through untouched.
- **What is not a variable name is not a reference.** `${PORT:-3000}` is shell
  syntax and reaches the shell unchanged.
- **A name nothing sets is an error**, not an empty string — the same reason
  `MINATO_URL_<SERVICE>` is left unset when there is no proxy. So referring to
  `${MINATO_URL_API}` makes the service refuse to start while the proxy is
  down, rather than start with the variable missing; `minato doctor` says how
  to get one back.

::: warning Values written before this existed
`${...}` and `$$` now mean something they did not. A value already holding one
changes: `$$` becomes a single `$`, and `${NAME}` naming a variable that does
not exist stops `minato up` rather than being passed through. Double the
dollar — `$$` for a literal `$`, `$${` for a literal `${` — for anything meant
as text.
:::

::: warning A secret cannot be built into another value
`DATABASE_URL = "postgres://user:${PASSWORD}@db/app"` is refused when
`PASSWORD` is a `op://` or `keychain://` reference. Those are resolved in
memory when the container starts, and expanding one here would put the secret
into `minato env ls` and into anything written out of it.

Store the composed value as the secret, or give the application the two
variables and let it join them.
:::

## Writing it to a file

Some tools do not read the environment they are started with. `wrangler dev`
does not pass its own to the Worker; Vite and dotenvx read a file off disk.
`env_file` writes the settled values where they can find them:

```toml
[services.api]
env_file = ".minato/env.api"
```

```sh
wrangler dev --env-file .env --env-file .minato/env.api
```

The path is relative to the worktree, and the file is written before the
service starts — on `minato up` and again whenever scale-to-zero wakes it. It
is left in place afterwards, so `pnpm dev` run from the worktree by hand reads
the same values.

**Rewriting it unchanged is not a write**, so a dev server watching the file
does not restart every time the service wakes.

- **A path git tracks is refused.** A generated file leaves the worktree dirty
  for good, and committing it would put one branch's URLs into every other
  checkout. Write somewhere gitignored — `.minato/` is already there.
- **A file Minato did not write is never overwritten.** The header line is the
  marker, so an `.env.local` of your own is safe: you get an error naming it,
  not a replacement.
- **Not `.minato/env` or `.minato/env.local`.** Minato reads those two as
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

Do not commit secrets. Write a reference and Minato resolves it when the
container starts:

```
DATABASE_PASSWORD = op://Development/myapp/password    # 1Password CLI
API_KEY           = keychain://minato/myapp/api-key    # macOS Keychain
STRIPE_KEY        = env://STRIPE_KEY                   # the daemon's environment
```

The resolved value goes to the container in memory and **never touches disk**.
`minato env ls` shows the reference, not the value — including with `--reveal`,
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
$ minato env get DATABASE_URL
postgres://db:5432/app
```

One line, no decoration, for scripts. Unlike `env ls` this prints the real
value — you asked for it specifically.

## Files, if you prefer

```
~/.minato/env              global
.minato/env                project, committed
.minato/env.local          workspace, gitignored
```

Plain `KEY=value`, one per line, `#` for comments. Add `.minato/env.local` to
`.gitignore`.

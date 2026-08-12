# `minato.toml`

Lives at the repository root and is committed. Every worktree reads the same
one.

```toml
[project]
name = "myapp"
# domain = "myapp.localhost"

[runtime]
default = "docker"

[services.web]
image = "node:22"
port = 3000
command = "npm run dev"
health = "http://localhost:3000/healthz"
idle_timeout = "30m"
depends_on = ["db"]
env = { NODE_ENV = "development" }

[services.db]
image = "postgres:16"
port = 5432
scope = "project"
expose = false
volumes = ["pgdata:/var/lib/postgresql/data"]
```

## `[project]`

| Key | Type | | |
| --- | --- | --- | --- |
| `name` | string | **required** | Appears in every URL. Must be unique across the projects one daemon manages |
| `domain` | string | `{name}.localhost` | The URL suffix. Anything other than `.localhost` needs its own `/etc/resolver` entry |
| `carry` | array | `[]` | Files to copy into a new worktree, relative to the repository root |

Registering two projects under one name is refused rather than allowed to
collide.

### `carry`

```toml
[project]
carry = [".env", "apps/api/.dev.vars"]
```

`git worktree add` gives a new worktree the tracked files and nothing else, so
an untracked but required `.env` is missing and the services cannot start.
These are copied from the main worktree during `minato new`, before anything
starts.

- **A missing source is not an error.** Not every checkout has a `.env` yet,
  and failing `minato new` over one would be worse than the gap it fills. It
  is reported, not passed over in silence.
- **An existing destination is never overwritten.** Whatever git just checked
  out wins — this is for what git does not carry, not a way to replace what it
  does.
- Permissions come along, so a `0600` `.env` stays `0600`.
- Paths that leave the repository are refused, including through a symlink.
  Directories are not copied; name the files.

## `[runtime]`

| Key | Type | | |
| --- | --- | --- | --- |
| `default` | string | `"docker"` | `"docker"` or `"apple"` |

See [Runtimes](../guide/runtimes).

## `[services.<name>]`

The service name appears in URLs and in `MINATO_URL_<SERVICE>`, so keep it to
letters, digits and `-`.

### Image and command

| Key | Type | | |
| --- | --- | --- | --- |
| `image` | string | one of these | A prebuilt image. `postgres:16`, `docker.io/library/node:22` |
| `build` | string | one of these | A build context, relative to the worktree. Mutually exclusive with `image` |
| `dockerfile` | string | `{build}/Dockerfile` | The Dockerfile, relative to the worktree. Needs `build` |
| `build_args` | table | `{}` | `--build-arg` values. Needs `build` |
| `command` | string | image default | Replaces the image's command. Parsed shell-style, so quotes group arguments |
| `setup` | string | — | Run once before the service first starts. Parsed shell-style |
| `workdir` | string | `/workspace` | Working directory inside the container |
| `tty` | bool | `false` | Run the process on a terminal, with its stdin left open |

Your worktree is mounted at `/workspace`, which is why that is the default.

#### `tty`

```toml
[services.dev]
image = "node:24-bookworm-slim"
command = "npx turbo run dev"
tty = true
```

What a program looks for before it draws anything. Turborepo, Vitest and the
rest ask whether they are talking to a terminal and settle for plain
scrolling text when they are not — which is what a container gives them
without this. With it, colour comes through and
[`minato logs -f dev`](./cli#logs) becomes that terminal: what you type
reaches the program.

::: warning A terminal changes what the logs are
The two output streams become one, so nothing tells stderr from stdout any
more, and lines arrive ending `\r\n`. That is what a terminal is, not
something Minato adds. Leave `tty` off for a service whose logs get piped
into something.
:::

Whether a container has a terminal is fixed when it is created, so turning
this on for a service that is already running recreates it — a restart, on
the next `minato up`.

#### `setup`

```toml
[services.web]
image = "node:24-bookworm-slim"
setup = "sh -c 'pnpm install --frozen-lockfile'"
command = "sh -c 'pnpm dev'"
```

Runs before the service first starts, so `command` is left doing nothing but
starting the app. It runs in a container of its own with the service's image,
environment and volumes, so what it installs into a volume is there when the
real container comes up.

**Once per worktree, not once per container.** A stopped container is
recreated by the next `up`, so anything tied to container creation would run
on every `down`/`up` — which is what this exists to avoid. Minato remembers
the command it ran against the worktree:

- Change what `setup` says and it runs again. There is nothing else to
  compare, so editing it is the way to re-run it — **changing `image` does
  not**, so a native module built against the old runtime stays in the volume
  until you say otherwise
- A `setup` that fails stops the `up` and is not remembered, so fixing it and
  running `up` again retries
- `minato rm` forgets it, along with the `@workspace` volumes it populated
- A `scope = "project"` service is set up once for the project, not once per
  worktree — it has one container for all of them

It runs in `startup_order`, immediately before its own service starts, so
anything it names in `depends_on` is already up. Migrations against a `db`
work; what does not is a `setup` that expects its *own* service to be
running, because that is the thing it is about to start.

**One `setup` runs at a time**, even where the services around it start
together. Every service mounts the same project-wide cache volume, so two
installs at once would be two arbitrary commands writing to one directory —
safe for a package manager's own store, which is built for it, and not
something Minato can promise for whatever else a `setup` does. So it does
not: the first `up` after `minato new` pays for its setups end to end, and
later ones find them recorded and skip straight past.

Waking a stopped service with a request does not run `setup` — only `minato
up` does, so an edit takes effect on the next `up` rather than on the next
request.

Not to be confused with `minato setup`, which is the privileged host setup.

### Networking

| Key | Type | | |
| --- | --- | --- | --- |
| `port` | integer | — | The port the app listens on **inside** the container |
| `expose` | boolean | `true` when `port` is set | Whether to give it a URL. Without one it is reached and stopped through `depends_on` |

There is no host port to configure. Docker forwards to a port it chooses; Apple
Container gives the container its own IP.

Bind `0.0.0.0` inside the container, not `127.0.0.1` — a server on loopback
inside a container is unreachable from outside it.

#### Building

```toml
[services.web]
build = "."
dockerfile = "./docker/web.Dockerfile"   # optional
build_args = { NODE_VERSION = "22" }     # optional
port = 3000
```

The context comes from **the worktree, not the main checkout**, so a branch
that edits its Dockerfile gets the image that Dockerfile describes. It has to
stay inside the worktree; `build = "../.."` is refused rather than handed to
the runtime as a build context.

The image is tagged `minato-{project}-{service}:{fingerprint}`, where the
fingerprint covers the Dockerfile and the build args. Two consequences worth
knowing:

- Two worktrees whose Dockerfiles agree land on the same tag and **share one
  image**, built once.
- **A build is skipped when that tag already exists.** This is what keeps
  waking a stopped service from running a build.

::: warning A copied file does not trigger a rebuild
The fingerprint cannot see files the Dockerfile `COPY`s in, so editing
`package.json` alone does not cause a rebuild. Use `minato up --build`.
`docker compose` behaves the same way.
:::

### Readiness

| Key | Type | | |
| --- | --- | --- | --- |
| `health` | string | TCP connect | How to decide the service is ready |

```toml
health = "http://localhost:3000/healthz"   # 2xx or 3xx
health = "tcp://localhost:5432"            # a connection succeeds
health = "cmd:pg_isready -U postgres"      # runs inside the container
```

**Only the path is used** for `http://`. What you write is the address from
inside the container; Minato reaches it at whatever address the runtime
assigned.

Starting a service waits for this check to pass before moving on, which is
what keeps the `curl` right after `minato up` from meeting a connection
refused.

::: warning The wait gives up after 15 seconds
Waiting forever would mean `minato up` never returns, so a service that is
not ready by then is left starting and the command carries on. A dev server
that compiles for a minute on its first run will hit this. Nothing is
broken — the URL still works once it comes up, because reaching for it waits
— but `depends_on` stops being a guarantee at that point.
:::

### Lifecycle

| Key | Type | | |
| --- | --- | --- | --- |
| `idle_timeout` | duration | `"30m"` | Time without a request before it stops itself. Without a URL of its own, it follows the services that `depends_on` it |
| `depends_on` | array | `[]` | Services to start first, whether by `minato up` or by a request waking this one |
| `scope` | string | `"workspace"` | `"workspace"` or `"project"` |

Durations are `humantime`: `"30s"`, `"10m"`, `"2h"`.

`depends_on` starts a dependency first **and waits for it to be ready**, by
the same check [Readiness](#readiness) describes — so a service can assume
the ones it names are answering, up to the 15-second limit noted there. On
Apple Container it also decides whether `MINATO_HOST_<PEER>` is available,
since the address is read when the service starts.

`scope = "project"` shares one instance across every worktree. Good for a
database you do not want to seed repeatedly; bad when two branches carry
incompatible migrations.

### Storage

| Key | Type | | |
| --- | --- | --- | --- |
| `volumes` | array | `[]` | Mounts |

```toml
volumes = [
  "pgdata:/var/lib/postgresql/data",   # named, managed, shared across worktrees
  "node-modules@workspace:/workspace/node_modules",  # one per worktree
  "./seed:/seed",                      # host path, relative to the worktree
  "/etc/ssl/certs:/certs:ro",          # absolute, read-only
  "~/.cache/npm:/root/.npm",           # home-relative
]
```

A source with no `/` is named storage; anything starting with `/`, `./` or `~/`
is a host path. A `:ro` or `:rw` suffix sets the mode, defaulting to read-write.
The container path must be absolute.

**Named storage is already namespaced per project.** `pgdata` becomes the
Docker volume `minato-{project}-pgdata`, so there is no need to prefix the
name yourself — `myapp-pgdata` under project `myapp` would end up as
`minato-myapp-myapp-pgdata`.

#### Scope

Named storage is shared by every worktree of the project by default. That is
what makes it useful for a package cache, and what makes it wrong for anything
a branch can change the shape of — `node_modules` against a lockfile that
differs per branch would fight itself.

`@workspace` on the name gives each worktree its own:

```toml
volumes = [
  "pnpm-store:/pnpm-store",                          # shared
  "node-modules@workspace:/workspace/node_modules",  # one per worktree
  "certs@workspace:/certs:ro",                       # composes with :ro
]
```

| Written | Docker volume |
| --- | --- |
| `pnpm-store` | `minato-{project}-pnpm-store` |
| `node-modules@workspace` | `minato-{project}-{workspace}.node-modules` |

The worktree is joined with `.` rather than `-` on purpose. Projects,
worktrees and volume names are all DNS labels, so a hyphen occurs inside any
of them: joined with one, worktree `feat-1` with volume `cache` and the
project volume `feat-1-cache` would be the same storage. A `.` cannot appear
in a label, so the two forms can never meet.

A volume name has to be a label itself — lowercase letters, digits and
hyphens.

`@project` can be written out where being explicit helps; it is the default
either way. An unrecognised suffix is refused rather than treated as part of
the name, since `@worktree` would otherwise quietly produce a shared volume
called `node-modules@worktree`.

**A workspace volume goes when its worktree goes.** `minato rm` removes it
along with the containers, since there is no longer a worktree it belongs to.
Project volumes are left alone — they are shared, and outlive any one
worktree.

::: warning Changing the scope of an existing volume
The scope is part of the real name, so adding or removing `@workspace` points
the service at different storage. Nothing is deleted, but whatever the old
volume held stops being visible.
:::

A `scope = "project"` service cannot ask for `@workspace` storage: one
instance serves every worktree, so there is no worktree whose volume it would
be. That is refused when the configuration is read.

Apple Container has no named volumes, so they become bind mounts under
`~/.minato/volumes/<project>/`.

### Environment

| Key | Type | | |
| --- | --- | --- | --- |
| `env` | table | `{}` | Variables for this service |
| `env_file` | string | — | Where to write the settled environment, relative to the worktree |

```toml
env = { NODE_ENV = "development", PORT = "3000" }
```

A value may refer to another variable, which is how a per-worktree URL reaches
the name an application already reads:

```toml
env = { NEXT_PUBLIC_API_URL = "${MINATO_URL_API}" }
```

This is the *project* layer, and it is committed — keep secrets out of it. See
[Environment variables](../guide/environment-variables).

`env_file` writes the settled result where a tool that reads a file rather
than its own environment can find it:

```toml
env_file = ".minato/env.api"
```

Written before the service starts and only for the ones being started, secrets
left out. A path git tracks is
refused, a file Minato did not write is never overwritten, and so are
`.minato/env` and `.minato/env.local` — Minato reads those as layers of its
own — and any path another service already claims. A service with
`scope = "project"` cannot have one either: it is mounted no worktree to write
into.

## Validation

```console
$ minato status
✗ error: invalid configuration: service `web`: depends_on names an unknown service `database`
```

The configuration is checked when it is read. Unknown service references,
circular `depends_on`, malformed volumes, `carry` entries that leave the
repository and invalid durations are all caught before anything starts.

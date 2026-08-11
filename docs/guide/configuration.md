# Configuration

Everything lives in `minato.toml` at the repository root. It is committed, and
every worktree reads the same one.

For an exhaustive list of keys, see the
[`minato.toml` reference](../reference/minato-toml). This page is about the
decisions behind them.

## A minimal file

```toml
[project]
name = "myapp"

[services.web]
image = "node:22"
port = 3000
command = "npm run dev"
```

The project name appears in every URL, so it has to be unique across the
projects one daemon manages. Minato refuses to register two projects with the
same name rather than let their URLs collide.

## Several services

```toml
[services.web]
image = "node:22"
port = 3000
command = "npm run dev"
depends_on = ["api"]

[services.api]
image = "node:22"
port = 8080
command = "npm run api"
depends_on = ["db"]

[services.db]
image = "postgres:16"
port = 5432
scope = "project"
expose = false
volumes = ["pgdata:/var/lib/postgresql/data"]
env = { POSTGRES_PASSWORD = "postgres" }
```

`depends_on` sets the start order. It does not wait for the dependency to be
*healthy* before starting the next one, but it does start them in order, and
each is given time to come up.

## Scope: one per worktree, or one shared

This is the decision that matters most.

```toml
scope = "workspace"   # the default: one instance per worktree
scope = "project"     # one instance, shared by every worktree
```

A database per worktree means seeding each one and paying for all of them. A
shared one means every branch sees the same data, which is usually what you
want while developing — and occasionally exactly what you don't, when two
branches carry different migrations.

Minato does not solve the migration problem. If two branches migrate the same
shared database in incompatible ways, they will fight. Use
`scope = "workspace"` for those, and accept the seeding cost.

## Exposing services

```toml
expose = false
```

A service with `expose = false` gets no URL and no route. Other services still
reach it from inside, but nothing outside the environment can. Databases and
caches should almost always set it.

It defaults to true whenever `port` is set.

## Health checks

```toml
health = "http://localhost:3000/healthz"
health = "tcp://localhost:5432"
```

This is how Minato decides a service is *ready*, as opposed to merely started.
Without it, readiness means "a TCP connection succeeds", which for an HTTP
service can be true well before it can serve anything.

Two things to know:

- **Only the path is used** for `http://`. What you write is the address from
  inside the container; what Minato can reach is whatever the runtime assigned.
  The host and port you write are ignored.
- **`cmd:` runs inside the container.** `health = "cmd:pg_isready -U postgres"`
  is the only way to tell a database that accepts connections from one that
  will answer a query — postgres listens well before it has finished
  initialising. The command is split shell-style but runs without a shell, so
  wrap pipes in `sh -c`.

Readiness is also what scale-to-zero waits for when a request wakes a stopped
service, so a good health check makes the first request faster and more
reliable.

## Idle timeout

```toml
idle_timeout = "30m"
```

How long a service goes without a request before it stops itself. The default
is 30 minutes. Set it longer for something slow to start, shorter if you make
a lot of worktrees.

Time is measured from the last request through the proxy, so traffic between
containers does not count.

A service with no URL of its own — `expose = false`, which a database usually
is — has no request to measure. It follows the exposed services that name it in
`depends_on` instead: it stops once every one of them has gone quiet, and a
request that wakes one of them starts it back up first.

**So name it in `depends_on`.** With nothing pointing at it there is neither a
signal to stop on nor a way back up, and it stays running for as long as the
daemon does.

## Volumes

```toml
volumes = [
  "pgdata:/var/lib/postgresql/data",   # named, managed by the runtime
  "./seed:/seed",                      # a host path, relative to the worktree
  "/etc/ssl/certs:/certs:ro",          # absolute, read-only
]
```

A name without a slash is storage the runtime manages, scoped to the project so
it is shared between worktrees. Anything starting with `/`, `./` or `~/` is a
host path.

Node's `node_modules` is the usual reason to reach for a named volume: put one
at `/workspace/node_modules` per workspace and the install happens once.

## Environment variables

Small, non-secret values can live here:

```toml
[services.web]
env = { NODE_ENV = "development" }
```

Anything else — anything that differs per machine, per worktree, or that is a
secret — belongs in the layered environment instead. See
[Environment variables](./environment-variables).

## Choosing a runtime

```toml
[runtime]
default = "docker"   # or "apple"
```

Per project, so different repositories can use different backends. See
[Runtimes](./runtimes) for what changes when you switch.

## The URL suffix

```toml
[project]
name = "myapp"
domain = "myapp.localhost"   # the default, derived from name
```

Override `domain` to serve a project under something else. Whatever you choose
still has to resolve to 127.0.0.1, which for anything other than `.localhost`
means another `/etc/resolver` entry.

## Building your own image

Point `build` at a context instead of naming an `image`:

```toml
[services.web]
build = "."
port = 3000
command = "npm run dev"
```

The context comes from the worktree, so a branch that edits its Dockerfile
gets the image that Dockerfile describes.

Images are tagged with a fingerprint of the Dockerfile and the build args, so
two worktrees that agree share one image and a build is skipped when that exact
image already exists. That last part is what keeps waking a stopped service
fast.

The fingerprint cannot see a file the Dockerfile copies in, so a change to
`package.json` alone does not rebuild. `minato up --build` forces one.

Prefer a prebuilt image where you can. Mounting your source into `node:22`
starts faster than building, and it is the shorter path to a working
environment; reach for `build` when you need system packages or a toolchain
that an off-the-shelf image does not carry.

## What is not supported yet

- **`minato.local.toml`** — per-worktree overrides. Environment variable layers
  cover most of what it was for.

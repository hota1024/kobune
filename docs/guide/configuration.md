# Configuration

Everything lives in `kobune.toml` at the repository root. It is committed, and
every worktree reads the same one. Two more files may be merged over it where a
machine or a clone has to differ; [The other two
layers](#the-other-two-layers) is about those.

For an exhaustive list of keys, see the
[`kobune.toml` reference](../reference/kobune-toml). This page is about the
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
projects one daemon manages. Kobune refuses to register two projects with the
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

`depends_on` sets the start order, and a service waits for what it depends on
to be *ready* — not merely started — before it begins. The wait gives up after
15 seconds and carries on, so a dependency slower than that stops being a
guarantee. What makes `ready` mean anything is the health check below.

## Scope: one per worktree, or one shared

This is the decision that matters most.

```toml
scope = "workspace"   # the default: one instance per worktree
scope = "project"     # one instance, shared by every worktree
```

A database per worktree means seeding each one and paying for all of them. A
shared one means every branch sees the same data, which is usually what you
want while developing, and occasionally the opposite, when two branches carry
different migrations.

Kobune does not solve the migration problem. If two branches migrate the same
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

This is how Kobune decides a service is *ready*, as opposed to merely started.
Without it, readiness means "a TCP connection succeeds", which for an HTTP
service can be true well before it can serve anything.

Two things to know:

- **Only the path is used** for `http://`. What you write is the address from
  inside the container; what Kobune can reach is whatever the runtime assigned.
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
  "pgdata:/var/lib/postgresql/data",                 # named, one per project
  "node-modules@workspace:/workspace/node_modules",  # named, one per worktree
  "./seed:/seed",                                    # a host path, relative to the worktree
  "/etc/ssl/certs:/certs:ro",                        # absolute, read-only
]
```

A name without a slash is storage the runtime manages, scoped to the project so
it is shared between worktrees. Anything starting with `/`, `./` or `~/` is a
host path.

**`@workspace` on the name gives each worktree its own instead**, and
`node_modules` is what it is for. Shared across worktrees it fights itself, one
branch's lockfile installing over another's. Per worktree it costs an install
per branch and is correct.

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
[Runtimes](./runtimes) for what changes when you switch, and
[Choosing per machine](./runtimes#choosing-per-machine) where the answer is not
the same on every computer that clones the repository.

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
gets the image that Dockerfile describes. Builds run under BuildKit, the
builder `docker build` itself uses, so a Dockerfile that works on the command
line works here — cache mounts and all. A `.dockerignore` beside it is applied,
so what it names is never sent.

Images are tagged with a fingerprint of the Dockerfile and the build args, so
two worktrees that agree share one image and a build is skipped when that exact
image already exists. That last part is what keeps waking a stopped service
fast.

The fingerprint cannot see a file the Dockerfile copies in, so a change to
`package.json` alone does not rebuild. `kobune up --build` forces one.

Prefer a prebuilt image where you can. Mounting your source into `node:22`
starts faster than building, and it is the shorter path to a working
environment; reach for `build` when you need system packages or a toolchain
that an off-the-shelf image does not carry.

## The other two layers

`kobune.toml` is the middle of three files, and the other two are absent on
most checkouts. `~/.kobune/config.toml` is read before it and holds what is
true of the computer rather than of the project — `[runtime] default = "apple"`
on the Mac that runs Apple Container, with no project having to know about it.
`kobune.local.toml`, beside `kobune.toml`, is read after it and is the same
thing for one clone.

Tables merge and everything else replaces, so either one can set
`[services.web] port` without restating the image beside it. What they came to
is in no file you can open, so
[`kobune config show`](../reference/cli#configuration) is how to see it.

[Layers](../reference/kobune-toml#layers) has the whole of it.

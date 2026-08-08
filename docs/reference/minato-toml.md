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

Registering two projects under one name is refused rather than allowed to
collide.

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
| `workdir` | string | `/workspace` | Working directory inside the container |

Your worktree is mounted at `/workspace`, which is why that is the default.

### Networking

| Key | Type | | |
| --- | --- | --- | --- |
| `port` | integer | — | The port the app listens on **inside** the container |
| `expose` | boolean | `true` when `port` is set | Whether to give it a URL |

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
health = "cmd:pg_isready"                  # not supported yet
```

**Only the path is used** for `http://`. What you write is the address from
inside the container; Minato reaches it at whatever address the runtime
assigned.

### Lifecycle

| Key | Type | | |
| --- | --- | --- | --- |
| `idle_timeout` | duration | `"30m"` | Time without a request before it stops itself |
| `depends_on` | array | `[]` | Services to start first |
| `scope` | string | `"workspace"` | `"workspace"` or `"project"` |

Durations are `humantime`: `"30s"`, `"10m"`, `"2h"`.

`depends_on` sets order. On Apple Container it also decides whether
`MINATO_HOST_<PEER>` is available, since the address is read when the service
starts.

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
  "./seed:/seed",                      # host path, relative to the worktree
  "/etc/ssl/certs:/certs:ro",          # absolute, read-only
  "~/.cache/npm:/root/.npm",           # home-relative
]
```

A source with no `/` is named storage; anything starting with `/`, `./` or `~/`
is a host path. A `:ro` or `:rw` suffix sets the mode, defaulting to read-write.
The container path must be absolute.

Apple Container has no named volumes, so they become bind mounts under
`~/.minato/volumes/<project>/`.

### Environment

| Key | Type | | |
| --- | --- | --- | --- |
| `env` | table | `{}` | Variables for this service |

```toml
env = { NODE_ENV = "development", PORT = "3000" }
```

This is the *project* layer, and it is committed — keep secrets out of it. See
[Environment variables](../guide/environment-variables).

## Validation

```console
$ minato status
error: invalid configuration: service `web`: depends_on names an unknown service `database`
```

The configuration is checked when it is read. Unknown service references,
circular `depends_on`, malformed volumes and invalid durations are all caught
before anything starts.

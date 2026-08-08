# A web app and a database

Two services, then a third shared across every branch. This is where `scope`
and `depends_on` start to matter.

Follows on from [A preview per branch](./first-preview).

## Two services

```toml
[project]
name = "myapp"

[runtime]
default = "docker"

[services.web]
image = "node:22"
port = 3000
command = "npm run dev"
depends_on = ["api"]

[services.api]
image = "node:22"
port = 8080
command = "npm run api"
health = "http://localhost:8080/healthz"
```

```console
$ minato up
  ✓ starting api
  ✓ waiting for api
  ✓ starting web
  ✓ waiting for web

  web   ready     https://web.myapp.localhost
  api   ready     https://api.myapp.localhost
```

`depends_on` put `api` first. Both got their own URL.

## Letting the frontend find the API

The API's URL is different on every branch, so it cannot be hardcoded. Minato
injects it:

```js
const api = process.env.MINATO_URL_API   // https://api.feature-x.myapp.localhost
```

```console
$ minato exec web -- printenv MINATO_URL_API
https://api.myapp.localhost
```

Every service gets `MINATO_URL_<SERVICE>` for every other service. **This is
the piece that makes per-worktree environments work at all** — without it, the
frontend would have to guess.

For server-to-server calls inside the same workspace you can also use the
service name directly on Docker (`http://api:8080`), which skips the proxy.
That does not work on Apple Container; see [Runtimes](../guide/runtimes).

## Adding a database

```toml
[services.db]
image = "postgres:16"
port = 5432
scope = "project"
expose = false
volumes = ["pgdata:/var/lib/postgresql/data"]
env = { POSTGRES_PASSWORD = "postgres", POSTGRES_DB = "myapp" }

[services.api]
image = "node:22"
port = 8080
command = "npm run api"
depends_on = ["db"]
```

Three decisions in there:

**`scope = "project"`** — one database for every worktree, rather than one
each. You seed it once, and branches see the same data.

**`expose = false`** — no URL, no route. The database is reachable from other
services and from nowhere else. Always set this on a database.

**`volumes`** — named storage so the data survives `down` and `up`. Because it
is named rather than a host path, the runtime manages it and it is scoped to
the project.

```console
$ minato up
  ✓ starting db
  ✓ starting api
  ✓ starting web

  web   ready     https://web.myapp.localhost
  api   ready     https://api.myapp.localhost
  db    ready     (internal only)
```

`(internal only)` is `expose = false` doing its job.

## Confirming the sharing

```console
$ minato new feature/reports
$ cd ../myapp.wt/feature-reports
$ minato status
  web   ready     https://web.feature-reports.myapp.localhost
  api   ready     https://api.feature-reports.myapp.localhost
  db    ready     (internal only)
```

New `web` and `api`; the *same* `db`:

```console
$ docker ps --filter label=dev.minato.project=myapp --format '{{.Names}}'
minato-myapp-feature-reports-web
minato-myapp-feature-reports-api
minato-myapp-main-web
minato-myapp-main-api
minato-myapp-shared-db
```

`minato-myapp-shared-db` — one, not one per worktree. Write a row from one
branch and the other sees it.

## When sharing is wrong

Two branches with incompatible migrations against one database will fight.
Minato does not solve this. When it applies:

```toml
[services.db]
scope = "workspace"   # one each
```

You pay the seeding cost and get independence. Decide per project, and expect
to change your mind on the branch that adds a migration.

## Connecting to it

```toml
[services.api]
env = { DATABASE_URL = "postgres://postgres:postgres@db:5432/myapp" }
```

`db:5432` resolves on Docker. Better, keep the password out of the repository:

```console
$ minato env set DATABASE_PASSWORD='op://Development/myapp/db' --scope project
```

That is a reference, not a value. It is resolved when the container starts and
never written to disk. See
[Environment variables](../guide/environment-variables).

## Idle timeouts with several services

Only requests **through the proxy** count as activity. A database that only the
API talks to looks idle even while the API is busy, and will stop.

```toml
[services.db]
idle_timeout = "8h"
```

It will still be woken by whatever needs it, but this avoids paying the restart
repeatedly during a working day.

## Next

- [Sharing a preview](./sharing) — put a branch on the internet
- [Configuration](../guide/configuration) — the rest of the keys

---
name: kobune
description: Work with the preview environment behind a git worktree. Use it to branch off and check something works, read a service's logs, run tests inside a container, or set environment variables. Active in any repository with a kobune.toml.
---

# Kobune

One worktree, one environment. A worktree appears and its environment appears
with it; the worktree goes and so does the environment. Hold on to that
correspondence and you never lose track of which environment you are looking
at.

## Principles

**Never reach for `docker`.** Anything `docker ps` or `docker logs` shows is
visible through Kobune too. Touching it directly puts the real state at odds
with the state Kobune knows about.

**Never guess a port.** Ask `kobune url <service>`. Ports change from one start
to the next; the URL does not.

**Confirm by actually reaching it.** "It should be up" is not a check.

## Start here

```bash
kobune status --json
```

That gives the current workspace, the state of each service, and the URLs.
With no `kobune.toml`, `kobune init` writes a starter one.

## Configuration

`kobune.toml` sits at the repository root and every worktree reads the same
one. **The full reference is
<https://minato.1024.works/reference/kobune-toml>** — read it before writing
one rather than guessing at key names.

Two more files may be merged over it, later ones winning:
`~/.kobune/config.toml` for what is true of the machine, and
`kobune.local.toml` beside `kobune.toml` for what is true of one clone. Both
are absent on most checkouts. **`kobune config show` says which layer settled
each value** — reach for it before concluding that `kobune.toml` says
something it does not, because the merged result is in no file you can read.

### There is no `kobune.toml` yet

**Look for a compose file first**, before writing anything:

```bash
kobune init --from-compose      # finds compose.yaml, docker-compose.yml, …
kobune init                     # only if there is no compose file
```

It converts what maps, leaves `TODO` comments where compose could not say
what Kobune needs, and names every key it had no equivalent for. **Read the
TODOs before the first `kobune up`** — that is the whole of what it could not
decide for you.

Either form also adds `kobune.local.toml` and `.kobune/env.local` to
`.gitignore`, so there is nothing to do about those by hand.

Deriving the same file by hand from a compose file you can see is slower and
gets the service URLs wrong, which is the one mistake that still starts.

Two services, which is the shape most projects want:

```toml
[project]
name = "myapp"

[services.api]
image = "node:22"
port = 8787
command = "sh -c 'npm run dev:api'"

[services.web]
image = "node:22"
port = 3000
command = "sh -c 'npm run dev:web'"
depends_on = ["api"]     # starts api first, and waits for it to be ready
```

**That example is one shape, not the shape.** It happens to have npm scripts;
plenty of projects are a bare `node server.js` with no `package.json` at all,
and then there is nothing for `setup` to do. Take the keys from it, not the
values.

Worth knowing before you hit them:

- `command` is split shell-style, so `sh -c '...'` works as written
- `depends_on` **waits for the dependency to be ready**, not just to start.
  The wait gives up after 15 seconds and carries on, so a dependency that
  takes longer than that is not a guarantee
- A named volume (`cache:/path`) is per project and **shared by every
  worktree**. Kobune namespaces it, so writing your own project prefix
  gets you `kobune-myapp-myapp-cache`. Use `cache@workspace:/path` for one
  per worktree — `node_modules` against a per-branch lockfile needs it
- Your worktree is mounted at `/workspace`. Anything a build writes under
  it lands in the repository on the host — point caches at
  `/var/cache/kobune`, which every service gets as `KOBUNE_CACHE_DIR` and
  every worktree shares. **`env` values are not interpolated**, so write the
  path out there; `$KOBUNE_CACHE_DIR` only expands in a `command` or a
  start-up script
- `[project] carry = [".env"]` names the untracked files a new worktree
  needs. Without it `kobune new` produces an environment that cannot start
- `setup` runs once before a service first starts — put `pnpm install`
  there, not in `command`, so it does not run on every `down`/`up`. It runs
  again if you change what it says
- `health = "http://localhost:PORT/path"` is what makes `ready` mean
  "serving". Without it, `ready` only means the container is running

## Everyday work

### Start on a new branch

```bash
kobune new feature/user-auth
```

Creates the worktree, brings the environment up, and prints the URLs. There is
no need to run `git worktree add` yourself — though Kobune recognises the
worktree if you do.

Move into the new worktree before working. Its path is the `path` field of
`kobune status --json`.

**A new worktree gets the tracked files and nothing else.** If the services
need an untracked file — `.env` almost always — name it in `[project] carry`
so `kobune new` copies it over. Without that the new environment fails to
start every time, and the logs blame a missing variable rather than a missing
file.

### Check a change

```bash
URL=$(kobune url web)
curl -sS --fail-with-body "$URL/api/health"
```

**`curl -s` on its own is not enough.** It swallows errors, leaving nothing
that looks like anything but an empty response. Use `-sS --fail-with-body`, or
check the exit code.

**Name the service.** `kobune url web` returns one line, something like
`https://web.feature-user-auth.myapp.localhost`. `kobune url` with no name
lists every service instead, which is for reading rather than substituting —
`$(kobune url)` puts a table where a URL was expected.

**That URL works while the service is stopped** — a request wakes the
environment up. It can take a few seconds, but `curl` waits for readiness, so
use it as-is.

**`curl: (60)` does not mean you are stuck.** It means the local CA is not in
the system trust store, and putting it there needs sudo — which is a person's
job. You do not need it. `kobune doctor --json` reports where the certificate
is, and `--cacert` verifies against it properly:

```bash
CA=$(kobune doctor --json | jq -r '.checks[] | select(.id == "ca") | .detail')
curl -sS --fail-with-body --cacert "$CA" "$(kobune url web)/"
```

Use that rather than stopping, and rather than `-k`, which would hide a real
certificate problem alongside this one. Mention to the user that
`kobune setup` would make the plain `curl` work.

### After editing a file

Your worktree is mounted into the container, so the *file* changes the instant
you save it. Whether the **process** notices is up to what `command` starts.

- a dev server with a watcher — `next dev`, `vite`, `nodemon` — picks it up,
  and there is nothing to do
- anything else, `node server.js` included, is still running the old code

```bash
kobune down --service api && kobune up --service api
```

**Check this before you doubt the edit.** A stale process answers exactly like
a change that did not work — a 404 on the route you just added — and the next
move it invites is to go back and rewrite code that was already correct.
`kobune exec api -- grep <something-you-just-wrote> /workspace/api/server.js`
settles which of the two it is in one command.

There is no `kobune restart`.

### Read the logs

```bash
kobune logs                 # every service
kobune logs web -n 50       # the last 50 lines of web
kobune logs web -f          # keep streaming (stop it yourself)
```

A service configured with `tty = true` has a terminal, and following it from
a real terminal hands that terminal over — colour, cursor movement, the lot.
That is for a person, not for reading: **pass `--no-input` when the output is
going to be parsed**, and it stays the plain stream. Nothing to do when you
have no terminal anyway, which is the usual case.

### Run a command in a container

```bash
kobune exec web -- pnpm test
```

**The command's exit code comes straight back**, so tests can be judged by exit
status alone. Output arrives split across stdout and stderr.

`-C /workspace/apps/api` runs it somewhere other than the service's `workdir`.

When a service will not start, `--fresh` is the way in:

```bash
kobune exec --fresh api -- env
kobune exec --fresh api -- sh -c 'pnpm install'
```

No stdin is attached, so `-- sh` alone exits immediately — use `sh -c`.

That runs in a throwaway container built from the same image, environment and
volumes, **without the service's start-up command** — so it works when the
real container has died, which is exactly when you need to look.

### Environment variables

```bash
kobune env ls               # shows which layer each value comes from
kobune env set API_KEY=xxx  # the workspace layer by default: this worktree only
kobune env set DEBUG=1 --scope project   # the whole repository
```

Do not write `.env` directly. There are three layers, and editing one by hand
leaves it unclear which is winning.

**A change needs `kobune down && kobune up`.** Containers that are already
running do not pick it up.

Every service receives the other services' URLs as `KOBUNE_URL_<SERVICE>`
(`KOBUNE_URL_API` for a service named `api`). Use those when the frontend calls
the API — hardcoding breaks from one worktree to the next.

**The same URL works from inside the container**, so server-to-server calls use
it too: the hostnames are pointed at Kobune's gateway in every container of the
workspace. One Host and one Origin for both halves of an app is what keeps
cookies and CORS from having to know about two.

**Do not reach for `NODE_TLS_REJECT_UNAUTHORIZED=0`.** Kobune's CA is mounted
into every service, named as `KOBUNE_CA_FILE`, and already set as
`NODE_EXTRA_CA_CERTS` — a Node service verifies these URLs with nothing added
to `kobune.toml`. Another stack points its own additive variable at
`${KOBUNE_CA_FILE}`; `SSL_CERT_FILE`, `CURL_CA_BUNDLE` and `REQUESTS_CA_BUNDLE`
replace the trust store rather than adding to it, so a container set up through
one of those trusts nothing else.

**If `SELF_SIGNED_CERT_IN_CHAIN` still comes back, something between the
container and the process is filtering the environment.** Turborepo 2's strict
mode is the usual one: it passes through only what `turbo.json` names, so the
variable reaches `turbo` and not the server it starts.

```json
{ "globalPassThroughEnv": ["NODE_EXTRA_CA_CERTS"] }
```

Read `/proc/<pid>/environ` of the process that actually fetches, not of pid 1,
before concluding the variable is missing.

To reach the name the application already reads, refer to it from `env` in
`kobune.toml`. `${NAME}` is expanded; a bare `$NAME` is not.

**Which name that is only exists in the application.** Kobune knows the value
to give — `KOBUNE_URL_API` — and cannot know that this project's web server
reads it as `ROOMS_API`. Grep the source for the variable before writing the
`env` block; a compose file, if there is one, usually names it too.

```toml
[services.web.env]
NEXT_PUBLIC_API_URL     = "${KOBUNE_URL_API}"
NEXT_ALLOWED_DEV_ORIGIN = "${KOBUNE_HOSTNAME_WEB}"
```

`KOBUNE_HOSTNAME_<SERVICE>` is the host with no scheme or port — what a CORS
origin, `allowedDevOrigins` and a cookie domain want. (`KOBUNE_HOST_<SERVICE>`
is a different thing: Apple Container's peer IP.)

For a tool that reads a file rather than its own environment (`wrangler dev`,
Vite, dotenvx), `env_file` writes the settled values into the worktree before
the service starts. Secrets are left out of it.

```toml
[services.api]
env_file = ".kobune/env.api"
```

**They are only there while the proxy is listening.** With no proxy there is
no URL to hand out, so the variable is left unset rather than set to
something that does not work, and a start-up script reading it fails with
`KOBUNE_URL_WEB: parameter not set`. `kobune up` warns when this is the case;
`kobune env ls` shows what is actually injected, and `kobune doctor` says how
to get the proxy up.

### Share it with someone

```bash
kobune tunnel status
```

An environment can be published over Cloudflare Tunnel, and `kobune status`
then shows a second URL per service. **Do not run `kobune tunnel enable`
yourself** — it puts the environment on the public internet, and that is the
user's call to make, not yours. If they ask for a shareable link, tell them the
command and let them run it.

### Clean up

```bash
kobune rm -w feature-user-auth
```

Removes the worktree and its environment. The branch stays.

## When something is wrong

**Work through these in order.** Do not fall back to `docker` on a hunch.

1. `kobune status --json` — look at each service's `state`, which is a
   plain string:

   ```bash
   kobune status --json | jq -r '.workspace.services[] | "\(.name) \(.state)"'
   ```

   - `stopped` → reach for it, or run `kobune up`
   - `starting` → wait. The container is up but its `health` check is not
     answering, which is what a dev server still building looks like
   - `failed` → `reason`, beside the state, says why. A container that
     exited non-zero lands here, so this is what a start-up script that
     died looks like
2. `kobune logs <service>` — errors from the app itself
3. `kobune env ls --service <name>` — what that container is actually given,
   and which layer each value came from. Check here before concluding a
   variable is wrong; without `--service` you get only what every service
   shares. When a `${...}` will not settle the listing still arrives: that
   value is shown as written and carries `unsettled` in `--json`, with the
   name it refers to and why
4. `kobune config show` — where a setting came from, when the behaviour does
   not match what `kobune.toml` says. Two other files can be merged over it,
   and neither is in the repository
5. `kobune doctor` — problems with the environment. **The fix is in `fix`**

### Common symptoms

| Symptom | Where to look |
| --- | --- |
| `curl` exits 60 | The CA is not in the system trust store. Verify with `--cacert` against the path `kobune doctor --json` reports; trusting it needs sudo, so that is the user's to run |
| The URL does not connect | `kobune doctor`. Usually DNS or the proxy is not set up yet |
| A 404 comes back | Wrong hostname. Get it again from `kobune url <service>` |
| A 502 comes back | The service is registered but not answering. `kobune logs` |
| `KOBUNE_URL_*: parameter not set` | The proxy is not listening, so no URL was injected. `kobune doctor` |
| `kobune exec` says the container is not running | It died. `kobune logs` for why, `kobune exec --fresh` to get inside anyway |
| Startup never finishes | Watch it with `kobune logs -f` |
| A config change does nothing | `kobune down && kobune up` |
| A code change does nothing | The process is stale, not the file. `kobune down --service X && kobune up --service X` |

## Reading the output

Every command supports `--json`. Use it whenever you are parsing.

On failure, the exit code says what kind of failure it was.

| Code | Meaning |
| --- | --- |
| 4 | Not found (workspace or service) |
| 5 | Already exists |
| 6 / 7 | No configuration / invalid configuration |
| 8 | Outside a git repository |
| 9 | Cannot reach the container runtime |
| 10 | A runtime operation failed |
| 11 | Unsupported |

A `--json` error may carry a `hint`. **Read it — it says what to do next.**

## Do not

- Run `docker` or `container` directly
- Put a port in a URL (`localhost:3000` and friends)
- Edit `.env` by hand
- Leave `kobune logs -f` running
- Report "it's up" without checking
- Read empty `curl -s` output as an empty response (it may be a certificate
  error)

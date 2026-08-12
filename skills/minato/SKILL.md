---
name: minato
description: Work with the preview environment behind a git worktree. Use it to branch off and check something works, read a service's logs, run tests inside a container, or set environment variables. Active in any repository with a minato.toml.
---

# Minato

One worktree, one environment. A worktree appears and its environment appears
with it; the worktree goes and so does the environment. Hold on to that
correspondence and you never lose track of which environment you are looking
at.

## Principles

**Never reach for `docker`.** Anything `docker ps` or `docker logs` shows is
visible through Minato too. Touching it directly puts the real state at odds
with the state Minato knows about.

**Never guess a port.** Ask `minato url`. Ports change from one start to the
next; the URL does not.

**Confirm by actually reaching it.** "It should be up" is not a check.

## Start here

```bash
minato status --json
```

That gives the current workspace, the state of each service, and the URLs.
With no `minato.toml`, `minato init` writes a starter one.

## Configuration

`minato.toml` sits at the repository root and every worktree reads the same
one. **The full reference is
<https://minato.1024.works/reference/minato-toml>** — read it before writing
one rather than guessing at key names.

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

Worth knowing before you hit them:

- `command` is split shell-style, so `sh -c '...'` works as written
- `depends_on` **waits for the dependency to be ready**, not just to start.
  The wait gives up after 15 seconds and carries on, so a dependency that
  takes longer than that is not a guarantee
- A named volume (`cache:/path`) is per project and **shared by every
  worktree**. Minato namespaces it, so writing your own project prefix
  gets you `minato-myapp-myapp-cache`. Use `cache@workspace:/path` for one
  per worktree — `node_modules` against a per-branch lockfile needs it
- Your worktree is mounted at `/workspace`. Anything a build writes under
  it lands in the repository on the host — point caches at
  `/var/cache/minato`, which every service gets as `MINATO_CACHE_DIR` and
  every worktree shares. **`env` values are not interpolated**, so write the
  path out there; `$MINATO_CACHE_DIR` only expands in a `command` or a
  start-up script
- `[project] carry = [".env"]` names the untracked files a new worktree
  needs. Without it `minato new` produces an environment that cannot start
- `setup` runs once before a service first starts — put `pnpm install`
  there, not in `command`, so it does not run on every `down`/`up`. It runs
  again if you change what it says
- `health = "http://localhost:PORT/path"` is what makes `ready` mean
  "serving". Without it, `ready` only means the container is running

## Everyday work

### Start on a new branch

```bash
minato new feature/user-auth
```

Creates the worktree, brings the environment up, and prints the URLs. There is
no need to run `git worktree add` yourself — though Minato recognises the
worktree if you do.

Move into the new worktree before working. Its path is the `path` field of
`minato status --json`.

**A new worktree gets the tracked files and nothing else.** If the services
need an untracked file — `.env` almost always — name it in `[project] carry`
so `minato new` copies it over. Without that the new environment fails to
start every time, and the logs blame a missing variable rather than a missing
file.

### Check a change

```bash
URL=$(minato url web)
curl -sS --fail-with-body "$URL/api/health"
```

**`curl -s` on its own is not enough.** It swallows errors, leaving nothing
that looks like anything but an empty response. Use `-sS --fail-with-body`, or
check the exit code.

`minato url` returns one line, something like
`https://web.feature-user-auth.myapp.localhost`. **That URL works while the
service is stopped** — a request wakes the environment up. It can take a few
seconds, but `curl` waits for readiness, so use it as-is.

**`curl: (60)` does not mean you are stuck.** It means the local CA is not in
the system trust store, and putting it there needs sudo — which is a person's
job. You do not need it. `minato doctor --json` reports where the certificate
is, and `--cacert` verifies against it properly:

```bash
CA=$(minato doctor --json | jq -r '.checks[] | select(.id == "ca") | .detail')
curl -sS --fail-with-body --cacert "$CA" "$(minato url web)/"
```

Use that rather than stopping, and rather than `-k`, which would hide a real
certificate problem alongside this one. Mention to the user that
`minato setup` would make the plain `curl` work.

### After editing a file

Your worktree is mounted into the container, so the *file* changes the instant
you save it. Whether the **process** notices is up to what `command` starts.

- a dev server with a watcher — `next dev`, `vite`, `nodemon` — picks it up,
  and there is nothing to do
- anything else, `node server.js` included, is still running the old code

```bash
minato down --service api && minato up --service api
```

**Check this before you doubt the edit.** A stale process answers exactly like
a change that did not work — a 404 on the route you just added — and the next
move it invites is to go back and rewrite code that was already correct.
`minato exec api -- grep <something-you-just-wrote> /workspace/api/server.js`
settles which of the two it is in one command.

There is no `minato restart`.

### Read the logs

```bash
minato logs                 # every service
minato logs web -n 50       # the last 50 lines of web
minato logs web -f          # keep streaming (stop it yourself)
```

A service configured with `tty = true` has a terminal, and following it from
a real terminal hands that terminal over — colour, cursor movement, the lot.
That is for a person, not for reading: **pass `--no-input` when the output is
going to be parsed**, and it stays the plain stream. Nothing to do when you
have no terminal anyway, which is the usual case.

### Run a command in a container

```bash
minato exec web -- pnpm test
```

**The command's exit code comes straight back**, so tests can be judged by exit
status alone. Output arrives split across stdout and stderr.

`-C /workspace/apps/api` runs it somewhere other than the service's `workdir`.

When a service will not start, `--fresh` is the way in:

```bash
minato exec --fresh api -- env
minato exec --fresh api -- sh -c 'pnpm install'
```

No stdin is attached, so `-- sh` alone exits immediately — use `sh -c`.

That runs in a throwaway container built from the same image, environment and
volumes, **without the service's start-up command** — so it works when the
real container has died, which is exactly when you need to look.

### Environment variables

```bash
minato env ls               # shows which layer each value comes from
minato env set API_KEY=xxx  # the workspace layer by default: this worktree only
minato env set DEBUG=1 --scope project   # the whole repository
```

Do not write `.env` directly. There are three layers, and editing one by hand
leaves it unclear which is winning.

**A change needs `minato down && minato up`.** Containers that are already
running do not pick it up.

Every service receives the other services' URLs as `MINATO_URL_<SERVICE>`
(`MINATO_URL_API` for a service named `api`). Use those when the frontend calls
the API — hardcoding breaks from one worktree to the next.

**The same URL works from inside the container**, so server-to-server calls use
it too: the hostnames are pointed at Minato's gateway in every container of the
workspace. One Host and one Origin for both halves of an app is what keeps
cookies and CORS from having to know about two.

**Do not reach for `NODE_TLS_REJECT_UNAUTHORIZED=0`.** Minato's CA is mounted
into every service, named as `MINATO_CA_FILE`, and already set as
`NODE_EXTRA_CA_CERTS` — a Node service verifies these URLs with nothing added
to `minato.toml`. Another stack points its own additive variable at
`${MINATO_CA_FILE}`; `SSL_CERT_FILE`, `CURL_CA_BUNDLE` and `REQUESTS_CA_BUNDLE`
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
`minato.toml`. `${NAME}` is expanded; a bare `$NAME` is not.

```toml
[services.web.env]
NEXT_PUBLIC_API_URL     = "${MINATO_URL_API}"
NEXT_ALLOWED_DEV_ORIGIN = "${MINATO_HOSTNAME_WEB}"
```

`MINATO_HOSTNAME_<SERVICE>` is the host with no scheme or port — what a CORS
origin, `allowedDevOrigins` and a cookie domain want. (`MINATO_HOST_<SERVICE>`
is a different thing: Apple Container's peer IP.)

For a tool that reads a file rather than its own environment (`wrangler dev`,
Vite, dotenvx), `env_file` writes the settled values into the worktree before
the service starts. Secrets are left out of it.

```toml
[services.api]
env_file = ".minato/env.api"
```

**They are only there while the proxy is listening.** With no proxy there is
no URL to hand out, so the variable is left unset rather than set to
something that does not work, and a start-up script reading it fails with
`MINATO_URL_WEB: parameter not set`. `minato up` warns when this is the case;
`minato env ls` shows what is actually injected, and `minato doctor` says how
to get the proxy up.

### Share it with someone

```bash
minato tunnel status
```

An environment can be published over Cloudflare Tunnel, and `minato status`
then shows a second URL per service. **Do not run `minato tunnel enable`
yourself** — it puts the environment on the public internet, and that is the
user's call to make, not yours. If they ask for a shareable link, tell them the
command and let them run it.

### Clean up

```bash
minato rm -w feature-user-auth
```

Removes the worktree and its environment. The branch stays.

## When something is wrong

**Work through these in order.** Do not fall back to `docker` on a hunch.

1. `minato status --json` — look at each service's `state`, which is a
   plain string:

   ```bash
   minato status --json | jq -r '.workspace.services[] | "\(.name) \(.state)"'
   ```

   - `stopped` → reach for it, or run `minato up`
   - `starting` → wait. The container is up but its `health` check is not
     answering, which is what a dev server still building looks like
   - `failed` → `reason`, beside the state, says why. A container that
     exited non-zero lands here, so this is what a start-up script that
     died looks like
2. `minato logs <service>` — errors from the app itself
3. `minato env ls --service <name>` — what that container is actually given,
   and which layer each value came from. Check here before concluding a
   variable is wrong; without `--service` you get only what every service
   shares. When a `${...}` will not settle the listing still arrives: that
   value is shown as written and carries `unsettled` in `--json`, with the
   name it refers to and why
4. `minato doctor` — problems with the environment. **The fix is in `fix`**

### Common symptoms

| Symptom | Where to look |
| --- | --- |
| `curl` exits 60 | The CA is not in the system trust store. Verify with `--cacert` against the path `minato doctor --json` reports; trusting it needs sudo, so that is the user's to run |
| The URL does not connect | `minato doctor`. Usually DNS or the proxy is not set up yet |
| A 404 comes back | Wrong hostname. Get it again from `minato url` |
| A 502 comes back | The service is registered but not answering. `minato logs` |
| `MINATO_URL_*: parameter not set` | The proxy is not listening, so no URL was injected. `minato doctor` |
| `minato exec` says the container is not running | It died. `minato logs` for why, `minato exec --fresh` to get inside anyway |
| Startup never finishes | Watch it with `minato logs -f` |
| A config change does nothing | `minato down && minato up` |
| A code change does nothing | The process is stale, not the file. `minato down --service X && minato up --service X` |

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
- Leave `minato logs -f` running
- Report "it's up" without checking
- Read empty `curl -s` output as an empty response (it may be a certificate
  error)

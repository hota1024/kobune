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

### Read the logs

```bash
minato logs                 # every service
minato logs web -n 50       # the last 50 lines of web
minato logs web -f          # keep streaming (stop it yourself)
```

### Run a command in a container

```bash
minato exec web -- pnpm test
```

**The command's exit code comes straight back**, so tests can be judged by exit
status alone. Output arrives split across stdout and stderr.

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

### Clean up

```bash
minato rm -w feature-user-auth
```

Removes the worktree and its environment. The branch stays.

## When something is wrong

**Work through these in order.** Do not fall back to `docker` on a hunch.

1. `minato status --json` — look at each service's `state`
   - `stopped` → reach for it, or run `minato up`
   - `starting` → wait
   - `failed` → `reason` says why
2. `minato logs <service>` — errors from the app itself
3. `minato doctor` — problems with the environment. **The fix is in `fix`**

### Common symptoms

| Symptom | Where to look |
| --- | --- |
| `curl` exits 60 | The certificate is not trusted. `minato doctor` prints the steps to trust the CA (they need sudo, so ask a person) |
| The URL does not connect | `minato doctor`. Usually DNS or the proxy is not set up yet |
| A 404 comes back | Wrong hostname. Get it again from `minato url` |
| A 502 comes back | The service is registered but not answering. `minato logs` |
| Startup never finishes | Watch it with `minato logs -f` |
| A config change does nothing | `minato down && minato up` |

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

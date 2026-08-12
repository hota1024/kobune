# Driving Minato as an agent would

The README's first line calls this a development environment manager that is
agent-friendly by design. `skills/minato/SKILL.md` is written for one. Nothing
in this repository had ever checked whether an agent can get through a task
with it.

This is the record of doing that on 2026-08-12, against `0bd57cf`. It is kept
so it can be repeated rather than remembered.

## The rules

- a real two-service project, not a fixture: a web that fetches from an api
  and renders what it gets
- everything through `minato`. **`docker` only to observe**, never to act
- `SKILL.md` as the only instructions, followed as written
- every place it did not say enough gets written down, including the ones that
  are nobody's fault

## The project

```
agentproj/
  api/server.js     # /healthz, /todos
  web/server.js     # fetches ${API_URL}/todos and renders it
  .env              # untracked, as .env always is
```

```toml
[project]
name = "agentproj"
carry = [".env"]

[services.api]
image = "node:22"
port = 8787
command = "sh -c 'node api/server.js'"
health = "http://localhost:8787/healthz"

[services.web]
image = "node:22"
port = 3000
command = "sh -c 'node web/server.js'"
depends_on = ["api"]
health = "http://localhost:3000/healthz"
env = { API_URL = "${MINATO_URL_API}" }
```

Written from `SKILL.md`'s two-service template plus its notes on `carry`,
`health` and `${MINATO_URL_...}`. It was right first time, which is worth
saying: the format did not need guessing at.

## What worked

**Getting started.** `minato status --json` in a project with no
configuration answers with `config_not_found`, exit 6, and a hint naming
`minato init`. `minato init` writes a starter file. Nothing to work out.

**Bringing it up.** `minato up` pulled, started both services in dependency
order, waited for both health checks and printed two URLs. First time.

**The service-to-service URL.** `web` reached `api` through
`MINATO_URL_API` — the same hostname from inside the container as from
outside — and rendered its data:

```console
$ curl --cacert "$CA" https://web.agentproj.localhost:19443/
<h1>Todos</h1><ul><li>write the review</li></ul>
```

**A worktree, which is the whole point.**

```console
$ minato new feature/counts
  ✓ starting api
  ✓ starting web
● api  ready  https://api.feature-counts.agentproj.localhost:19443
● web  ready  https://web.feature-counts.agentproj.localhost:19443
```

The untracked `.env` came with it, because `carry` names it. Both worktrees
answered independently, on their own hostnames, from their own containers.
`status --json` carries the new worktree's path, so an agent knows where to
`cd`.

**Scale-to-zero.** With both services stopped, one request brought back the
service *and* the `depends_on` behind it, in **0.7 seconds**. That is #49's
fix in the real flow rather than in a test.

## Where it tripped

### 1. An agent is told to stop when it does not have to

`SKILL.md` says to confirm by actually reaching the URL. Doing that on a
machine where `minato setup` has not run:

```console
$ curl -sS --fail-with-body "$(minato url web)/"
curl: (60) SSL certificate problem: self signed certificate in certificate chain
```

The trail from there works exactly as designed — `SKILL.md` names exit 60,
points at `minato doctor`, and `doctor` says `local CA trust: not trusted`
with the `sudo` command to fix it. And then `SKILL.md` says trusting it is a
person's job, so ask.

**But the agent does not need the trust store.** `doctor --json` already
reports where the certificate is:

```console
$ CA=$(minato doctor --json | jq -r '.checks[] | select(.id == "ca") | .detail')
$ curl -sS --fail-with-body --cacert "$CA" "$(minato url web)/"
<h1>Todos</h1><ul><li>write the review</li></ul>
```

Everything needed was already there, in a command `SKILL.md` sends the agent
to. It blocked at the one step it calls the most important.

Fixed: `SKILL.md` now shows the `--cacert` form, and says to mention `minato
setup` to the user rather than waiting on it.

### 2. Nothing said what to do after editing source

Adding a route to `api/server.js` and checking it:

```console
$ curl --cacert "$CA" "$(minato url api)/todos/count"
not found
```

The only "a change needs" advice in `SKILL.md` was about environment
variables. A 404 on a route you just wrote reads as a mistake in the edit, and
the next move it invites is to rewrite code that was already correct.

The file was in the container all along — the worktree is bind-mounted:

```console
$ minato exec api -- grep -c "todos/count" /workspace/api/server.js
1
```

The *process* was stale. `node server.js` has no watcher.

Fixed: a section on what changes when you save a file and what does not, the
one-command way to tell a stale process from a bad edit, and the recovery.

### 3. `minato restart` was documented and does not exist

The obvious recovery for a stale process. `docs/DESIGN.md` §10 listed it; the
CLI has no such command, and `--help` does not mention it. What works is
`minato down --service api && minato up --service api`.

Fixed: removed from `DESIGN.md`, and `SKILL.md` says outright that there is
no `minato restart`, because an agent that has seen the design will try it.

### 4. `--json` does not match its own documented example

`SKILL.md` opens with `minato status --json` and says to look at each
service's `state`. The obvious way to do that:

```console
$ minato status --json | jq -r '.workspace.services[] | "\(.name): \(.state)"'
api: {"state":"ready"}
web: {"state":"ready"}
```

`state` is an object. `docs/DESIGN.md` §10 documented it as `"state": "ready"`.
An agent comparing `.state` to `"ready"` gets nothing, on the first command it
is told to run.

`ServiceState` is an internally tagged enum, which is why: it lets `failed`
carry its `reason` in the same place. That is a reasonable Rust shape and an
awkward JSON one.

Fixed here: the documented example, and `SKILL.md` now says to read
`.state.state` and where `reason` lives.

**Since taken.** `state` is now a plain string with `reason` beside it, at
`PROTOCOL_VERSION` 6:

```console
$ minato status --json | jq -r '.workspace.services[] | "\(.name): \(.state)"'
web: ready
broken: failed
```

It was left out of this run's own pull request on purpose — a protocol change
made quietly inside a validation task is the wrong way round — and taken as
its own.

## What this run was not

I followed `SKILL.md` rather than my knowledge of the code, and the findings
were real. But I wrote much of the surrounding system, so it found **what the
instructions fail to say** — not what a model with no context would misread.

## The independent run

Done on 2026-08-12 against `00cfa31`: an agent with no knowledge of this
codebase, a project it had never seen — a booking page and the rooms service
behind it — and nothing but the Skill. Its only configuration was a
`docker-compose.yml`.

**It finished the task and never once reached for `docker`.** The `--cacert`
recipe added above worked first try for someone who did not write it, and the
prediction of `curl: (60)` reproduced verbatim. `${MINATO_URL_API}`,
`depends_on`, `health` — all used correctly, from the Skill alone.

It found two things I could not have.

### It never used `--from-compose`, in the one project that needed it

It read the compose file for the port numbers, then **hand-wrote `minato.toml`
from the reference documentation**, deriving ports, commands and health paths
itself. `--from-compose` had shipped four commits earlier.

It is documented in `DESIGN.md`, in both CLI references and in the changelog,
and mentioned **zero times in `SKILL.md`** — the only file an agent reads. In
its own words:

> The Skill is silent on the "no `minato.toml` exists, and there's a stray
> `docker-compose.yml` instead" scenario, which is exactly what I hit.

A feature built to remove the entry barrier, invisible to the reader standing
at it. **This is the one an author cannot find**: I knew the feature existed,
so I never noticed that the instructions did not.

### The converter carried a compose-ism it had a better answer for

Converting the same file gave:

```toml
ROOMS_API = "http://api:8080"
```

Faithful, and wrong here: it bypasses the proxy, hands the application a
different URL from the browser's, and does not resolve at all under Apple
Container. **The agent, writing by hand from the Skill, got this right. The
converter meant to save that work got it wrong.**

Both are fixed. The same project now converts and starts with no hand-editing
at all.

### And two smaller ones

- the Skill's worked example assumes npm scripts; this project was a bare
  `node main.js` with no `package.json`, and the agent had to notice rather
  than copy
- `MINATO_URL_<SERVICE>` is the value, but which variable *name* the
  application reads exists only in its source. The Skill said the first and
  not the second

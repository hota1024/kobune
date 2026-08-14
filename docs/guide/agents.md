# Working with AI agents

Minato is built to be driven by an agent. This page is about what that means in
practice, and how to set it up.

## Install the Skill

```console
$ minato skill install
╭ skill ───────────────────────────────────────────────────╮
│ installed  /path/to/myapp/.claude/skills/minato/SKILL.md │
╰──────────────────────────────────────────────────────────╯
```

That writes a Skill file Claude Code picks up automatically. Commit it — every
worktree and every teammate then gets the same instructions.

```console
$ minato skill show     # print it without writing
$ minato skill install --force   # overwrite local edits
```

Re-running with unchanged content does nothing, so it will not dirty your
repository.

## What the Skill says

It is not a command reference — `--help` covers that. It carries the judgements
an agent cannot infer:

- **Never reach for `docker`.** Anything `docker ps` shows is visible through
  Minato, and touching containers directly puts the real state at odds with
  what Minato believes.
- **Never guess a port.** Ask `minato url`. Ports change; URLs do not.
- **Confirm by actually reaching it.** "It should be up" is not a check.
- **`curl -s` alone is not enough** — it swallows errors and an untrusted
  certificate looks identical to an empty response.
- **Do not enable the tunnel.** Publishing to the internet is the user's call.

## Why the design looks the way it does

Three decisions exist because an agent, not a person, is reading the output.

### Every command speaks JSON

```console
$ minato status --json
{
  "result": "workspace",
  "workspace": {
    "project": "myapp",
    "services": [
      { "name": "web", "state": "ready",
        "url": "https://web.feature-auth.myapp.localhost" }
    ]
  }
}
```

Nothing has to be parsed out of human-readable text, and `state` is a string
rather than an object, so `.state == "ready"` is a comparison that works. A
`failed` service carries a `reason` beside its state.

Without `--json` there is still nothing to strip. On a terminal the CLI draws
its results — a frame, aligned columns, colour on the parts that carry meaning
— but an agent is never on one. Captured output is plain text, with no escape
sequences, no box-drawing characters, and nothing wrapped or truncated however
long a URL is. `minato url <service>` and `minato env get` print one bare line
either way, and `logs` and `exec` pass the container's output through
untouched.

### Exit codes say what went wrong

```console
$ minato url nope; echo $?
4
```

| Code | Meaning |
| --- | --- |
| 4 | Not found |
| 5 | Already exists |
| 6 / 7 | No configuration / invalid configuration |
| 8 | Outside a git repository |
| 9 | Cannot reach the container runtime |
| 10 | A runtime operation failed |
| 11 | Unsupported |

An agent can branch on these without reading anything. The full list is in the
[exit code reference](../reference/exit-codes).

### `exec` passes the exit code through

```console
$ minato exec web -- npm test; echo $?
1
```

Test success is readable from the exit status alone, which is the whole point.

### Errors carry a hint

```console
$ minato tunnel enable --domain example.com --json
{
  "error": {
    "code": "unsupported",
    "message": "a tunnel exposes this environment to the internet",
    "hint": "put a Cloudflare Access policy in front of the hostname, then re-run with --public"
  }
}
```

`hint` says what to do next, not just what went wrong.

## Waiting, rather than failing

A stopped environment starting up takes a few seconds. What happens next
depends on who asked:

- **A browser** gets a page that says "starting" and reloads itself.
- **Everything else** — curl, fetch, an agent — is held until the service is
  ready, up to 120 seconds.

The second case is deliberate. Returning 503 while something starts reads to an
agent as "the server is broken", and it will go and change code that was never
wrong.

## A workable loop

```bash
minato status --json                       # where are we?
minato new feature/x                       # branch and environment together
cd ../myapp.wt/feature-x
# … edit …
minato exec web -- npm test                # exit code is the test result
curl -sS --fail-with-body "$(minato url web)/api/health"
minato logs web -n 50                      # when that fails
minato doctor                              # when the environment is at fault
```

## Not an MCP server

There is not one, and that is intentional. With `--json` on every command, Bash
is enough, and a second surface would be another thing to keep correct.

## Does it actually work?

Fair question to ask of anything that calls itself agent-friendly.
[`docs/AGENT-RUN.md`](https://github.com/hota1024/minato/blob/main/docs/AGENT-RUN.md)
is the record of driving a two-service project through a real task with
nothing but the Skill — what worked first time, and the four places the
instructions did not say enough. The four have been fixed; the record is kept
so the next run can be compared against it.

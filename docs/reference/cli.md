# CLI commands

Every command accepts `--json` and `-w, --workspace`.

| Flag | |
| --- | --- |
| `--json` | Print the response as JSON. Errors go to stdout too, so an agent watches one stream |
| `-w, --workspace <name>` | Which workspace to act on. Inferred from the current directory when omitted |

## What the output looks like

On a terminal, results are drawn: a framed panel, columns that line up, and
colour on the parts that carry meaning — a service's state, a URL, a command
you are being asked to run. Long-running commands hold the bottom line for
what is happening now, and let finished steps scroll up above it.

Piped, redirected or captured, the same views print as plain text: no frame,
no colour, no cursor movement, and nothing wrapped or truncated however long a
URL is. So `minato status | grep web` reads the same as it always did.

| | |
| --- | --- |
| `--json` | Never decorated, whatever it is printing to |
| `NO_COLOR` | Set to anything: keeps the layout, drops the colour |
| `TERM=dumb` | Treated as a pipe throughout |
| `minato url`, `minato env get` | One bare line, always. They exist to be substituted into other commands |
| `minato logs`, `minato exec` | Passed through verbatim, stdout and stderr kept apart |

## Setting up

### `minato init`

Writes a starter `minato.toml` at the repository root, guessing the project
name from the directory. Run from inside a worktree it still writes to the main
one.

```console
$ minato init
$ minato init --force    # overwrite an existing file
```

### `minato doctor`

Diagnoses the environment and prints a fix for anything that is not `✓`. It
checks the runtime your project uses, the proxy and DNS listeners, launchd
socket activation, the local CA and whether it is trusted, `/etc/resolver`, and
whether a name actually resolves to 127.0.0.1.

### `minato setup`

Walks through the parts that need root: the LaunchDaemon, the resolver entry,
and trusting the CA. Each step's commands are printed and then offered, one at
a time, and only what you say yes to is run — **nothing runs unasked.** With no
terminal to answer at — an agent, a pipe, `--json` — the commands are printed
for you to run yourself and none of them are run.

```console
$ minato setup
$ minato setup --yes       # every step, without asking
$ minato setup --dry-run   # print the commands, run none of them
```

| Flag | |
| --- | --- |
| `-y`, `--yes` | Run every step without asking |
| `--dry-run` | Print the commands and run none of them |

The steps are generated for the state *after* setup — installing launchd moves
DNS to :53, so that is the port the resolver gets. Say no to launchd and the
resolver step is rewritten for the port DNS is actually on, so declining one
step cannot break the next.

**A LaunchDaemon launchd already has is not installed again.** The step becomes
waking its job instead, since launchd answers a second `bootstrap` of a label it
knows with `Input/output error` — installing again could only fail. A plist
sitting on disk that was never bootstrapped is still an installation, and gets
one.

Anything declined, or anything whose command failed, is printed at the end to
run by hand. A failed step is also a non-zero exit code; a declined one is not.

## Workspaces

### `minato new <branch>`

Creates a worktree, starts its environment, prints the URLs.

```console
$ minato new feature/user-auth
$ minato new hotfix/x --base v1.2.0
$ minato new feature/x --path ../elsewhere
$ minato new feature/x --no-start
```

| Flag | |
| --- | --- |
| `--base <ref>` | What to branch from, for a new branch |
| `--path <dir>` | Where to put the worktree. Default `../{repo}.wt/{branch}` |
| `--no-start` | Create it without starting anything |
| `--build` | Rebuild images even when nothing has changed |

An existing branch is checked out rather than created.

### `minato ls`

Every workspace, with how many of its services are running.

```console
$ minato ls
$ minato ls --all-projects   # every project this daemon knows about
```

With `--all-projects` a `PROJECT` column appears. Other projects contribute
their **registered** worktrees only — finding unregistered ones would mean
opening someone else's repository — so a project you have never run a command
in shows fewer rows than it would from inside.

### `minato status`

The current workspace in detail: each service's state, URL, and the address the
proxy forwards to.

| State | Meaning |
| --- | --- |
| `stopped` | No container, or one that was stopped. Reaching for the URL starts it |
| `starting` | The container is up, but its `health` check is not answering yet |
| `ready` | Serving |
| `failed` | It fell over. `reason` says what happened |

::: tip `ready` is only checked when `health` is an HTTP check
A container being up and the app inside being able to serve are two different
things. With `health = "http://..."` set, the check is run before a service is
called ready, so a dev server still compiling reads as `starting` — the answer
worth waiting on.

Without it, `ready` means "the container is running", which is all that can be
known from outside. A connection attempt would not add anything: Docker
publishes a port by putting a forwarder in front of it, and that forwarder
accepts whether or not anything inside is listening. **If you want `ready` to
mean served, set [`health`](./minato-toml#readiness).**
:::

### `minato rm`

Removes the worktree and its containers. The branch stays, and a shared
`scope = "project"` service stays because other worktrees use it.

```console
$ minato rm -w feature-auth
$ minato rm -w feature-auth -f   # even with uncommitted changes
```

## Services

### `minato up [services…]`

Starts services, and whatever they depend on. Everything when none are named.

| Flag | |
| --- | --- |
| `--build` | Rebuild images even when nothing Minato can see has changed |

A running container is left alone unless its image has changed. A stopped one
is recreated so configuration changes take effect.

`--build` is for a change the fingerprint cannot see, such as a file the
Dockerfile copies in.

### `minato down [services…]`

```console
$ minato down
$ minato down web
$ minato down --all    # every workspace in the project
```

A shared service only stops when you name it, because other worktrees may be
using it.

### `minato url [service]`

Prints one line and nothing else. The first reachable service when none is
named.

```console
$ curl -sS --fail-with-body "$(minato url web)/api/health"
```

The URL works while the service is stopped — a request starts it.

### `minato logs [services…]`

```console
$ minato logs
$ minato logs web -n 100
$ minato logs web -f
```

| Flag | |
| --- | --- |
| `-f, --follow` | Keep streaming |
| `-n, --tail <n>` | Lines from the end |

Undecorated, with stdout and stderr kept separate.

### `minato exec <service> -- <command>`

```console
$ minato exec web -- npm test
$ minato exec web -- sh
$ minato exec -C /workspace/apps/api api -- pnpm test   # somewhere else
```

**The exit code is the command's own.** No TTY is requested, so anything
waiting for input hangs rather than prompts.

`-C` sets the working directory, defaulting to the service's
[`workdir`](./minato-toml#image-and-command). It is `-C` rather than `-w`
because `-w` already selects the workspace.

#### `--fresh`

```console
$ minato exec --fresh api -- env
$ minato exec --fresh api -- cat /workspace/.env
$ minato exec --fresh api -- sh -c 'pnpm install --frozen-lockfile'
```

No stdin is attached, so `-- sh` on its own reads end-of-file and exits at
once. Pass what you want run with `sh -c`.

Runs the command in a container made for it and removed afterwards, built from
the service's image, environment and volumes but **without the service's
start-up command**.

**The service does not have to be running**, which is the point: a start-up
script that fails leaves nothing to exec into, and that is when you most want
to look around.

It publishes no ports and carries no Minato labels, so it cannot take the real
container's ports, appear in `minato status`, or answer to the service's name
on the network. The image is pulled or built first if it is not there yet, so
this works before a service has ever come up cleanly.

## Interrupting

Ctrl-C asks the daemon to stop and waits for its reply, rather than killing the
CLI where it stands. The exit code is 130.

Work already done is not undone: a cancelled `up` can leave a container
running, which `minato status` shows and `minato down` clears.

`minato logs -f` is the exception — Ctrl-C is simply how you leave it.

## Environment variables

```console
$ minato env ls [--reveal] [--service <name>]
$ minato env get <KEY>
$ minato env set <KEY=VALUE> [--scope global|project|workspace]
$ minato env unset <KEY> [--scope …]
```

`ls` shows which layer each value came from and masks secrets. `--reveal` shows
plain values but still leaves secret *references* as references. `get` prints
one value for piping.

`--service` shows what one container is given, its own
[`env`](./minato-toml#environment) included, so
`minato env ls --service api` answers "is `MINATO_URL_WEB` actually reaching
`api`?" without starting anything. Without it, only what every service shares:
a service's own `env` belongs to that service, and there is no
`MINATO_SERVICE` because no service is being named. `get` takes it too.

The layer column names five of them, innermost winning: `injected`, `global`,
`project`, `service`, `workspace`. `service` is a service's own `env` in
`minato.toml` — it has its own name because reading it as `project` would
send you to edit `.minato/env` for a value the service overrides.

`--scope` defaults to `workspace`.

## The tunnel

```console
$ minato tunnel enable --domain example.com --public
$ minato tunnel disable
$ minato tunnel status
```

`--public` is required, and acknowledges that the environment goes on the
internet with no Access policy Minato can verify. The domain is remembered
after the first time.

## Agents

```console
$ minato skill install [--force]
$ minato skill show
```

Writes `.claude/skills/minato/SKILL.md`. Unchanged content is left alone.

## The daemon

```console
$ minato daemon start
$ minato daemon stop
$ minato daemon status
```

Any command starts the daemon if it is down, so these are rarely needed.
`stop` on a machine with the LaunchDaemon installed is immediately followed by
launchd starting it again — that is how it picks up new settings while keeping
ports 80 and 443.

## Keeping it current

```console
$ minato update
$ minato update --check
```

Replaces both binaries in the directory the running `minato` came from with the
current `nightly`. `--check` reports and installs nothing. Under `--json`:

```json
{ "status": "available", "commit": "…", "running": "…" }
```

`status` is one of `current`, `available`, `installed` or `unknown` — `unknown`
meaning this build records no commit, so there is nothing to compare.

A check runs by itself once a day after any command and prints one line to
stderr. `MINATO_NO_UPDATE_CHECK` turns it off, and `--json` never includes it.

`minato --version` checks too, every time rather than once a day, and after
the version line rather than before it:

```console
$ minato --version
minato 0.1.0 (c7282b8)
› a newer build is available (9f3c1a2). Install it with minato update
```

The line is stderr, there is none when the running build is the published one,
and `--json` and `MINATO_NO_UPDATE_CHECK` skip the check as they do the daily
one.

## Taking it off again

```console
$ minato uninstall
╭ uninstall ─────────────────────────────────────────────────────────────────╮
│ containers:                                                                │
│ myapp / main               web                                             │
│ myapp / main               db                                              │
│ myapp / feature-user-auth  web                                             │
│                                                                            │
│ files:                                                                     │
│ state, logs and the local CA  /home/u/.minato                              │
│ shell completions             /home/u/.config/fish/completions/minato.fish │
│ the binary                    /home/u/.local/bin/minato                    │
│ the binary                    /home/u/.local/bin/minatod                   │
│                                                                            │
│ needs root:                                                                │
│   stop the LaunchDaemon holding 80/443/53                                  │
│     sudo launchctl bootout system/dev.minato.daemon                        │
│     sudo rm /Library/LaunchDaemons/dev.minato.daemon.plist                 │
│   stop trusting the local CA                                               │
│     sudo security remove-trusted-cert -d ~/.minato/ca/minato-ca.crt        │
│                                                                            │
│ left alone — 2 worktrees:                                                  │
│   /path/to/myapp                                                           │
│   /path/to/myapp.wt/feature-user-auth                                      │
╰────────────────────────────────────────────────────────────────────────────╯
Remove all of this? [y/N]
```

| Flag | |
| --- | --- |
| `-y, --yes` | Go ahead without asking. Required where there is no terminal |
| `--dry-run` | Print the list and remove nothing |

**Worktrees are never touched.** They are your checkouts, with your
uncommitted work in them; `minato rm` removes one at a time, and asks for
`--force` when git objects. They are listed so you can see what is being left
behind.

Nothing is listed that is not there, so the list is what is actually on the
machine rather than everywhere Minato might have put something. The binaries
are left alone when they are `cargo build` output — running `uninstall` from a
checkout removes the installation, not your build.

The steps that need root are run with `sudo`, which asks for your password.
Without a terminal to type into — an agent, a pipe, CI — they are printed
instead, the same as `minato setup` does, and the rest of the uninstall still
happens.

## Completions

```console
$ minato completions <bash|zsh|fish|elvish|powershell>
```

Writes the script to stdout. See
[Installation](../guide/installation#shell-completions) for where each shell
expects it; the install script does this already.

## Environment variables that configure Minato

| | |
| --- | --- |
| `MINATO_HOME` | Where state, logs, the socket and the CA live. Default `~/.minato` |
| `MINATO_HTTP_PORT` | Proxy HTTP port. Default 80, falling back to 18080. A port named here is used as given |
| `MINATO_HTTPS_PORT` | Proxy HTTPS port. Default 443, falling back to 18443. A port named here is used as given |
| `MINATO_DNS_PORT` | DNS port. Default 53 |
| `MINATO_CLOUDFLARED` | A `cloudflared` binary somewhere other than `PATH` |
| `MINATO_LOG` | Log filter for the daemon, e.g. `debug` |
| `MINATO_NO_UPDATE_CHECK` | Set to anything to stop the update check, both the daily one and `--version`'s |

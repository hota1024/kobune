# CLI commands

Every command accepts `--json` and `-w, --workspace`.

| Flag | |
| --- | --- |
| `--json` | Print the response as JSON. Errors go to stdout too, so an agent watches one stream |
| `-w, --workspace <name>` | Which workspace to act on. Inferred from the current directory when omitted |

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

Prints the commands for the parts that need root: the LaunchDaemon, the
resolver entry, and trusting the CA. **It never runs them.** The steps are
generated for the state *after* setup — installing launchd moves DNS to :53, so
that is the port the resolver gets.

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
```

**The exit code is the command's own.** No TTY is requested, so anything
waiting for input hangs rather than prompts.

## Interrupting

Ctrl-C asks the daemon to stop and waits for its reply, rather than killing the
CLI where it stands. The exit code is 130.

Work already done is not undone: a cancelled `up` can leave a container
running, which `minato status` shows and `minato down` clears.

`minato logs -f` is the exception — Ctrl-C is simply how you leave it.

## Environment variables

```console
$ minato env ls [--reveal]
$ minato env get <KEY>
$ minato env set <KEY=VALUE> [--scope global|project|workspace]
$ minato env unset <KEY> [--scope …]
```

`ls` shows which layer each value came from and masks secrets. `--reveal` shows
plain values but still leaves secret *references* as references. `get` prints
one value for piping.

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

## Environment variables that configure Minato

| | |
| --- | --- |
| `MINATO_HOME` | Where state, logs, the socket and the CA live. Default `~/.minato` |
| `MINATO_HTTP_PORT` | Proxy HTTP port. Default 80 |
| `MINATO_HTTPS_PORT` | Proxy HTTPS port. Default 443 |
| `MINATO_DNS_PORT` | DNS port. Default 53 |
| `MINATO_CLOUDFLARED` | A `cloudflared` binary somewhere other than `PATH` |
| `MINATO_LOG` | Log filter for the daemon, e.g. `debug` |

# Everyday workflow

The commands you actually use, in the order you tend to use them.

## Where a command acts

Almost every command needs to know which workspace you mean. There are two ways
it finds out:

1. **The directory you are in.** Inside a worktree, that worktree is the target.
2. **`-w, --workspace`.** Names one explicitly, from anywhere in the repository.

```console
$ cd ../myapp.wt/feature-auth && minato status   # this worktree
$ minato status -w feature-auth                  # the same, from anywhere
```

The workspace name is the sanitised branch name: `feature/user-auth` becomes
`feature-user-auth`. `minato ls` shows both.

## Starting work on something

```console
$ minato new feature/user-auth
```

Creates the worktree, starts the environment, prints the URLs. Options:

```console
$ minato new hotfix/login --base v1.2.0   # branch from somewhere specific
$ minato new feature/x --path ../elsewhere
$ minato new feature/x --no-start         # worktree only
```

::: warning A new worktree has only the tracked files
`git worktree add` brings what git knows about and nothing else, so an
untracked `.env` is not there and the services fail to start. Name those files
and Minato brings them over:

```toml
[project]
carry = [".env"]
```

Existing files are never replaced, and a missing one is reported rather than
fatal. See [`carry`](../reference/minato-toml#carry).
:::

If the branch already exists, it is checked out rather than created.

A worktree made with plain `git worktree add` is picked up too — Minato
registers it the first time you run a command in it, so you are never told you
made your worktree the wrong way.

## Seeing what is going on

```console
$ minato ls        # every workspace, and how many services are up
$ minato status    # this workspace in detail: state, URLs, addresses
```

A service is in one of four states:

| State | Meaning |
| --- | --- |
| `ready` | Running and answering |
| `starting` | Container up, not answering yet |
| `stopped` | Not running. A request will start it |
| `failed` | Tried and failed. `reason` says why |

`stopped` is not a problem. It is what an environment nobody is using should
look like.

## Getting a URL

```console
$ minato url          # every service, and the way in
$ minato url web      # a specific one
```

Naming a service prints one line and nothing else, so it substitutes
cleanly:

```console
$ curl -sS --fail-with-body "$(minato url web)/api/health"
```

**Ask for the URL rather than writing one.** It survives restarts; the port
underneath does not.

To open one on a phone, `--qr` draws it as a code to photograph. That wants
a URL a phone can resolve, so it uses the [tunnel](/guide/tunnel) URL when
there is one:

```console
$ minato url web --qr
```

## Starting and stopping

```console
$ minato up               # everything in this workspace
$ minato up web api       # only these, and whatever they depend on
$ minato down             # stop this workspace
$ minato down --all       # every workspace in the project
```

`up` leaves a running container alone, so running it twice is harmless. A
*stopped* container is deleted and recreated, so a configuration change takes
effect — that costs a few seconds, which is cheaper than wondering why an edit
did nothing.

You mostly do not need `up`. A request to a stopped service starts it.

## Logs

```console
$ minato logs                  # every service in this workspace
$ minato logs web              # one
$ minato logs web -n 100       # the last 100 lines
$ minato logs web -f           # follow
```

Output is undecorated, so it greps and pipes. stdout and stderr stay separate.

With several services, lines are interleaved and each is tagged with the
service it came from.

### Interactive services

A service that runs something you would normally interact with — turborepo's
task switcher, a test runner in watch mode — needs a terminal to draw on and
a keyboard to answer. Give it one:

```toml
[services.dev]
image = "node:24-bookworm-slim"
command = "npx turbo run dev"
tty = true
```

Then following that one service hands it this terminal, colour, keys and
all:

```console
$ minato logs -f dev
```

Ctrl-P Ctrl-Q gives the terminal back and leaves the service running.
Everything else, Ctrl-C included, goes to the program. The details, and how
to turn it off, are in [the CLI reference](../reference/cli#typing-at-a-service).

## Running things inside a container

```console
$ minato exec web -- npm test
$ minato exec web -- sh
```

**The exit code is the command's own**, so this works:

```console
$ minato exec web -- npm test && echo "passed"
```

No TTY is requested. A command that waits for input will hang rather than
prompt, so pass whatever `--yes` flag it needs.

## Environment variables

```console
$ minato env ls                          # with the layer each came from
$ minato env get DATABASE_URL            # one value, for piping
$ minato env set API_KEY=xxx             # this worktree
$ minato env set LOG_LEVEL=debug --scope project
$ minato env unset API_KEY
```

A change does not reach a running container. `minato down && minato up` picks
it up, and the CLI reminds you.

See [Environment variables](./environment-variables).

## Finishing up

```console
$ minato rm -w feature-user-auth        # worktree and containers
$ minato rm -w feature-user-auth -f     # even with uncommitted changes
```

The branch stays. A shared `scope = "project"` service stays too, because other
worktrees are using it.

## The daemon

```console
$ minato daemon status
$ minato daemon start
$ minato daemon stop
$ minato daemon restart
```

You rarely touch these. Any command starts the daemon if it is down. Stopping
it stops the proxy and DNS, so URLs stop resolving until it comes back — but
containers keep running.

If launchd is installed, launchd goes on holding ports 80, 443 and 53 while the
job is idle, and the next request starts it again. That is how the daemon picks
up new settings without the ports ever changing hands. To have it back without
waiting for a request — after an update, say — `minato daemon restart` starts it
through launchd too.

## When something is wrong

Work through it in this order rather than reaching for `docker`:

```console
$ minato status      # what state is it in?
$ minato logs web    # what does the app say?
$ minato doctor      # what does the environment say?
```

See [Troubleshooting](./troubleshooting).

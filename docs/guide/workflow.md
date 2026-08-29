# Everyday workflow

The commands you actually use, in the order you tend to use them.

## Where a command acts

Almost every command needs to know which workspace you mean. There are two ways
it finds out:

1. **The directory you are in.** Inside a worktree, that worktree is the target.
2. **`-w, --workspace`.** Names one explicitly, from anywhere in the repository.

```console
$ cd ../myapp.wt/feature-auth && kobune status   # this worktree
$ kobune status -w feature-auth                  # the same, from anywhere
```

The workspace name is the sanitised branch name: `feature/user-auth` becomes
`feature-user-auth`. `kobune ls` shows both.

Getting into one is a command of its own, and the name can be as much of it as
tells it apart from the others:

```console
$ kobune cd feature-auth   # this shell, into that worktree
$ kobune cd fuauth         # the same one
```

It needs the shell function from [`kobune
shell-init`](../reference/cli#shell-integration), which is one line in your
startup file: a program cannot change the directory of the shell that started
it, so without the function this prints the path instead of moving.

## Starting work on something

```console
$ kobune new feature/user-auth
```

Creates the worktree, starts the environment, prints the URLs. Options:

```console
$ kobune new hotfix/login --base v1.2.0   # branch from somewhere specific
$ kobune new feature/x --path ../elsewhere
$ kobune new feature/x --no-start         # worktree only
```

::: warning A new worktree has only the tracked files
`git worktree add` brings what git knows about and nothing else, so an
untracked `.env` is not there and the services fail to start. Name those files
and Kobune brings them over:

```toml
[project]
carry = [".env"]
```

Existing files are never replaced, and a missing one is reported rather than
fatal. See [`carry`](../reference/kobune-toml#carry).
:::

If the branch already exists, it is checked out rather than created.

A worktree made with plain `git worktree add` is picked up too — Kobune
registers it the first time you run a command in it, so you are never told you
made your worktree the wrong way.

## Seeing what is going on

```console
$ kobune           # the dashboard: all of it at once, and it keeps up
$ kobune ls        # every workspace, and how many services are up
$ kobune status    # this workspace in detail: state, URLs, addresses
```

`kobune` on its own opens a full screen and reads it again every few seconds,
so what is on it is what is true now rather than what was true when you pressed
return. `u` and `d` start and stop whatever the cursor is on, and `?` lists the
rest of the keys — [The dashboard](../reference/cli#the-dashboard) has them
all. The other two print once and exit, which is what a script wants.

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
$ kobune url          # every service, and the way in
$ kobune url web      # a specific one
```

Naming a service prints one line and nothing else, so it substitutes
cleanly:

```console
$ curl -sS --fail-with-body "$(kobune url web)/api/health"
```

**Ask for the URL rather than writing one.** It survives restarts; the port
underneath does not.

To open one on a phone, `--qr` draws it as a code to photograph. That wants
a URL a phone can resolve, so it uses the [tunnel](/guide/tunnel) URL when
there is one:

```console
$ kobune url web --qr
```

## Starting and stopping

```console
$ kobune up               # everything in this workspace
$ kobune up web api       # only these, and whatever they depend on
$ kobune down             # stop this workspace
$ kobune down --all       # every workspace in the project
```

`up` leaves a running container alone, so running it twice is harmless. A
*stopped* container is deleted and recreated, so a configuration change takes
effect — that costs a few seconds, which is cheaper than wondering why an edit
did nothing.

You mostly do not need `up`. A request to a stopped service starts it.

## Logs

```console
$ kobune logs                  # every service in this workspace
$ kobune logs web              # one
$ kobune logs web -n 100       # the last 100 lines
$ kobune logs web -f           # follow
```

Output is undecorated, so it greps and pipes. stdout and stderr stay separate.

With several services, lines are interleaved and each is tagged with the
service it came from.

The [dashboard](../reference/cli#the-dashboard) has a pane for the same thing:
`l` follows whatever the cursor is on without leaving the states behind, and
`L` gives it the whole screen. It keeps the last couple of thousand lines, so
`kobune logs` is still what to reach for when you want all of them or want to
pipe them somewhere.

### Interactive services

A service that runs something you would normally interact with — Turborepo's
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
$ kobune logs -f dev
```

Ctrl-P Ctrl-Q gives the terminal back and leaves the service running.
Everything else, Ctrl-C included, goes to the program. The details, and how to
turn it off, are in
[the CLI reference](../reference/cli#typing-at-a-service).

## Running things inside a container

```console
$ kobune exec web -- npm test
$ kobune exec web -- sh
```

**The exit code is the command's own**, so this works:

```console
$ kobune exec web -- npm test && echo "passed"
```

No TTY is requested. A command that waits for input will hang rather than
prompt, so pass whatever `--yes` flag it needs.

## Environment variables

```console
$ kobune env ls                          # with the layer each came from
$ kobune env get DATABASE_URL            # one value, for piping
$ kobune env set API_KEY=xxx             # this worktree
$ kobune env set LOG_LEVEL=debug --scope project
$ kobune env unset API_KEY
```

A change does not reach a running container. `kobune down && kobune up` picks
it up, and the CLI reminds you.

See [Environment variables](./environment-variables).

## Finishing up

```console
$ kobune rm -w feature-user-auth        # worktree and containers
$ kobune rm -w feature-user-auth -f     # even with uncommitted changes
```

The branch stays. A shared `scope = "project"` service stays too, because other
worktrees are using it.

## The daemon

```console
$ kobune daemon status
$ kobune daemon start
$ kobune daemon stop
$ kobune daemon restart
```

You rarely touch these. Any command starts the daemon if it is down. Stopping
it stops the proxy and DNS, so URLs stop resolving until it comes back — but
containers keep running.

If launchd is installed, launchd goes on holding ports 80, 443 and 53 while the
job is idle, and the next request starts it again. That is how the daemon picks
up new settings without the ports ever changing hands. To have it back without
waiting for a request — after an update, say — `kobune daemon restart` starts it
through launchd too.

## When something is wrong

Work through it in this order rather than reaching for `docker`:

```console
$ kobune status      # what state is it in?
$ kobune logs web    # what does the app say?
$ kobune doctor      # what does the environment say?
```

See [Troubleshooting](./troubleshooting).

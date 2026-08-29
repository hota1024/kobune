# CLI commands

Every command accepts `--json`, and every command but one accepts
`-w, --workspace`.

| Flag | Description |
| --- | --- |
| `--json` | Print the response as JSON. Errors go to stdout too, so an agent watches one stream |
| `-w, --workspace <name>` | Which workspace to act on. Inferred from the current directory when omitted. [`kobune cd`](#kobune-cd-workspace) is the exception: it takes the name after the command and turns this flag down |

## What the output looks like

On a terminal, results are drawn: a framed panel, columns that line up, and
colour on the parts that carry meaning — a service's state, a URL, a command
you are being asked to run. Long-running commands hold the bottom line for
what is happening now, and let finished steps scroll up above it.

Piped, redirected or captured, the same views print as plain text: no frame,
no colour, no cursor movement, and nothing wrapped or truncated however long a
URL is. So `kobune status | grep web` reads the same as it always did.

| Setting or command | How it prints |
| --- | --- |
| `--json` | Never decorated, whatever it is printing to |
| `NO_COLOR` | Set to anything: keeps the layout, drops the colour |
| `TERM=dumb` | Treated as a pipe throughout |
| `kobune url <service>`, `kobune env get` | One bare line, always. They exist to be substituted into other commands |
| `kobune logs`, `kobune exec` | Passed through verbatim, stdout and stderr kept apart |
| `kobune logs -f <service>` | With `tty`, the terminal is the service's — see [Typing at a service](#typing-at-a-service) |
| `kobune` with no command | A full screen, where there is one to draw on — see [The dashboard](#the-dashboard) |

## Setting up

### `kobune init`

Writes a starter `kobune.toml` at the repository root, guessing the project
name from the directory. Run from inside a worktree it still writes to the main
one.

```console
$ kobune init
$ kobune init --force    # overwrite an existing file
```

It also adds `kobune.local.toml` and `.kobune/env.local` to `.gitignore`,
creating the file if there is none. Both belong to one machine, and either
one committed defeats its own purpose. Nothing is appended where git already
covers the name, so a second run does not grow the block, and `--force` is
about `kobune.toml` alone.

#### Converting a compose file

```console
$ kobune init --from-compose              # finds compose.yaml, docker-compose.yml, …
$ kobune init --from-compose infra.yml    # or that one
```

**Not a complete conversion, on purpose.** Compose is large and half of it has
no meaning here, so every key lands in one of three places and none of them is
silence:

- **converted** — `image`, `build`, `ports`, `expose`, `command`,
  `environment`, `depends_on`, `volumes`, `healthcheck`, `working_dir`, `tty`
- **left as a `TODO` beside its service** — what compose cannot express:
  whether a database is shared between worktrees, what `setup` should run
- **named in the report** — `restart`, `deploy`, `networks`, `logging` and the
  rest, per service

Read the TODOs before the first `kobune up`. A generated file that looks
finished and is not costs more than no conversion at all.

Two conversions are worth knowing about:

- **`env_file` becomes `carry`.** The key means the opposite in the two
  formats — compose *reads* the file, Kobune *writes* it — so mapping it
  across would overwrite your `.env` on the first `up`. `carry` is what it
  actually implies: a file each new worktree needs and git does not bring
- **`ports: ["3000:8000"]` takes the container side**, `8000`. Kobune
  publishes on a port it chooses; what it needs is where the app listens
  inside

### `kobune doctor`

Diagnoses the environment and prints a fix for anything that is not `✓`. It
checks the runtime your project uses, the proxy and DNS listeners, launchd
socket activation, the local CA and whether it is trusted, `/etc/resolver`, and
whether a name actually resolves to 127.0.0.1.

### `kobune setup`

Walks through the parts that need root: the LaunchDaemon, the resolver entry,
and trusting the CA. Each step's commands are printed and then offered, one at
a time, and only what you say yes to is run — **nothing runs unasked.** With no
terminal to answer at — an agent, a pipe, `--json` — the commands are printed
for you to run yourself and none of them are run.

```console
$ kobune setup
$ kobune setup --yes       # every step, without asking
$ kobune setup --dry-run   # print the commands, run none of them
```

| Flag | Description |
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

## The dashboard

`kobune` with nothing after it opens a full screen: the workspaces down the
left, and the one under the cursor in full on the right. It reads the listing
again every three seconds, which is the reason it exists — scale-to-zero stops
services without being asked and another terminal can start them, so anything
printed once is out of date within seconds.

```console
$ kobune                 # the workspace the current directory is in
$ kobune tui -w feat-1   # a particular one
```

The cursor starts on the workspace the current directory belongs to, the same
way every other command chooses what to act on.

| Key | What it does |
| --- | --- |
| `↑` `↓`, `j` `k` | Move the cursor, or scroll the logs |
| `tab`, `←` `→` | Move between the panes that are on screen |
| `u` | Start what the cursor is on: one service where the services cursor is, the whole workspace otherwise |
| `d` | Stop it, the same way |
| `o` | Open the URL in a browser |
| `l` | Follow the logs of what the cursor is on, in a pane below |
| `L` | Give that pane the whole screen, and give it back |
| `pg up`, `pg dn` | A screenful at a time |
| `g`, `G` | The top, and the bottom |
| `c` | Check the machine over, as `kobune doctor` does |
| `e` | The environment variables, masked |
| `Q` | The URL as a code to photograph |
| `r` | Read the listing again now |
| `?` | The keys |
| `q`, `ctrl-c` | Leave |
| `esc` | Back out one step: the overlay, then the full screen, then the log pane, then the dashboard |

One start or stop runs at a time, and the bottom line carries its steps as they
arrive — the same steps `kobune up` prints. The reason for a failure stays
there until the next thing happens.

### What the overlays show

`c`, `e`, `Q` and `?` draw a box over the dashboard. Each is a view a
printed command already had — `kobune doctor`, `kobune env ls`,
`kobune url --qr` — so what is in the box is what that command prints, to
the character.

While one is up the arrow keys scroll it rather than moving the cursor
behind it, and `↑` or `↓` appears on its edge when there is more of it
than fits. The key that opened it closes it, `esc` closes it, and any
other key closes it and then does what it always does. The one thing
worse than an overlay you cannot scroll is one you cannot get out from
behind.

**`e` masks the values, and there is no key that unmasks them.** These are
[secrets](../guide/environment-variables#secrets) resolved out of 1Password
and the Keychain, and a dashboard is a screen somebody else can be standing
behind. `kobune env ls --reveal` is a deliberate act and stays one.

`Q` needs nothing from the daemon: the listing already carries the URL. It
uses the tunnel URL when there is one, and says so when there is not,
because a `.localhost` name resolves through this machine and nowhere
else — the phone that just photographed it would get nothing.

### The log pane

`l` opens it on whatever the cursor is on — one service where the services
cursor is, every service in the workspace otherwise — and a second `l` on the
same thing closes it. It opens on the last 200 lines and follows from there.

**It stays on what it was opened on.** Moving the cursor afterwards changes
what the panes above show and leaves the logs alone, so two workspaces can be
compared without losing the stream. Pressing `l` somewhere else moves it there.

Scrolling up stops the pane following, and it says so: `paused` and how many
rows have arrived underneath. `G` goes back to the newest row and starts
following again. New lines never move what you are reading.

A line wider than the pane is wrapped, not cut, and the rows after the first
are indented under the service column so that one line still reads as one
line. Nothing is lost to the edge of the window — a stack trace is exactly
the sort of line that overflows.

Colour comes through. A dev server's output is mostly escape sequences —
Turborepo's prefixes, a status code in green — and they are read rather than
printed, so the pane shows what a terminal would. What a terminal would *do* —
move the cursor, clear the screen — is dropped, since the pane has neither.

**Reading only.** Nothing typed reaches the service; `kobune logs -f` is what
lends it your terminal, and the two are not the same thing. See
[Typing at a service](#typing-at-a-service).

The pane keeps 2000 lines. Past that the oldest go, which is why `kobune logs`
exists for the times you want all of them.

**Leaving stops nothing.** The services keep running, and so does a start or
stop that had not finished. An environment outliving the terminal you looked at
it from is the point of having a daemon at all.

**It needs a terminal.** Piped, redirected, under `TERM=dumb` or with `--json`,
`kobune` on its own prints the help it always has. Asked for by name,
`kobune tui` says what is wrong instead: that it needs a terminal, or — under
`--json` — that a screen has no document to return, and that
`kobune status --json` is the same environment as one.

## Workspaces

### `kobune new <branch>`

Creates a worktree, starts its environment, prints the URLs.

```console
$ kobune new feature/user-auth
$ kobune new hotfix/x --base v1.2.0
$ kobune new feature/x --path ../elsewhere
$ kobune new feature/x --no-start
```

| Flag | Description |
| --- | --- |
| `--base <ref>` | What to branch from, for a new branch |
| `--path <dir>` | Where to put the worktree. Default `../{repo}.wt/{branch}` |
| `--no-start` | Create it without starting anything |
| `--build` | Rebuild images even when nothing has changed |

An existing branch is checked out rather than created.

### `kobune ls`

Every workspace, with how many of its services are running.

```console
$ kobune ls
$ kobune ls --all-projects   # every project this daemon knows about
```

With `--all-projects` a `PROJECT` column appears. Other projects contribute
their **registered** worktrees only — finding unregistered ones would mean
opening someone else's repository — so a project you have never run a command
in shows fewer rows than it would from inside.

### `kobune cd [workspace]`

Moves the shell to a workspace's worktree.

```console
$ kobune cd feature/user-auth
$ kobune cd fuauth             # enough characters, in the right order
$ kobune cd                    # the main worktree
```

The name is matched against both the label and the branch, and it does not
have to be either in full: an exact name is tried first, then a prefix, then
anything the name sits inside, and last the characters appearing in order.
The closest kind of match wins outright, so a label typed in full beats a
longer label that happens to contain it.

**A tie is a question rather than a guess.** Two workspaces that fit the same
way get you both names and no movement, because a shell that lands near where
you meant is worse than one that stays where it was:

```console
$ kobune cd auth
✗ error: `auth` could mean feature-user-auth or fix-auth
  hint: name one of them exactly, or type enough of it to tell them apart
```

**A program cannot change the directory of the shell that started it.** So
this prints the path, and the function from [`kobune
shell-init`](#shell-integration) is what makes it a move. Without that
function, `cd "$(kobune cd feature)"` does the same thing by hand.

**The workspace goes after `cd`, and `-w` is turned down.** That function
recognises `kobune cd …` and passes everything else through, so a second way
of saying it would print a path and leave the shell where it was — which is
harder to see than an error, and looks like the function is not installed.

Tab completes the names, here and after `-w` on the commands that take it.

### `kobune status`

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
mean served, set [`health`](./kobune-toml#readiness).**
:::

### `kobune rm`

Removes the worktree and its containers. The branch stays, and a shared
`scope = "project"` service stays because other worktrees use it.

```console
$ kobune rm -w feature-auth
$ kobune rm -w feature-auth -f   # even with uncommitted changes
```

## Services

### `kobune up [services…]`

Starts services, and whatever they depend on. Everything when none are named.

| Flag | Description |
| --- | --- |
| `--build` | Rebuild images even when nothing Kobune can see has changed |

A running container is left alone unless its image has changed. A stopped one
is recreated so configuration changes take effect.

`--build` is for a change the fingerprint cannot see, such as a file the
Dockerfile copies in.

### `kobune down [services…]`

```console
$ kobune down
$ kobune down web
$ kobune down --all    # every workspace in the project
```

A shared service only stops when you name it, because other worktrees may be
using it.

### `kobune url [service] [--qr]`

Naming a service prints one line and nothing else, to be substituted into
another command:

```console
$ curl -sS --fail-with-body "$(kobune url web)/api/health"
```

Naming none lists every service and where it can be reached, including the
ones with no way in and the tunnel URLs when a tunnel is up:

```console
$ kobune url
web   https://web.feat-1.myapp.localhost
api   https://api.feat-1.myapp.localhost
db    internal only
```

`--qr` draws the URL as a QR code, for opening on a phone. The tunnel URL
is used when there is one — a `.localhost` name resolves on this machine
and nowhere else, and the code says so when that is all there is.

```console
$ kobune url web --qr
```

The URL works while the service is stopped — a request starts it.

### `kobune logs [services…]`

```console
$ kobune logs
$ kobune logs web -n 100
$ kobune logs web -f
$ kobune logs -f dev          # a service with `tty`: type at it
```

| Flag | Description |
| --- | --- |
| `-f, --follow` | Keep streaming |
| `-n, --tail <n>` | Lines from the end |
| `--no-input` | Read only, never take the terminal |

Undecorated, with stdout and stderr kept separate.

#### Typing at a service

A service configured with [`tty`](./kobune-toml#tty) has a terminal, and
`kobune logs -f` lends it this one: colour comes through, a full-screen
interface draws itself, and keys reach the program. This is how Turborepo's
task switcher, a watching test runner and anything else interactive works
under Kobune.

It happens on its own when it can hardly mean anything else — following
**one named service**, from a terminal, without `--json`. A pipeline, an
agent, and `kobune logs -f` with no service named all get the plain stream
they always did. `--no-input` turns it off for the times you want to watch
without being able to type by accident.

| Input | What it does |
| --- | --- |
| Ctrl-P Ctrl-Q | Detach. The service keeps running |
| Anything else | Goes to the program, Ctrl-C included |
| The mouse and the trackpad | Go to the program too, where it wants them |

**The wheel scrolls what the program scrolls.** A full-screen program asks
for mouse reports once, in the first bytes it writes, and attaching an hour
later would otherwise miss the request — leaving a terminal that sends
nothing and a wheel that does nothing. So the daemon listens to the service's
terminal from before the container starts, keeps what the program made of it,
and sets it again here — so Turborepo's log pane scrolls under the pointer as
it does when you run it yourself. It is put back the way it was when you
detach.

**Restarting the daemon loses it.** The announcement was heard once, by a
daemon that is no longer running, and there is nowhere to read it back from.
The log is not that place: Docker holds everything after a program's last
newline until the program ends, and a full-screen one never ends a line — so
those bytes arrive in the log only once the service is over, which is exactly
too late to be of use. The service keeps running and keys still reach it, but
the mouse and the alternate screen are gone until `kobune down && kobune up`
starts the container again.

Ctrl-C belongs to the program: in a task runner it usually means "quit",
which stops the service. Ctrl-P Ctrl-Q is how you leave without stopping
anything — the sequence `docker attach` uses.

Asking for a service that has no terminal is not an error. Kobune says so,
in one line, and streams the logs as usual.

::: warning Apple Container fixes the size at start-up
Its terminal takes its size when the service starts and cannot be resized
afterwards, so a full-screen program draws to 120×40 however big the window
is. Kobune says so when you attach. Docker follows the window.
:::

### `kobune exec <service> -- <command>`

```console
$ kobune exec web -- npm test
$ kobune exec web -- sh
$ kobune exec -C /workspace/apps/api api -- pnpm test   # somewhere else
```

**The exit code is the command's own.** No TTY is requested, so anything
waiting for input hangs rather than prompts.

`-C` sets the working directory, defaulting to the service's
[`workdir`](./kobune-toml#image-and-command). It is `-C` rather than `-w`
because `-w` already selects the workspace.

#### `--fresh`

```console
$ kobune exec --fresh api -- env
$ kobune exec --fresh api -- cat /workspace/.env
$ kobune exec --fresh api -- sh -c 'pnpm install --frozen-lockfile'
```

No stdin is attached, so `-- sh` on its own reads end-of-file and exits at
once. Pass what you want run with `sh -c`.

Runs the command in a container made for it and removed afterwards, built from
the service's image, environment and volumes but **without the service's
start-up command**.

**The service does not have to be running**, which is the point: a start-up
script that fails leaves nothing to exec into, and that is when you most want
to look around.

It publishes no ports and carries no Kobune labels, so it cannot take the real
container's ports, appear in `kobune status`, or answer to the service's name
on the network. The image is pulled or built first if it is not there yet, so
this works before a service has ever come up cleanly.

## Interrupting

Ctrl-C asks the daemon to stop and waits for its reply, rather than killing the
CLI where it stands. The exit code is 130.

Work already done is not undone: a cancelled `up` can leave a container
running, which `kobune status` shows and `kobune down` clears.

`kobune logs -f` is the exception — Ctrl-C is simply how you leave it. Where
it has [taken the terminal](#typing-at-a-service), Ctrl-C is the program's,
and Ctrl-P Ctrl-Q leaves.

## Environment variables

```console
$ kobune env ls [--reveal] [--service <name>]
$ kobune env get <KEY>
$ kobune env set <KEY=VALUE> [--scope global|project|workspace]
$ kobune env unset <KEY> [--scope …]
```

`ls` shows which layer each value came from and masks secrets. `--reveal` shows
plain values but still leaves secret *references* as references. `get` prints
one value for piping.

`--service` shows what one container is given, its own
[`env`](./kobune-toml#environment) included, so
`kobune env ls --service api` answers "is `KOBUNE_URL_WEB` actually reaching
`api`?" without starting anything. Without it, only what every service shares:
a service's own `env` belongs to that service, and there is no
`KOBUNE_SERVICE` because no service is being named. `get` takes it too.

When a `${...}` will not settle, `ls` shows *that* value as written and says
why underneath rather than refusing — the one at fault can only be found by
looking at the values, and the ones that settled are still shown settled. In
`--json` such a value carries an `unsettled` object (`reference`, and a
`reason` of `undefined`, `only_with_service`, `needs_proxy`, `secret` or
`cycle`); a value that settled has no such field. `get` does refuse, with exit
code 7, since what it prints is meant to be used.

The layer column names five of them, innermost winning: `injected`, `global`,
`project`, `service`, `workspace`. `service` is a service's own `env` in
`kobune.toml` — it has its own name because reading it as `project` would
send you to edit `.kobune/env` for a value the service overrides.

`--scope` defaults to `workspace`.

## Configuration

```console
$ kobune config show [--all]
```

Three files make up the configuration, read in turn and merged, later ones
winning: `~/.kobune/config.toml`, the project's `kobune.toml`, and
`kobune.local.toml` beside it. The result is in no file you can open, which is
what this is for.

```console
$ kobune config show
╭ config ───────────────────────────────────────────╮
│ LAYER    FILE                                     │
│ global   ~/.kobune/config.toml          read      │
│ project  ~/src/myapp/kobune.toml        read      │
│ local    ~/src/myapp/kobune.local.toml  not found │
│                                                   │
│ › every value comes from one layer alone          │
╰───────────────────────────────────────────────────╯
```

**A layer with no file is still listed, with the path it was looked for at.**
"My override is not applying" is usually "the file is somewhere else", and only
the path can say so. Two of the three are meant to be missing most of the time,
so `not found` is not a fault.

Below the layers come the keys more than one layer had an opinion about, by
their dotted path, with the layer that won and the ones it beat. `--all` lists
every key instead; without it a configuration of any size would bury the four
that are contested.

**It keeps working when the configuration does not.** Where the merge is not a
usable configuration — a misspelled key, or two layers that contradict each
other — every other command stops with that message and nothing else. This one
lists the files it read and says which layer set what, with the message
underneath, since that is the state the question is usually being asked from.
A file that is not TOML at all still fails here, because the error already
names it and there is no merged document to explain.

See [Layers](./kobune-toml#layers) for what each file is for.

## The tunnel

```console
$ kobune tunnel enable --provider quick --public
$ kobune tunnel enable --provider cloudflare --domain example.com --public
$ kobune tunnel disable
$ kobune tunnel status
```

`--provider` picks the tunnel service. `quick` needs no account and no domain
and hands out a throwaway hostname per service; `cloudflare` runs a named
tunnel on a zone of yours. Both are remembered after the first time, along with
the domain, so later runs need neither flag. The default is `cloudflare`.

`--public` is required, and acknowledges that the environment goes on the
internet with nothing Kobune can verify in front of it. Refusing says which
case you are in: a hostname of yours that you can protect, or one the service
handed out that nothing can be attached to.

`--domain` applies to `cloudflare` and takes the zone itself — `example.com`,
not `dev.example.com`. One level below the zone is what its Universal SSL
certificate covers, and a tunnel hostname sits exactly there.

## Agents

```console
$ kobune skill install [--force]
$ kobune skill show
```

Writes `.claude/skills/kobune/SKILL.md`. Unchanged content is left alone.

## The daemon

```console
$ kobune daemon start
$ kobune daemon stop
$ kobune daemon restart
$ kobune daemon status
```

Any command starts the daemon if it is down, so these are rarely needed. On a
machine with the LaunchDaemon installed, starting one goes through launchd —
which is how ports 80 and 443 stay held. `stop` leaves the job idle rather than
unloaded: launchd keeps the ports, and the next request through one of them
starts the daemon again. `restart` is what brings it straight back without
waiting for that request.

`kobune ping` is the shortest way to ask whether one is answering. It prints
what `daemon status` prints without the socket line, and starts a daemon if
there is none — so a `pong` says the daemon is up and speaking a protocol this
CLI knows, not that it already was.

`restart` is for the one case that does not fix itself: a daemon left running
from an older build. It answers every command happily and speaks a protocol the
new CLI does not, which reads as

```
error: the daemon speaks protocol 6, which this kobune (protocol 7) cannot
talk to. Restart it with `kobune daemon restart`
```

Updating the binaries does not replace the process that is already running.

`start` and `restart` fail when what came up is not launchd's job. The daemon is
running, launchd still holds 80, 443 and 53 for the job that did not, and no URL
answers — so the exit code says 1 and the hint says which of those states the
machine is in. Nothing else changes its exit code over this: `kobune up` and the
rest did what they were asked, and print the same wording as a notice.

`restart` fails for a second reason: the daemon it meant to replace is still
there. The stop is given five seconds to take, and one that outlasts it is
holding the socket when the start reaches for it, so nothing was restarted
however readily it answers.

```
error: the daemon outlasted every stop, so nothing was restarted: what is
answering has been up 1h 0m
```

A daemon launchd woke during the stop answers in exactly the same way, and that
one is where a restart wanted the machine. What tells the two apart is how long
each has been up: the replacement started while the restart was waiting, and
the daemon that would not go away carries that wait on top of the uptime it had
before.

## Keeping it current

```console
$ kobune update
$ kobune update --check
```

Replaces both binaries in the directory the running `kobune` came from with the
current `nightly`. `--check` reports and installs nothing. Under `--json`:

```json
{ "status": "available", "commit": "…", "running": "…" }
```

`status` is one of `current`, `available`, `installed` or `unknown` — `unknown`
meaning this build records no commit, so there is nothing to compare.

`installed` carries `next` as well: the commands worth running now that the
binaries have moved, each with the state that makes it worth running.

```json
{
  "status": "installed",
  "commit": "…",
  "next": [
    { "command": "kobune daemon restart", "reason": "the daemon is still the previous build" }
  ]
}
```

It is empty when there is nothing to do — no daemon was running — and it holds
only what the build being replaced can still be sure of. The rest is printed by
the build that lands, on its first run: one line per step on **stderr**, under
`kobune changed to 9f3c1a2 since the last run`, once per build and never under
`--json`. The Skill step is about the repository the command ran in. The commit
that last ran is kept in `~/.kobune/build.json`, written after the lines are
printed, and `kobune daemon` carries no notice at all — `stop` returns before
the socket goes quiet, so the answer would be about a daemon it has just asked
to leave.

A check runs by itself once a day after any command and prints one line to
stderr. `KOBUNE_NO_UPDATE_CHECK` turns it off, and `--json` never includes it.

`kobune --version` checks too, every time rather than once a day, and after
the version line rather than before it:

```console
$ kobune --version
kobune 0.1.0 (c7282b8)
› a newer build is available (9f3c1a2). Install it with kobune update
```

The line is stderr, there is none when the running build is the published one,
and `--json` and `KOBUNE_NO_UPDATE_CHECK` skip the check as they do the daily
one.

## Taking it off again

```console
$ kobune uninstall
╭ uninstall ─────────────────────────────────────────────────────────────────╮
│ containers:                                                                │
│ myapp / main               web                                             │
│ myapp / main               db                                              │
│ myapp / feature-user-auth  web                                             │
│                                                                            │
│ storage — the data in it goes too:                                         │
│ myapp  kobune-myapp-pgdata                                                 │
│ myapp  kobune-myapp-feature-user-auth.node-modules                         │
│                                                                            │
│ files:                                                                     │
│ state, logs and the local CA  /home/u/.kobune                              │
│ shell completions             /home/u/.config/fish/completions/kobune.fish │
│ the binary                    /home/u/.local/bin/kobune                    │
│ the binary                    /home/u/.local/bin/kobuned                   │
│                                                                            │
│ needs root:                                                                │
│   stop the LaunchDaemon holding 80/443/53                                  │
│     sudo launchctl bootout system/dev.kobune.daemon                        │
│     sudo rm /Library/LaunchDaemons/dev.kobune.daemon.plist                 │
│   stop trusting the local CA                                               │
│     sudo security remove-trusted-cert -d ~/.kobune/ca/kobune-ca.crt        │
│                                                                            │
│ left alone — 2 worktrees:                                                  │
│   /path/to/myapp                                                           │
│   /path/to/myapp.wt/feature-user-auth                                      │
╰────────────────────────────────────────────────────────────────────────────╯
Remove all of this? [y/N]
```

| Flag | Description |
| --- | --- |
| `-y, --yes` | Go ahead without asking. Required where there is no terminal |
| `--dry-run` | Print the list and remove nothing |

**Worktrees are never touched.** They are your checkouts, with your
uncommitted work in them; `kobune rm` removes one at a time, and asks for
`--force` when git objects. They are listed so you can see what is being left
behind.

**Named volumes go, and are named before they do.** A project volume is
shared between worktrees and outlives every one of them, so nothing on the
`kobune rm` path ever removes it — an uninstall is where it finally goes. It
is listed under the name the runtime knows it by, the one `docker volume ls`
prints, so a database worth keeping can be copied out before you answer. They
are found by that label rather than from the daemon's records, which is how
the storage of a project whose repository you deleted months ago is reclaimed
too.

Storage that could not be listed — a runtime that is installed and not
answering — is reported rather than counted as none, and so is a volume that
would not go. Both mean an uninstall that left something behind, and both
make the exit code non-zero.

Nothing is listed that is not there, so the list is what is actually on the
machine rather than everywhere Kobune might have put something. The binaries
are left alone when they are `cargo build` output — running `uninstall` from a
checkout removes the installation, not your build.

The steps that need root are run with `sudo`, which asks for your password.
Without a terminal to type into — an agent, a pipe, CI — they are printed
instead, the same as `kobune setup` does, and the rest of the uninstall still
happens.

## Completions

```console
$ kobune completions <bash|zsh|fish|elvish|powershell>
```

Writes the script to stdout. See
[Installation](../guide/installation#shell-completions) for where each shell
expects it; the install script does this already.

The workspace names are asked for as you type, so `kobune cd` and `-w`
complete against the worktrees that exist now. Nothing is started to answer:
with no daemon running, Tab offers nothing rather than waiting for one to come
up.

## Shell integration

```console
$ kobune shell-init <bash|zsh|fish>
```

Prints one shell function, which passes everything that is not `cd` straight
through to the command. Load it from your shell's startup file:

::: code-group
```console [fish]
$ echo 'kobune shell-init fish | source' >> ~/.config/fish/config.fish
```

```console [zsh]
$ echo 'eval "$(kobune shell-init zsh)"' >> ~/.zshrc
```

```console [bash]
$ echo 'eval "$(kobune shell-init bash)"' >> ~/.bashrc
```
:::

This is what [`kobune cd`](#kobune-cd-workspace) needs, and all it needs. The
install script writes the completions for you and leaves this line alone: a
startup file is yours, and a program that edits one has to be trusted with
everything else in it.

`elvish` and `powershell` are not accepted here. The function is written by
hand for each shell, and one nobody has run is worse than none.

## Environment variables that configure Kobune

| Variable | Description |
| --- | --- |
| `KOBUNE_HOME` | Where state, logs, the socket, the CA and `config.toml` live. Default `~/.kobune` |
| `KOBUNE_HTTP_PORT` | Proxy HTTP port. Default 80, falling back to 18080. A port named here is used as given |
| `KOBUNE_HTTPS_PORT` | Proxy HTTPS port. Default 443, falling back to 18443. A port named here is used as given |
| `KOBUNE_DNS_PORT` | DNS port. Default 53 |
| `KOBUNE_CLOUDFLARED` | A `cloudflared` binary somewhere neither `PATH` nor the usual install prefixes reach |
| `KOBUNE_CONTAINER` | The same, for Apple Container's `container` |
| `KOBUNE_DAEMON` | The `kobuned` to start, for when it is not sitting beside the `kobune` being run |
| `KOBUNE_LOG` | Log filter for the daemon, e.g. `debug` |
| `KOBUNE_NO_UPDATE_CHECK` | Set to anything to stop the update check, both the daily one and `--version`'s |

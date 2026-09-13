# kobune-studio

The dashboard, in a browser. It shows one project: every workspace the
project has, what is running in each, the address each service answers on,
and the logs underneath — the same screen `kobune` with no arguments draws
in a terminal, for when a terminal is not where you are.

It is a client of the daemon and nothing more. It depends on
`kobune-client` and `kobune-api`, never on `kobune-runtime`, so nothing
here knows what Docker is (`docs/DESIGN.md` §3, §13).

## Running it

```console
$ kobune studio
kobune studio  http://127.0.0.1:17823/#token=f7711f8253038449…
```

`kobune studio` hands over to this binary — `exec`, so the terminal, the
signals and the exit code are the studio's from that point. Running
`kobune-studio` yourself does the same thing and takes the same flags.

It opens your browser at that URL. `--no-open` prints it instead, which is
what you want over SSH or in a container, and `--port` moves the listener.
`-w` opens a particular workspace first rather than the busiest one; it
travels in the fragment beside the token, so it is not sent to the server
either.

`--path` names the project when the working directory is not inside it.
The subcommand does not pass one — the working directory is inherited, and
the studio resolves the project from it like every other command.

`kobune studio --json` is refused rather than ignored, for the reason the
full-screen dashboard refuses it: a screen is not something to answer an
agent with, and one that never returns is worse.

**The token in the fragment belongs to that run of the process.** Restarting
the studio mints a new one and invalidates the URL you had. Opening
`http://127.0.0.1:17823/` without a token gets you a page that says so
rather than a dashboard.

The page keeps it in `sessionStorage`, so reloading does not send you back
to the terminal for a fresh URL. That is per tab and goes when the tab
does, and script running on this origin could read a variable just as
easily — so it buys the reload without giving anything up. A token the
server refuses is thrown away rather than retried.

## Building it

The page is compiled into the binary, so it has to exist before `cargo`
runs:

```console
$ cd apps/studio/web
$ pnpm install
$ pnpm build
$ cargo build -p kobune-studio
```

A checkout that has never run `pnpm` still compiles — `build.rs` puts a
placeholder page in `web/dist` saying what to run. You will get a binary
that serves that sentence and nothing else, which is the intended failure:
the Rust build does not become hostage to a JavaScript one.

`pnpm dev` serves the page from Vite with hot reload and proxies `/api` to
a `kobune-studio` you started separately. The proxy rewrites `Host` and
`Origin`, because the guard below checks both and Vite's port is neither.

## Who may connect

The daemon's socket asks for nothing. The only access control it has is who
can reach it at all — the mode on `KOBUNE_HOME` and the uid on the far end
(`docs/DESIGN.md` §3). **A listening port has neither**, and
`kobune exec <service> -- env` prints the secrets §8 resolved from 1Password
and the Keychain. Three things stand in, and a request has to pass all
three:

- **The listener never leaves loopback.** There is no `--host`, because the
  one setting that turns "a page on this machine" into "anybody on this
  network may start containers" is a setting §3 has no reader for
- **A token, minted per run**, required as `Authorization: Bearer` on every
  `/api` request. `Authorization` is not a CORS-simple header, so a page on
  another origin that tried the call would be preflighted — and nothing
  here answers a preflight
- **`Host` and `Origin` are checked**, which is what stops DNS rebinding: a
  name that resolves to 127.0.0.1 to get past the browser still arrives
  with its own name in `Host`

A refused request is a bare 401. Saying which of the three failed would
tell somebody who has proved nothing.

The static page is served without a token. It is an empty shell until it
has one, and the alternative is a chicken and egg.

## What it does not do

- **`exec` is not there.** Lending a terminal to a container needs a
  terminal to lend, and the API's half of it is a PTY rather than a log.
  The button says where the command lives instead of pretending
- **Environment values stay masked.** There is no reveal, because a browser
  tab is the weakest place on the machine to put a resolved secret
- **Tabs are this browser's.** They are kept in `localStorage`, so a second
  browser opens its own set. Keeping them with the daemon's state would
  make them the same tabs everywhere, and would mean new API
- **An installation updated before the studio existed will not have one.**
  That `kobune update` extracted two binaries and did not know to look for
  a third, so the CLI has the subcommand and nothing to hand over to.
  Running `kobune update` again fixes it, and `kobune studio` says so

## How the screen stays true

The listing is re-read every three seconds on a connection of its own,
which is what the TUI and the menu-bar app already do (`docs/DESIGN.md`
§10). One HTTP request is one connection, so an `up` that takes a minute
never blocks the poll that draws the screen for the minute it most needs
drawing.

A followed log is its own connection, streamed to the page as server-sent
events. Closing the pane aborts the request, which cancels the follow — the
daemon lets go of the runtime's log stream at once, rather than leaking one
follower per pane anybody ever opened.

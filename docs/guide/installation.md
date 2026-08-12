# Installation

```console
$ curl -fsSL https://minato.1024.works/install.sh | sh
```

That picks the archive for your machine, checks it against its published
`.sha256`, installs `minato` and `minatod` into `~/.local/bin`, and writes shell
completions for whichever of bash, zsh and fish you have.

Nothing it does needs root, and it prints the one PATH line you may need at the
end — in the syntax of the shell you are actually in, so a fish user is told
`fish_add_path` rather than an `export` line that fish would reject.

## Requirements

| | |
| --- | --- |
| **A container runtime** | Docker, OrbStack or colima — or Apple Container on macOS 26+ |
| **macOS** | Fully supported. Linux works for the core, minus launchd socket activation |
| **Rust 1.88+** | Only to [build from source](#build-it) |

The desktop app is optional and needs a little more; see
[The desktop app](./gui).

## The install script

Read it before you run it — [`install.sh`](https://minato.1024.works/install.sh)
is about 250 lines of POSIX shell and does nothing surprising. Two settings:

| | |
| --- | --- |
| `MINATO_INSTALL_DIR` | where the binaries go, `~/.local/bin` by default |
| `MINATO_NO_COMPLETIONS` | set to anything to skip the completion scripts |

```console
$ curl -fsSL https://minato.1024.works/install.sh | MINATO_INSTALL_DIR=/usr/local/bin sh
```

### The PATH line

If the install directory is not already on `PATH`, the script says how to add
it — for one shell, the one you are in:

::: code-group
```console [fish]
fish_add_path ~/.local/bin
```

```console [zsh]
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
. ~/.zshrc
```

```console [bash]
# ~/.bash_profile on macOS, ~/.bashrc on Linux: a login shell reads only
# the first, and macOS Terminal opens login shells.
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bash_profile
. ~/.bash_profile
```

```console [tcsh]
echo 'setenv PATH $HOME/.local/bin:$PATH' >> ~/.tcshrc
source ~/.tcshrc
```

```console [nushell]
# in $nu.config-path
$env.PATH = ($env.PATH | prepend '~/.local/bin')
```

```console [elvish]
# in ~/.config/elvish/rc.elv
set paths = ['~/.local/bin' $@paths]
```

```console [powershell]
# in $PROFILE
$env:PATH = "$HOME/.local/bin" + [IO.Path]::PathSeparator + $env:PATH
```
:::

It works out which one you are in from the process tree, not from `$SHELL`.
`$SHELL` is the *login* shell, which is a different thing the moment you start
fish from a zsh login — and being handed `export PATH` in fish is exactly the
kind of line that gets pasted into a config file and stays broken for months.
`ksh`, `mksh` and `dash` are recognised too, and fall back to `~/.profile`.

When it cannot tell, it prints all of them and lets you pick, rather than
guessing.

It installs the `nightly` build, which is replaced on every merge to `main`.
That is the latest build rather than a release: nothing in it carries a version,
and what it contains changes without notice.

Rerunning it upgrades in place. So does [`minato update`](#keeping-it-up-to-date),
without needing the network twice or a shell pipeline.

## A prebuilt binary, by hand

The same archives the script downloads:

| | |
| --- | --- |
| Apple Silicon | `minato-aarch64-apple-darwin.tar.gz` |
| Intel Mac | `minato-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 | `minato-x86_64-unknown-linux-gnu.tar.gz` |

```console
$ gh release download nightly --repo hota1024/minato \
    --pattern 'minato-aarch64-apple-darwin.tar.gz*'
$ shasum -a 256 -c minato-aarch64-apple-darwin.tar.gz.sha256
$ tar xzf minato-aarch64-apple-darwin.tar.gz
$ cd minato-aarch64-apple-darwin
```

::: warning macOS quarantines unsigned binaries
```console
$ xattr -d com.apple.quarantine minato minatod
```
The install script does this for you. Signing is unresolved, so the other
option is to build from source. The desktop app is not shipped at all for the
same reason: Gatekeeper stops an unsigned `.app` outright rather than warning
about it.
:::

## Build it

Minato is not on crates.io yet, so building means cloning it.

```console
$ git clone https://github.com/hota1024/minato
$ cd minato
$ cargo build --release --workspace
```

That produces two binaries in `target/release`:

- `minato` — the CLI you use
- `minatod` — the daemon it talks to

Put them somewhere on your `PATH`:

```console
$ cp target/release/minato target/release/minatod ~/.local/bin/
```

They ship together and expect to sit side by side: the CLI starts the daemon by
looking next to itself.

## Shell completions

The install script writes these already. To do it yourself, or for a shell it
did not find:

::: code-group
```console [fish]
$ minato completions fish > ~/.config/fish/completions/minato.fish
```

```console [zsh]
$ mkdir -p ~/.local/share/zsh/site-functions
$ minato completions zsh > ~/.local/share/zsh/site-functions/_minato
$ echo 'fpath=(~/.local/share/zsh/site-functions $fpath)' >> ~/.zshrc
```

```console [bash]
$ mkdir -p ~/.local/share/bash-completion/completions
$ minato completions bash > ~/.local/share/bash-completion/completions/minato
```
:::

fish loads its file with no further setup. zsh needs the directory on `fpath`,
which is why the extra line is there. bash needs
[bash-completion](https://github.com/scop/bash-completion) 2.x installed.

`elvish` and `powershell` are also accepted, since they come free with the
generator, but nothing is tested against them.

## Keeping it up to date

```console
$ minato update
› installing 9f3c1a2…
╭ update ─────────────────────────────────────────────────────────────╮
│ installed  9f3c1a2                                                  │
│                                                                     │
│ › the daemon is still the previous build, so run minato daemon stop │
╰─────────────────────────────────────────────────────────────────────╯
```

`update` replaces the installation it is run from — the directory holding the
`minato` that you invoked, not a configured one — with the current `nightly`.
Both binaries go together, because a CLI and a daemon from different builds
would not agree on the protocol between them.

The new files are written beside the old ones and renamed into place. A running
executable cannot be written to, but it can be replaced, so an update while the
daemon is up leaves it running the old build until it is restarted. Which is
what the last line is about: stopping it is how launchd picks the new one up,
and without the LaunchDaemon installed it reads `minato daemon restart`
instead, because nothing else would bring it back.

That line is **worked out, not printed regardless**: with no daemon running
there is nothing to replace, and the panel says only what it installed.

To look without installing:

```console
$ minato update --check
╭ update ─────────────────────────╮
│ available  9f3c1a2              │
│ running    c7282b8              │
│                                 │
│ › install it with minato update │
╰─────────────────────────────────╯
```

### What a new build leaves to do

The panel above can only speak for the build being replaced. Whether the Skill
in a repository matches the new one, and whether the LaunchDaemon is the shape
the new one writes, are questions only the build that has landed can answer —
so it answers them itself, the first time you run it:

```console
$ minato url web
https://web.myapp.localhost
› minato changed to 9f3c1a2 since the last run
› the daemon is still the previous build, so run minato daemon stop
› this repository's Skill is not this build's, so run minato skill install --force
```

Each line is there because something on this machine says so: a daemon answered
and it was not this build, `.claude/skills/minato/SKILL.md` differs from the one
this binary carries, the installed plist was written to an older shape. Nothing
is guessed — a repository that never had the Skill is not offered one, and a
plist from before this was recorded is left alone rather than called old.

It appears **once per build**, on the first run that is not `--json`, and this
is also what covers the updates `minato update` knows nothing about: rerunning
`install.sh`, a package manager, a build of your own. `~/.minato/build.json`
holds the commit that last ran, and is the whole of the state involved.

It is stderr, like every other remark of the CLI's own, and never appears under
`--json` — an agent's stream stays one document. `minato update --json` carries
the same steps as data instead:

```json
{
  "status": "installed",
  "commit": "9f3c1a2…",
  "next": [
    {
      "command": "minato daemon stop",
      "reason": "the daemon is still the previous build"
    }
  ]
}
```

### The automatic check

Once a day, after a command has finished, Minato asks GitHub what `nightly`
points at and says one line on **stderr** if it is not what you are running:

```
a newer build is available (9f3c1a2). Run `minato update`
```

It runs after the command, never before, so a slow network cannot delay output
you are waiting for. It is skipped entirely under `--json`, so nothing an agent
parses ever contains it. Every failure is silent: a check that cannot reach
GitHub has nothing to say.

Turn it off with an environment variable:

```console
$ export MINATO_NO_UPDATE_CHECK=1
```

The answer is cached in `~/.minato/update-check.json` for 24 hours, and the
notice is repeated from that cache in between — a warning shown once a day and
never again would just be missed.

### `minato --version`

The flag carries the same check, and unlike the automatic one it asks every
time: `--version` is a question about the build in front of you, and answering
it from a cache up to a day old would be answering a different one. The version
line is printed first and the check made after, so nothing you asked for waits
on the network:

```console
$ minato --version
minato 0.1.0 (c7282b8)
› a newer build is available (9f3c1a2). Install it with minato update
```

Nothing is added when you are already on the published build — the version line
said which build this is, and that is the whole of what was asked. `--json` and
`MINATO_NO_UPDATE_CHECK` skip it exactly as they skip the automatic one, and so
does a network that cannot be reached.

A build made from source reports nothing either way. It records the commit it
was built from, and with no commit to compare there is no honest answer: "up to
date" would be a guess, and "out of date" would push you off a build you made
on purpose.

```console
$ minato --version
minato 0.1.0 (9f3c1a2)
```

## Pick a container runtime

### Docker

Nothing to configure. Minato talks to the Docker API directly and never shells
out to the `docker` CLI, so the CLI does not have to be installed — only the
API has to be reachable. Docker Desktop, OrbStack and colima all work.

```console
$ minato doctor
│ …
│ ✓  container runtime  docker 29.4.0
│ …
```

### Apple Container

Needs macOS 26 or later and the service running:

```console
$ container system start
```

Then set it in `minato.toml`:

```toml
[runtime]
default = "apple"
```

There are two differences worth knowing before you choose it — see
[Runtimes](./runtimes).

## Start the daemon

```console
$ minato daemon start
╭ minatod ───────────────────────────────╮
│ running                                │
│                                        │
│ version   0.1.0                        │
│ protocol  1                            │
│ runtime   docker 29.4.0                │
│ uptime    0s                           │
│ socket    ~/.minato/minatod.sock       │
╰────────────────────────────────────────╯
```

You rarely need to do this by hand; any command starts the daemon if it is not
already up. The daemon holds the proxy, DNS and the idle timer, which is why
something has to stay resident.

## The privileged setup

To reach `https://web.myapp.localhost` with no port number, three things need
root, once:

```console
$ minato setup
╭ setup ─────────────────────────────────────────────────────────────────╮
│ the URLs need 3 steps, and they need root.                             │
│ each one is shown before it is run, and nothing runs until you say so. │
│                                                                        │
│ 1. let launchd hold 80/443/53 (the daemon itself stays non-root)       │
│ 2. point *.localhost at Minato's DNS                                   │
│ 3. trust the local CA, so HTTPS stops warning                          │
╰────────────────────────────────────────────────────────────────────────╯

1/3 let launchd hold 80/443/53 (the daemon itself stays non-root)
  generated plist: ~/.minato/dev.minato.daemon.plist
  sudo cp ~/.minato/dev.minato.daemon.plist /Library/LaunchDaemons/…
  …
run this? [y/N] y
  ✓ done

2/3 point *.localhost at Minato's DNS
  sudo mkdir -p /etc/resolver && printf 'nameserver 127.0.0.1\n' | sudo tee …
run this? [y/N] n
  – skipped
…
```

**Nothing runs unasked.** Every command is on the screen before the question
about it is, so what you agree to is what you have just read, and anything you
decline is printed again at the end to run by hand.

Say yes to all of it with `minato setup --yes`, or read the commands without
being asked about any of them with `minato setup --dry-run`.

**With no terminal to answer at — an agent, a pipe, `--json` — the commands are
printed and none of them are run.** An unattended `sudo` hangs at the password
prompt, and from your side it would look like a silent privilege escalation.

Afterwards:

```console
$ minato daemon stop   # launchd starts it again, holding the real ports
$ minato doctor
```

### Skipping it

You do not have to, and nothing has to be configured to skip it. When 80 and
443 cannot be held, the proxy takes 18080 and 18443 instead and the port goes
into the URL:

```console
$ minato url web
https://web.feat-1.myapp.localhost:18443
```

`minato doctor` says so rather than leaving you to notice.

To choose the ports yourself, name them — a port you name is used as given and
never fallen back from:

```console
$ export MINATO_HTTP_PORT=8080 MINATO_HTTPS_PORT=8443 MINATO_DNS_PORT=15353
$ minato daemon start
```

**DNS has no fallback**, because moving it achieves nothing on its own: the
`/etc/resolver` entry names the port, and writing that needs root either way.
That part is macOS, not Minato. `minato doctor` prints the exact command,
including the right port.

::: tip Already run `minato setup`?
Then the proxy does *not* fall back. launchd holds 80 whether or not its job
is running, so a refusal there means the job needs starting — and listening
somewhere else would hide that. `minato doctor` says which it is.
:::

## Check the result

```console
$ minato doctor
```

Every line comes with a fix when it is not `✓`. If something here is red, sort
it out before going further — most confusing behaviour later traces back to it.

## Where things live

`MINATO_HOME`, `~/.minato` by default, holds the daemon socket, its state file,
logs, the local CA, and any generated tunnel configuration.

A Unix socket path is limited to about 100 bytes, so `MINATO_HOME` cannot be
somewhere deep. Minato checks this at startup and tells you rather than failing
with an opaque error.

## Taking it off again

```console
$ minato uninstall
```

It shows what it found — containers, the daemon's state, the binaries, the
completions, and the steps that need root — and asks before removing any of
it. `--dry-run` prints the list and stops; `--yes` skips the question, and is
required where there is no terminal to ask at.

**Your worktrees are left where they are.** They are listed, so you can see
what is being kept, and `minato rm` is how one goes.

The full list of what it removes, and how the privileged steps are handled, is
in the [CLI reference](../reference/cli#taking-it-off-again).

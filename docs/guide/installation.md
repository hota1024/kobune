# Installation

Minato is not published to crates.io yet, so it is built from source.

## Requirements

| | |
| --- | --- |
| **Rust** | 1.85 or later |
| **A container runtime** | Docker, OrbStack or colima — or Apple Container on macOS 26+ |
| **macOS** | Fully supported. Linux works for the core, minus launchd socket activation |

The desktop app is optional and needs a little more; see
[The desktop app](./gui).

## Build it

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

## Pick a container runtime

### Docker

Nothing to configure. Minato talks to the Docker API directly and never shells
out to the `docker` CLI, so the CLI does not have to be installed — only the
API has to be reachable. Docker Desktop, OrbStack and colima all work.

```console
$ minato doctor
  ✓ container runtime             docker 29.4.0
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
minatod 0.1.0 is running
```

You rarely need to do this by hand; any command starts the daemon if it is not
already up. The daemon holds the proxy, DNS and the idle timer, which is why
something has to stay resident.

## The privileged setup

To reach `https://web.myapp.localhost` with no port number, three things need
root, once:

```console
$ minato setup
The URLs need the following setup.
It requires root, so read each command before running it.

1. let launchd hold 80/443/53 (the daemon itself stays non-root)
   sudo cp ~/.minato/dev.minato.daemon.plist /Library/LaunchDaemons/…
   …

2. point *.localhost at Minato's DNS
   sudo mkdir -p /etc/resolver && printf 'nameserver 127.0.0.1\n' | sudo tee …

3. trust the local CA, so HTTPS stops warning
   sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/… 
```

**`minato setup` prints these; it never runs them.** An unattended `sudo` hangs
an agent at the password prompt, and from your side it would look like a silent
privilege escalation. Read them, then run them yourself.

Afterwards:

```console
$ minato daemon stop   # launchd starts it again, holding the real ports
$ minato doctor
```

### Skipping it

You do not have to. Name unprivileged ports and everything works, with the port
in the URL:

```console
$ export MINATO_HTTP_PORT=8080 MINATO_HTTPS_PORT=8443 MINATO_DNS_PORT=15353
$ minato daemon start
```

You still need the `/etc/resolver` entry for `*.localhost` to resolve — that
part is macOS, not Minato. `minato doctor` prints the exact command, including
the right port.

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

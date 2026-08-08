# Minato

[![CI](https://github.com/hota1024/minato/actions/workflows/ci.yml/badge.svg)](https://github.com/hota1024/minato/actions/workflows/ci.yml)

A development environment manager for AI agents.

**Create a git worktree, and its preview environment is up.**

```console
$ minato new feature/user-auth
✓ worktree created  ~/ghq/github.com/hota1024/myapp.wt/feature-user-auth
✓ web   https://web.feature-user-auth.myapp.localhost
✓ api   https://api.feature-user-auth.myapp.localhost
```

## What it does

- **A worktree is an environment** — one appears with the worktree and goes with it
- **No ports to remember** — every service gets `{service}.{workspace}.{project}.localhost`
- **Scale-to-zero** — an untouched environment stops itself and wakes on the next request, so create as many worktrees as you like
- **Reachable remotely** — share with a phone or an outside reviewer over Cloudflare Tunnel
- **Usable by agents** — every command speaks `--json`, and `minato skill install` drops in the Skill
- **Prebuilt or built** — pull an image, or point `build` at a Dockerfile
- **Your choice of virtualisation** — Docker and Apple Container behind one Runtime abstraction, switched with `[runtime] default`

## Status

**Every milestone is done except Firecracker, which needs a Linux host.** Creating a worktree starts its containers and
they answer on `*.localhost`. An untouched environment stops itself and comes
back on the next request. Every service receives the others' URLs as
`MINATO_URL_<SERVICE>`.

```console
$ minato init
$ minato new feature/user-auth
  ✓ creating worktree feature/user-auth
  ✓ preparing the network
  ✓ pulling image busybox:latest
  ✓ starting web
  ✓ waiting for web

myapp / feature-user-auth  (feature/user-auth)
  /path/to/myapp.wt/feature-user-auth

  web   ready     https://web.feature-user-auth.myapp.localhost
```

The standard ports (80 and 443) need a one-off privileged setup.
`minato doctor` says where things stand and `minato setup` prints the commands
(**it never runs sudo for you** — read them, then run them yourself).

```console
$ minato setup
1. let launchd hold 80/443/53 (the daemon itself stays non-root)
2. point *.localhost at Minato's DNS
3. trust the local CA, so HTTPS stops warning
```

To skip all of that, name unprivileged ports and no permissions are needed:

```console
$ MINATO_HTTP_PORT=8080 MINATO_HTTPS_PORT=8443 MINATO_DNS_PORT=15353 minato daemon start
```

To share an environment with a phone or an outside reviewer, put it behind a
Cloudflare Tunnel:

```console
$ cloudflared tunnel login              # opens a browser; Minato will not run it for you
$ minato tunnel enable --domain example.com --public
$ minato status
  web   ready     https://web.feature-demo.myapp.localhost

  shared over the tunnel:
  web   https://web-feature-demo.myapp.example.com
```

`--public` is required and means what it says. Minato cannot apply a Cloudflare
Access policy — that needs the API rather than the `cloudflared` CLI — so it
will not put an environment on the internet without being asked. Put an Access
policy in front of the hostname yourself.

Scale-to-zero works through the tunnel too: a reviewer's first request wakes a
stopped environment, same as a local one.

## Runtimes

`[runtime] default` in `minato.toml` picks the backend; `minato doctor` reports
the one your project uses and any others that are reachable.

```toml
[runtime]
default = "apple"   # or "docker"
```

Apple Container needs macOS 26 or later and `container system start`. Two
differences to know about, both forced by the platform: services reach each
other through `MINATO_HOST_<SERVICE>`, which carries the peer's IP because
Apple Container has no container-to-container DNS, so a service must declare
`depends_on` to be sure its peer is up first; and every container shares the
default network, since a container can only join one and a per-workspace
network would cut off `scope = "project"` services.

Firecracker is not implemented. It needs KVM and cannot run on macOS.

## Documentation

<https://minato.1024.works> — in English and Japanese, covering installation,
configuration, everyday use, the runtimes, tunnels, tutorials, and a CLI and
`minato.toml` reference. The source is in [`docs/`](docs/).

```console
$ cd docs && pnpm install && pnpm dev
```

- Design notes: [docs/DESIGN.md](docs/DESIGN.md)

## Roadmap

| | |
| --- | --- |
| M0 ✅ | Docker and Apple Container runtimes, `new` / `up` / `down` / `rm` / `ls` / `status` / `url` |
| M1 ✅ | DNS, reverse proxy, TLS, `doctor` / `setup`, launchd socket activation |
| M2 ✅ | Scale-to-zero: health checks, idle stop, on-demand start |
| M3 ✅ | Environment variables: three-layer merge, secret references, injected URLs |
| M4 ✅ | Cloudflare Tunnel: one named tunnel, scale-to-zero through it |
| M5 ✅ | Skills, `logs` / `exec` |
| M6 ✅ | GUI: GPUI, living in the menu bar |
| M7 ✅ | Apple Container verified on real hardware; Firecracker needs a Linux host |

## GUI

```console
$ minato-desktop
```

Lives in the menu bar and shows each workspace's state, URLs and logs. Click a
URL to open it, or copy it.

## Layout

A Cargo workspace monorepo.

```
crates/    libraries   core / api / client / runtime / proxy / dns / tunnel
apps/      binaries    daemon (minatod) / cli (minato) / desktop (GUI)
skills/    the Skill for agents
xtask/     build tasks
```

## Development

```console
$ cargo build --workspace
$ cargo test --workspace
```

Running it needs a container runtime. Reaching the Docker API is enough — the
`docker` CLI itself is not required, and OrbStack, Docker Desktop and colima
all work.

```console
$ export PATH="$PWD/target/debug:$PATH"
$ minato daemon status
$ minato doctor
```

`MINATO_HOME` (default `~/.minato`) holds the daemon's socket, state, logs and
CA. A Unix socket path has a length limit, so it cannot live somewhere deep.

The listening ports come from `MINATO_HTTP_PORT`, `MINATO_HTTPS_PORT` and
`MINATO_DNS_PORT`.

## License

MIT. See [LICENSE](LICENSE).

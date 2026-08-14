# Runtimes

Kobune runs containers through a backend you choose per project.

```toml
[runtime]
default = "docker"   # or "apple"
```

`kobune doctor` reports the one your project uses, and any others it can reach:

```console
$ kobune doctor
│ …
│ ✓  container runtime            apple 1.2.1
│ ✓  Docker (available)           docker 29.4.0
│ …
```

## Docker

The default, and the better-supported one. Kobune talks to the Docker API
directly with `bollard` and never shells out to the `docker` CLI, so only the
API has to be reachable — Docker Desktop, OrbStack and colima all work.

Ports are forwarded to a dynamically chosen port on `127.0.0.1`. Never
`0.0.0.0`, which would put your development environment in front of everyone
else on the network.

Service names resolve through network aliases, so `db:5432` works from inside
any container in the same workspace.

The workspace's own hostnames are added to each container as well, pointed at
`host-gateway` — Docker's name for the host — so `https://api.myapp.localhost`
reaches the proxy from inside a container exactly as it does from the browser.

## Apple Container

Needs **macOS 26 or later** and the service running:

```console
$ container system start
```

Each container gets its own IP on a `192.168.x.x` network, so nothing is
published to the host and port collisions cannot happen. The proxy forwards
straight to the container.

There are three differences worth knowing about before you choose it.

### No name resolution between containers

Apple Container has no aliases and no container-to-container DNS. A container's
nameserver is its network gateway, which answers NXDOMAIN for every container
name. `db:5432` does not work.

Kobune injects the peer's **IP address** instead:

```
KOBUNE_HOST_DB = 192.168.64.7
```

So write this:

```js
const db = process.env.KOBUNE_HOST_DB ?? 'db'
```

::: warning depends_on matters here
The address is read when the service starts, so a peer that is not running yet
contributes no variable at all. **Declare `depends_on`** and Kobune starts them
in the right order.

An unset variable is deliberate. A hostname that never resolves would send you
looking for a DNS problem that does not exist; a missing variable points at the
ordering.
:::

### Service URLs go through /etc/hosts

There is no `--add-host` here, so the file that flag writes is generated and
mounted at `/etc/hosts` instead. The workspace's hostnames are pointed at the
network's gateway, which is the host: a container reaches the proxy there and
nowhere else, since it cannot see the host's loopback.

**The proxy has to be listening on that address**, and on 80 and 443 only
launchd can put it there. `kobune setup` writes that socket into the plist, so
a machine set up before Apple Container was installed needs `kobune setup` run
again. `kobune doctor` says so when it applies:

```console
$ kobune doctor
✗ reachable from containers  the proxy is not listening on 192.168.64.1, …
```

### Everything shares one network

A container can join exactly one network here, and there is no `network
connect`. Per-workspace networks would leave a `scope = "project"` service
attached to whichever worktree happened to start it, and unreachable from all
the others — which is the one thing that scope exists to prevent.

So every container sits on the default network. Worktrees are not isolated from
each other at the network level. For local development on one machine, by one
person, that is an acceptable trade; it is worth knowing if you were counting
on the isolation.

### Also

- **Named volumes** do not exist. Kobune maps them to bind mounts under
  `~/.kobune/volumes/<project>/`, which gives the same persistence.
- `kobune doctor` says `container system start` rather than "start Docker
  Desktop" when this is your runtime.

## Which to pick

Docker, unless you have a reason. It has network aliases, real named volumes,
and per-workspace isolation.

Apple Container is worth it for a lighter-weight VM per container with no
Docker Desktop running, if your services reach each other through
`KOBUNE_HOST_*` and you do not need worktrees isolated from one another.

## Firecracker

Not implemented. It needs KVM and cannot run on macOS at all, so there is
nowhere to develop it here. The `Runtime` trait exists to absorb exactly this
kind of difference, and the Apple Container work confirmed it can — a backend
returns the address the proxy should forward to, and the proxy never learns
which one produced it.

## Switching

Change the line and restart:

```console
$ kobune down --all
$ # edit kobune.toml
$ kobune up
```

Containers do not migrate. The old runtime's containers stay until you remove
them, and Kobune only manages what carries its own labels.

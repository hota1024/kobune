# Runtimes

Minato runs containers through a backend you choose per project.

```toml
[runtime]
default = "docker"   # or "apple"
```

`minato doctor` reports the one your project uses, and any others it can reach:

```console
$ minato doctor
  ✓ container runtime             apple 1.2.1
  ✓ Docker (available)            docker 29.4.0
```

## Docker

The default, and the better-supported one. Minato talks to the Docker API
directly with `bollard` and never shells out to the `docker` CLI, so only the
API has to be reachable — Docker Desktop, OrbStack and colima all work.

Ports are forwarded to a dynamically chosen port on `127.0.0.1`. Never
`0.0.0.0`, which would put your development environment in front of everyone
else on the network.

Service names resolve through network aliases, so `db:5432` works from inside
any container in the same workspace.

## Apple Container

Needs **macOS 26 or later** and the service running:

```console
$ container system start
```

Each container gets its own IP on a `192.168.x.x` network, so nothing is
published to the host and port collisions cannot happen. The proxy forwards
straight to the container.

There are two differences you have to design around.

### No name resolution between containers

Apple Container has no aliases and no container-to-container DNS. A container's
nameserver is its network gateway, which answers NXDOMAIN for every container
name. `db:5432` does not work.

Minato injects the peer's **IP address** instead:

```
MINATO_HOST_DB = 192.168.64.7
```

So write this:

```js
const db = process.env.MINATO_HOST_DB ?? 'db'
```

::: warning depends_on matters here
The address is read when the service starts, so a peer that is not running yet
contributes no variable at all. **Declare `depends_on`** and Minato starts them
in the right order.

An unset variable is deliberate. A hostname that never resolves would send you
looking for a DNS problem that does not exist; a missing variable points at the
ordering.
:::

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

- **Named volumes** do not exist. Minato maps them to bind mounts under
  `~/.minato/volumes/<project>/`, which gives the same persistence.
- `minato doctor` says `container system start` rather than "start Docker
  Desktop" when this is your runtime.

## Which to pick

Docker, unless you have a reason. It has network aliases, real named volumes,
and per-workspace isolation.

Apple Container is worth it for a lighter-weight VM per container with no
Docker Desktop running, if your services reach each other through
`MINATO_HOST_*` and you do not need worktrees isolated from one another.

## Firecracker

Not implemented. It needs KVM and cannot run on macOS at all, so there is
nowhere to develop it here. The `Runtime` trait exists to absorb exactly this
kind of difference, and the Apple Container work confirmed it can — a backend
returns the address the proxy should forward to, and the proxy never learns
which one produced it.

## Switching

Change the line and restart:

```console
$ minato down --all
$ # edit minato.toml
$ minato up
```

Containers do not migrate. The old runtime's containers stay until you remove
them, and Minato only manages what carries its own labels.

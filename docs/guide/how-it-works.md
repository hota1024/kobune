# How it works

Useful when something behaves in a way the other pages do not explain.

## The pieces

```
  minato (CLI) ──────┐
  minato-desktop ────┼── Unix socket / JSON-RPC ──┐
  SKILL.md (agent) ──┘                            │
                                            ┌───────────┐
                                            │  minatod  │
                                            └─────┬─────┘
            ┌──────────────┬──────────────────┼──────────────┬─────────────┐
            ▼              ▼                  ▼              ▼             ▼
       DNS (:53)    Proxy (:80/:443)      Runtime      Env resolver     Tunnel
     *.localhost →   routes on Host    Docker / Apple   3 layers +    cloudflared
      127.0.0.1                                        secret refs
```

**The daemon's API is the product.** The CLI, the desktop app and the Skill are
equal clients of it, and none of them holds logic of its own. Anything one can
do, all of them can.

## Why a daemon

Four things need something resident: the proxy holding 80 and 443, DNS holding
53, the idle timer that scale-to-zero depends on, and the `cloudflared`
process. A CLI that exits cannot do any of them.

## A request, end to end

You open `https://web.feature-auth.myapp.localhost`.

1. **DNS.** macOS does not resolve `*.localhost` on its own — Chrome does, but
   `curl`, Safari and Node's fetch do not, and agents use curl. So the daemon
   runs a DNS server and `/etc/resolver/localhost` points at it. It answers
   127.0.0.1 for *every* name under the suffix, including ones with no
   environment: a name that fails to resolve tells you nothing, while one that
   reaches the proxy gets you a 404 that says which workspaces exist.

2. **TLS.** The proxy issues a certificate for whatever name SNI asks for, on
   the spot, from a local CA in `~/.minato/ca/`. A wildcard cannot help here:
   `*.localhost` does not cover `web.feature-auth.myapp.localhost`, and each
   new worktree invents a name at a new depth. You trust one CA and never think
   about it again.

3. **Routing.** The proxy looks the hostname up in a table the daemon keeps.
   A running service has an address; a stopped one is in the table without one,
   because "stopped" and "does not exist" have to be told apart. The first is
   woken; the second is a 404.

4. **Waking.** A stopped service is started. A browser gets a self-reloading
   page after about 1.5 seconds; everything else is held until ready, up to
   120 seconds. Returning 503 to an agent reads as "the server is broken".

5. **Forwarding.** The proxy connects to the address and copies bytes.
   WebSocket upgrades pass through — HMR depends on it, and HTTP/2 is
   deliberately not advertised because an upgrade is an HTTP/1.1 mechanism.

The `Host` header is **not** rewritten. Vite and friends check it against an
allowlist, so the app sees the same URL you opened.

## Where the truth lives

The daemon keeps **no runtime state in a file**. Whether a container is alive
and what address it got are read from the container's own labels
(`dev.minato.*`). Restart the daemon and one listing rebuilds everything.

The state file holds two things: which worktrees Minato manages, and the URL
label issued to each. Labels are persisted so that changing the naming rules
later never changes an existing workspace's URL.

There is no second copy of the state, so there is nothing to reconcile after a
crash.

## Naming

`feature/user-auth` → `feature-user-auth`:

1. Lowercase, and replace anything outside `[a-z0-9-]` with `-`
2. Collapse runs of `-`, trim from both ends
3. Over 63 characters, truncate and append a hash of the original
4. Append a hash if the result collides with an existing workspace

**A hash is also appended when anything but a separator was dropped.** `/`,
`_`, `-`, `.` and space are separators; anything else disappearing means
information was lost. Without this, `feature/デモ環境` and `feature/検証環境`
would both become `feature` and their URLs would collide. Non-ASCII branch
names really do get used.

## What the runtime abstraction hides

A backend's job is to start one service and say where to reach it:

```rust
pub struct RunningService {
    pub endpoint: SocketAddr,
}
```

Under Docker that is a forwarded `127.0.0.1:49312`. Under Apple Container it is
the container's own `192.168.64.3:3000`. The proxy never learns which. That one
return type is what let Apple Container be added without redesigning anything —
a type built around port forwarding would have had to be rebuilt.

## Privileged ports

Ports below 1024 cannot be bound without root. launchd binds 53, 80 and 443 as
root and hands the daemon the file descriptors; `UserName` in the plist means
the daemon itself runs as you. Containers and files it creates end up owned by
the right account.

Without that, the proxy takes 18080 and 18443 and the URLs carry the port. A
proxy on an awkward port beats no proxy: with none at all no URL is issued,
which also means no `MINATO_URL_<SERVICE>` reaches a container. The one time
it does not move is when the plist *is* installed — launchd holds 80 either
way then, so a refusal means its job needs starting rather than a different
port.

macOS does not use systemd's `LISTEN_FDS` convention, so this goes through
`launch_activate_socket()`, looking descriptors up by the names in the plist.

`localhost` in `SockNodeName` makes launchd open sockets on both `::1` and
`127.0.0.1`. Both matter: macOS resolves `*.localhost` to both, and clients
prefer IPv6. Listening only on IPv4 once sent traffic to an unrelated app that
happened to be on `[::1]`.

## Further

The full record, including the decisions that were reversed and why, is in
[`docs/DESIGN.md`](https://github.com/hota1024/minato/blob/main/docs/DESIGN.md).

# Sharing over Cloudflare Tunnel

A Cloudflare Tunnel makes an environment reachable from outside your machine —
a phone, a reviewer, a webhook that has to reach you.

::: danger This puts your environment on the internet
Minato cannot apply a Cloudflare Access policy: that needs Cloudflare's API,
and everything here goes through the `cloudflared` CLI. Since it cannot promise
a policy is in place, it will not expose anything without `--public`, and it
says so every time.

Put an Access policy in front of the hostname yourself.
:::

## What you need

- A Cloudflare account with a domain on it
- `cloudflared` installed (`brew install cloudflared`)

## Set it up

```console
$ cloudflared tunnel login
```

That opens a browser and waits, which is why Minato does not run it for you —
the same reason `minato setup` runs nothing where there is no terminal to
answer at. An unattended interactive prompt hangs an agent.

Everything after login, Minato does:

```console
$ minato tunnel enable --domain example.com --public
  ✓ starting the tunnel
╭ tunnel ─────────────────────────────────────────────────────────────────╮
│ running  *.example.com                                                  │
│                                                                         │
│ DNS  *.myapp.example.com                                                │
│                                                                         │
│ this environment is reachable from the internet.                        │
│ Minato cannot see whether a Cloudflare Access policy is in front of it. │
╰─────────────────────────────────────────────────────────────────────────╯
```

It creates the named tunnel, routes a wildcard DNS record for the project, and
starts `cloudflared`. All of it is idempotent, so running it again is fine.

## The URLs

```console
$ minato status
╭ myapp / feature-auth ──────────────────────────────────╮
│ feature/auth  /path/to/myapp.wt/feature-auth           │
│                                                        │
│ ● web  ready  https://web.feature-auth.myapp.localhost │
│                                                        │
│ shared over the tunnel:                                │
│ web  https://web-feature-auth.myapp.example.com        │
╰────────────────────────────────────────────────────────╯
```

The tunnel hostname joins service and workspace with a `-`, because tunnel
hostnames only reliably support one level of subdomain.

Services with `expose = false` get no tunnel hostname. A database cannot be
reached from outside even by guessing.

## Scale-to-zero still works

A reviewer's first request wakes a stopped environment exactly as a local one
does. They wait a second or two and get the page.

This falls out of how routing works: the tunnel hostname is registered in the
proxy's routing table beside the `.localhost` one, pointing at the same
service. Both are ordinary routes.

## Turning it off

```console
$ minato tunnel disable
╭ tunnel ─────────────────╮
│ disabled  *.example.com │
╰─────────────────────────╯
```

Stops `cloudflared` and drops the tunnel hostnames. The named tunnel and its
DNS records stay in Cloudflare — they cost nothing idle, and keeping them means
re-enabling does not need another login.

The domain is remembered, so next time:

```console
$ minato tunnel enable --public
```

## Checking on it

```console
$ minato tunnel status
$ minato doctor | grep -i tunnel
```

`status` runs nothing. If setup is incomplete it prints the commands that are
left.

| State | Meaning |
| --- | --- |
| `disabled` | Never set up, or turned off |
| `not installed` | No `cloudflared` |
| `needs login` | `cloudflared tunnel login` has not been run |
| `stopped` | Configured, not currently up |
| `running` | Carrying traffic |

The daemon brings a tunnel that was on back up when it restarts, so a link you
gave someone keeps working.

## How it is arranged

One named tunnel per machine carries every project, with a single ingress rule
sending the whole zone to the local proxy and letting the proxy route on Host.
One wildcard DNS record per project — `*.myapp.example.com` — means worktrees
come and go without touching DNS or reloading `cloudflared`.

The hop from `cloudflared` to the proxy is plain HTTP over loopback. TLS is
terminated at Cloudflare's edge, and `cloudflared` has no reason to trust your
local CA.

::: tip Verified against a stub
The hostname routing, the generated configuration, the CLI arguments and
scale-to-zero through the tunnel are all tested. What has not been run is a
real named tunnel against a real zone. If something behaves unexpectedly, check
whether your Cloudflare plan allows a wildcard DNS record.
:::

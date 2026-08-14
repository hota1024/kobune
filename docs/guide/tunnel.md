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
│ DNS  *.example.com                                                      │
│                                                                         │
│ ! *.example.com now points here.                                        │
│ ! Names with a record of their own are unaffected;                      │
│ ! any other name in the zone reaches this machine.                      │
│                                                                         │
│ this environment is reachable from the internet.                        │
│ Minato cannot see whether a Cloudflare Access policy is in front of it. │
╰─────────────────────────────────────────────────────────────────────────╯
```

It creates the named tunnel, routes one wildcard DNS record for the zone, and
starts `cloudflared`. All of it is idempotent, so running it again is fine.

`--domain` is the zone itself — `example.com`, not `dev.example.com`. A hostname
one level below the zone is covered by its Universal SSL certificate, and one
below that is not.

The record covers the whole zone, so it is worth knowing what that does and
does not claim. An explicit record wins over a wildcard, so anything already
published under the domain keeps answering as before. A name Minato does not
know reaches the local proxy and gets a 404. That is what the note above says,
and it appears on the first `enable` for a domain and not again.

If a `*` record was already in the zone, `enable` says that instead. Cloudflare
reports only that the name is taken, not what it points at, so Minato cannot
tell whether the record reaches this tunnel — and if it does not, every hostname
goes elsewhere while everything here still reports `running`. Check that record
in the dashboard before trusting the URLs.

## The URLs

```console
$ minato status
╭ myapp / feature-auth ──────────────────────────────────╮
│ feature/auth  /path/to/myapp.wt/feature-auth           │
│                                                        │
│ ● web  ready  https://web.feature-auth.myapp.localhost │
│                                                        │
│ shared over the tunnel:                                │
│ web  https://web-feature-auth-myapp.example.com        │
╰────────────────────────────────────────────────────────╯
```

The tunnel hostname joins service, workspace and project with `-` into a single
label, where the local URL uses a subdomain per part. That is for the
certificate: Cloudflare's Universal SSL covers first-level subdomains only, so
a deeper hostname is refused at the TLS handshake — with the tunnel up and
plain HTTP through it working, which makes it look like anything but a
certificate problem. One label stays inside what the free certificate covers.

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
One wildcard DNS record — `*.example.com` — means projects and worktrees come
and go without touching DNS or reloading `cloudflared`.

The hop from `cloudflared` to the proxy is plain HTTP over loopback. TLS is
terminated at Cloudflare's edge, and `cloudflared` has no reason to trust your
local CA.

::: tip Verified against a real zone
The hostname routing, the generated configuration, the CLI arguments and
scale-to-zero through the tunnel are all tested against a stub. Beyond that,
`enable` has been run against a real Cloudflare zone on a free plan: the
wildcard record is created, and the tunnel URL answers over https on the zone's
Universal SSL certificate with nothing bought and nothing configured by hand.
:::

# Sharing a preview

Put a branch's environment on the internet so a phone, a designer or a webhook
can reach it.

::: danger Read this first
A tunnel makes your development environment reachable by anyone with the URL.
Kobune **cannot** apply a Cloudflare Access policy — that needs Cloudflare's
API, and everything here goes through the `cloudflared` CLI — so it cannot
promise anything is guarding it.

Put an Access policy in front of the hostname yourself. Kobune will not expose
anything without `--public`, and it repeats the warning every time.
:::

You need a Cloudflare account with a domain on it.

## Install and log in

```console
$ brew install cloudflared
$ cloudflared tunnel login
```

That opens a browser. Kobune does not run it for you: an interactive prompt in
a daemon hangs an agent at a step it cannot answer, the same reason
`kobune setup` runs nothing where there is no terminal to answer at.

## Turn it on

```console
$ kobune tunnel enable --domain example.com --public
  ✓ starting the tunnel
╭ tunnel ─────────────────────────────────────────────────────────────────╮
│ running  *.example.com                                                  │
│                                                                         │
│ DNS  *.example.com                                                      │
│                                                                         │
│ this environment is reachable from the internet.                        │
│ Kobune cannot see whether a Cloudflare Access policy is in front of it. │
╰─────────────────────────────────────────────────────────────────────────╯
```

Behind that: a named tunnel created, one wildcard DNS record routed for the
zone, `cloudflared` started. All idempotent, so running it again is fine.

## The link

```console
$ kobune status -w feature-checkout
╭ myapp / feature-checkout ──────────────────────────────────╮
│ feature/checkout  /path/to/myapp.wt/feature-checkout       │
│                                                            │
│ ● web  ready  https://web.feature-checkout.myapp.localhost │
│                                                            │
│ shared over the tunnel:                                    │
│ web  https://web-feature-checkout-myapp.example.com        │
╰────────────────────────────────────────────────────────────╯
```

Send the second one. Service, workspace and project are joined with `-` into a
single label, because Cloudflare's Universal SSL covers first-level subdomains
only — a deeper hostname would be refused at the TLS handshake.

```console
$ kobune status -w feature-checkout --json \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["workspace"]["services"][0]["tunnel_url"])'
```

## What your reviewer gets

- **A stopped environment still works.** Their first request wakes it, taking
  a second or two, exactly as a local request does. The tunnel hostname is an
  ordinary route in the same table.
- **Only exposed services are reachable.** Anything with `expose = false` — the
  database — has no tunnel hostname and cannot be reached even by guessing.
- **A real certificate.** TLS terminates at Cloudflare's edge, so there is no
  warning and nothing to trust. Your local CA is not involved.

## Add Access

Kobune cannot do this part. In the Cloudflare dashboard, under Zero Trust →
Access → Applications, add a self-hosted application for the hostname you are
sharing — `web-feature-checkout-myapp.example.com` — and a policy: an email
domain, or a one-time PIN for someone outside your organisation.

Do it before sharing anything you would not put on a public web server.

**Per hostname, not `*.example.com`.** A tunnel hostname is one label, so
there is no `*.myapp.example.com` to scope an application to any more, and the
zone-wide pattern would put Access in front of everything else the domain
serves — including production hostnames that have nothing to do with Kobune.
If the zone is only ever used for this, `*.example.com` is the shorter way to
cover every worktree at once; on a zone that serves anything else it locks out
your own visitors.

## Turn it off

```console
$ kobune tunnel disable
╭ tunnel ─────────────────╮
│ disabled  *.example.com │
╰─────────────────────────╯
```

The tunnel hostnames stop routing immediately; local URLs are untouched. The
named tunnel and DNS records stay in Cloudflare, so re-enabling needs no login:

```console
$ kobune tunnel enable --public
```

## Across a restart

The daemon brings a tunnel that was on back up when it restarts, and rebuilds
the routing table with it. A link you sent someone keeps working after you
reboot.

## When it does not work

```console
$ kobune tunnel status
$ kobune doctor | grep -i tunnel
$ tail -f ~/.kobune/logs/kobuned.log   # cloudflared logs here too
```

| Symptom | Likely cause |
| --- | --- |
| `needs login` | `cloudflared tunnel login` has not been run |
| `not installed` | `brew install cloudflared` |
| `stopped` while enabled | `cloudflared` exited — check the daemon log |
| Cloudflare 1016 | The DNS record is missing. Re-run `tunnel enable` |
| A wildcard record is refused | Your Cloudflare plan may not allow one |

::: tip Verified against a stub
The routing, the generated configuration and the CLI arguments are all tested,
including scale-to-zero through a tunnel hostname. What has not been exercised
is a real named tunnel against a real zone.
:::

## Next

- [Sharing over Cloudflare Tunnel](../guide/tunnel) — how it is arranged, and
  why
- [Working with AI agents](../guide/agents) — including why an agent should
  never run `tunnel enable` itself

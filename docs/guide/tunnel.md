# Sharing over a tunnel

A tunnel makes an environment reachable from outside your machine — a phone, a
reviewer, a webhook that has to reach you.

::: danger This puts your environment on the internet
No tunnel Kobune drives puts authentication in front of an environment, so it
will not expose one without `--public`, and it says so every time.

What that means depends on which tunnel you pick. Run without `--public` and it
says which of the two situations you are in before it stops. Read the section
for yours below.
:::

## Which one

```console
$ kobune tunnel enable --provider quick --public
$ kobune tunnel enable --provider cloudflare --domain example.com --public
```

| | `quick` | `cloudflare` |
| --- | --- | --- |
| Account | none | a Cloudflare account with a domain on it |
| Setup | none | `cloudflared tunnel login`, once |
| The hostname | Cloudflare's, invented per service | yours, under your zone |
| How long a URL lasts | until the tunnel stops | for good |
| A worktree made later | not reachable until you enable again | reachable at once |
| Surviving a daemon restart | no | yes |
| Access control | none possible | a Cloudflare Access policy, applied by you |

`quick` is for showing someone something now. `cloudflare` is for a link that
still works tomorrow.

The choice is remembered, so later runs need neither flag:

```console
$ kobune tunnel enable --public
```

`cloudflared` is what carries both, so install it either way:

```console
$ brew install cloudflared
```

## The quick way

Nothing to set up. No account, no domain, no login.

```console
$ kobune tunnel enable --provider quick --public
  ✓ starting the tunnel
╭ tunnel ─────────────────────────────────────────────────────╮
│ running  quick                                              │
│                                                             │
│ ! these URLs are Cloudflare's and last only as long as      │
│ ! this tunnel: restarting gives out different ones.         │
│ ! 2 services published; anything made later                 │
│ ! needs `kobune tunnel enable` again to be reachable.       │
│                                                             │
│ ! this environment is reachable from the internet.          │
│ There is no access control: anyone with the URL reaches it. │
╰─────────────────────────────────────────────────────────────╯
```

```console
$ kobune status
╭ myapp / feature-auth ────────────────────────────────────╮
│ feature/auth  /path/to/myapp.wt/feature-auth             │
│                                                          │
│ ● web  ready  https://web.feature-auth.myapp.localhost   │
│ ● api  ready  https://api.feature-auth.myapp.localhost   │
│                                                          │
│ shared over the tunnel:                                  │
│ web  https://restless-mode-plans-guru.trycloudflare.com  │
│ api  https://chapter-vhs-hometown-mill.trycloudflare.com │
╰──────────────────────────────────────────────────────────╯
```

### What it cannot do

**A quick tunnel carries one hostname to one service**, so Kobune runs a
`cloudflared` per exposed service in the workspace you enabled it from. Three
services is three processes and three unrelated hostnames.

That is where every limit below comes from.

**It covers what existed when you ran it.** A worktree you create afterwards has
no hostname, and neither does a service you add to `kobune.toml`. Run
`kobune tunnel enable --public` again to publish them.

**The URLs do not survive.** They are Cloudflare's, handed out per connection,
and they stop existing when the tunnel does. Restarting gives out different
ones, so a link you sent someone in the morning is dead by the afternoon.

**The daemon does not bring one back.** Reconnecting would hand out hostnames
nobody has, since the links people are holding point at the old ones either
way. So after `kobune daemon restart` the tunnel reads as `disabled` rather
than reconnecting, and enabling again is a deliberate act.

**Nothing can be put in front of it.** The hostname is Cloudflare's, not yours,
so there is nothing to attach a Cloudflare Access policy to. Anyone with the URL
reaches the environment.

## A tunnel on your own domain

`cloudflare` runs a named tunnel on a zone you own. The hostnames are yours,
they keep working, and you can put an Access policy in front of them.

### What you need

- A Cloudflare account with a domain on it
- `cloudflared` installed (`brew install cloudflared`)

### Set it up

```console
$ cloudflared tunnel login
```

That opens a browser and waits, which is why Kobune does not run it for you —
the same reason `kobune setup` runs nothing where there is no terminal to
answer at. An unattended interactive prompt hangs an agent.

Everything after login, Kobune does:

```console
$ kobune tunnel enable --provider cloudflare --domain example.com --public
  ✓ starting the tunnel
╭ tunnel ───────────────────────────────────────────────────────╮
│ running  cloudflare  *.example.com                            │
│                                                               │
│ DNS  *.example.com                                            │
│                                                               │
│ ! *.example.com now points here.                              │
│ ! Names with a record of their own are unaffected;            │
│ ! any other name in the zone reaches this machine.            │
│                                                               │
│ ! this environment is reachable from the internet.            │
│ Kobune cannot see whether an access policy is in front of it. │
╰───────────────────────────────────────────────────────────────╯
```

It creates the named tunnel, routes one wildcard DNS record for the zone, and
starts `cloudflared`. All of it is idempotent, so running it again is fine.

Kobune cannot apply the Access policy itself: that needs Cloudflare's API, and
everything here goes through the `cloudflared` CLI. Since it cannot promise a
policy is in place, it says so on every run. Put one in front of the hostname
yourself.

### Naming the zone

`--domain` is the zone itself — `example.com`, not `dev.example.com`. A hostname
one level below the zone is covered by its Universal SSL certificate, and one
below that is not.

It also has to be **the zone your `cloudflared tunnel login` covers**. That
login writes a certificate scoped to one zone, and `cloudflared tunnel route
dns` takes a hostname outside it as a name *relative* to that zone: naming
`other.com` on a login for `example.com` creates `*.other.com.example.com`,
exits successfully, and leaves `*.other.com` never having existed. Kobune
checks the name resolves after routing it and says so when it does not,
because nothing else about that state looks wrong — the tunnel is up, `status`
says `running`, and no URL ever arrives. Log in again to switch zones.

The record covers the whole zone, so it is worth knowing what that does and
does not claim. An explicit record wins over a wildcard, so anything already
published under the domain keeps answering as before. A name Kobune does not
know reaches the local proxy and gets a 404. That is what the note above says,
and it appears on the first `enable` for a domain and not again.

If a `*` record was already in the zone, `enable` says that instead. Cloudflare
reports only that the name is taken, not what it points at, so Kobune cannot
tell whether the record reaches this tunnel — and if it does not, every hostname
goes elsewhere while everything here still reports `running`. Check that record
in the dashboard before trusting the URLs.

## The URLs

```console
$ kobune status
╭ myapp / feature-auth ──────────────────────────────────╮
│ feature/auth  /path/to/myapp.wt/feature-auth           │
│                                                        │
│ ● web  ready  https://web.feature-auth.myapp.localhost │
│                                                        │
│ shared over the tunnel:                                │
│ web  https://web-feature-auth-myapp.example.com        │
╰────────────────────────────────────────────────────────╯
```

On your own zone the tunnel hostname joins service, workspace and project with
`-` into a single label, where the local URL uses a subdomain per part. That is
for the certificate: Cloudflare's Universal SSL covers first-level subdomains
only, so a deeper hostname is refused at the TLS handshake — with the tunnel up
and plain HTTP through it working, which makes it look like anything but a
certificate problem. One label stays inside what the free certificate covers.

A quick tunnel's hostnames are Cloudflare's own and follow no rule of Kobune's,
which is why they say nothing about the service they reach.

Services with `expose = false` get no tunnel hostname either way. A database
cannot be reached from outside even by guessing.

## Scale-to-zero still works

A reviewer's first request wakes a stopped environment exactly as a local one
does. They wait a second or two and get the page.

This falls out of how routing works: the tunnel hostname is registered in the
proxy's routing table beside the `.localhost` one, pointing at the same
service. Both are ordinary routes, whoever handed the name out.

## Turning it off

```console
$ kobune tunnel disable
╭ tunnel ─────────────────────────────╮
│ disabled  cloudflare  *.example.com │
│                                     │
│ DNS  *.example.com                  │
╰─────────────────────────────────────╯
```

Stops the tunnel and drops its hostnames from the routing table. A quick
tunnel's processes go with it and its hostnames stop existing; a named tunnel
and its DNS records stay in Cloudflare, where they cost nothing idle and mean
re-enabling does not need another login.

## Checking on it

```console
$ kobune tunnel status
$ kobune doctor | grep -i tunnel
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

`needs login` belongs to `cloudflare`; a quick tunnel asks nobody to log in and
goes from `not installed` straight to `running`.

The daemon brings a named tunnel that was on back up when it restarts, so a
link you gave someone keeps working. A quick tunnel it turns off instead, for
the reason above — leaving it `stopped` would be a red mark in `doctor` about
a state that is correct.

## How it is arranged

The two are arranged differently, and every difference in the table above comes
from this.

**`cloudflare` is one named tunnel per machine**, carrying every project, with a
single ingress rule sending the whole zone to the local proxy and letting the
proxy route on Host. One wildcard DNS record — `*.example.com` — means projects
and worktrees come and go without touching DNS or reloading `cloudflared`.

**`quick` is one `cloudflared` per service.** There is no zone, so no wildcard;
without a wildcard a hostname reaches exactly one origin; so each service needs
a connection and a hostname of its own. Nothing is registered anywhere, which is
what makes it need no account and what makes its URLs temporary.

The hop from `cloudflared` to the proxy is plain HTTP over loopback either way.
TLS is terminated at Cloudflare's edge, and `cloudflared` has no reason to trust
your local CA.

::: tip Verified against a real zone
The hostname routing, the generated configuration, the CLI arguments and
scale-to-zero through the tunnel are all tested against a stub. Beyond that,
`enable` has been run against a real Cloudflare zone on a free plan: the
wildcard record is created, and the tunnel URL answers over https on the zone's
Universal SSL certificate with nothing bought and nothing configured by hand.
:::

# Troubleshooting

Work through it in this order. Reaching for `docker` on a hunch is how state
ends up disagreeing.

```console
$ minato status      # what state is the service in?
$ minato logs web    # what does the app say?
$ minato doctor      # what does the environment say?
```

`minato doctor` prints a fix for every line that is not `✓`.

## Common symptoms

### `curl` exits with 60

The local CA is not trusted.

```console
$ minato doctor
│ …
│ !  local CA trust  not trusted; browsers and curl will warn over HTTPS
│
│ to fix:
│ ! local CA trust
│   sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/…
```

This is the most common first stumble. Beware that plain `curl -s` swallows the
error and looks like an empty response — use `-sS --fail-with-body`.

### The URL does not resolve

```console
$ minato doctor
│ …
│ ✗  DNS resolver (/etc/resolver/localhost)  not installed
│ …
```

macOS does not resolve `*.localhost` by itself. The fix is in the output; it
includes the right port for how your daemon is running.

### A 404 from the proxy

```
Minato: there is no environment behind `web.feat-1.myapp.localhost`.
Run `minato ls` to see which workspaces are up.
```

The hostname does not match a registered service. Usually a typo, a stale URL
after a rename, or `expose = false`. Get it again with `minato url`.

### A 502

The service is registered but not answering. It started and then fell over, or
it is listening on a different port than `minato.toml` says.

```console
$ minato logs web -n 50
$ minato status          # is the state ready, or failed?
```

Check that `port` matches what the app actually binds, and that it binds
`0.0.0.0` rather than `127.0.0.1` — a server bound to loopback inside a
container is unreachable from outside it.

### Startup never finishes

```console
$ minato logs web -f
```

Minato waits 15 seconds for readiness, then carries on and warns. A first start
that compiles or installs dependencies takes longer than that; the container is
still coming up.

A `health` check makes this more accurate. Without one, readiness only means a
TCP connection succeeded.

### A configuration change did nothing

Containers that are already running do not pick up changes.

```console
$ minato down && minato up
```

True for `minato.toml` and for environment variables alike.

### Nothing works after a reboot

```console
$ minato daemon status
$ minato doctor
```

Without the LaunchDaemon installed, the daemon does not come back on its own.
`minato setup` offers to install it.

### The LaunchDaemon is installed, but its job never runs

```console
$ minato doctor
│ !  launchd socket activation  inactive, though launchd has the LaunchDaemon
```

A daemon started any other way owns the Unix socket, so launchd's own job finds
it taken and stands down — and a clean exit is not restarted. It is that first
daemon still holding the fallback ports, not a setup that failed.

```console
$ minato daemon restart
```

Stopping hands the socket back, and starting reaches for :80 — which is
launchd's to answer — so what comes up is launchd's job, holding 80, 443 and 53.
No root is needed for that; a `launchctl kickstart` would want it. `minato
doctor` and `minato setup` both name this same command.

Stopping alone would work eventually, since the next request to arrive wakes the
job, but it leaves the machine with no daemon in the meantime and `minato daemon
status` reporting it stopped.

The restart says so when that does not work: reaching :80 found nothing to wake
— something else holds the port, or the job's socket never bound — and the start
fell through to a daemon of its own. It exits non-zero and names what is left,
so neither you nor a script has to run `minato doctor` to find out.

```console
$ sudo launchctl kickstart -k system/dev.minato.daemon
```

**Installing it again is not the fix**, and `minato setup` no longer offers to:
launchd answers a second `bootstrap` of a label it already has with `Bootstrap
failed: 5: Input/output error`.

### launchd's job is for a different `MINATO_HOME`

```console
$ minato doctor
│ !  launchd socket activation  inactive: launchd's job serves MINATO_HOME=/Users/hotaka/.minato, and this daemon runs under /tmp/minato-elsewhere
```

The plist carries the home it was installed for, and this shell is using another
one. launchd holds 80, 443 and 53 for the job it has, that job serves the other
home, and nothing run from here takes them away from it.

```console
$ minato daemon restart
✗ error: started a daemon outside launchd, so 80 and 443 are out and no URL will answer
  hint: launchd's job serves MINATO_HOME=/Users/hotaka/.minato, so those ports are held for a daemon that is not this one. Point MINATO_HOME there to reach it, or keep the ports this daemon fell back to
```

The other two commands are no better. A `launchctl kickstart` starts that same
job again, for that same home, and `minato setup` offers no launchd step here at
all — a second `bootstrap` of a label launchd already has comes back as
`Input/output error`, so it says what the state is and leaves the rest alone.

Point `MINATO_HOME` at the home the job serves to reach the daemon those ports
belong to. Otherwise this is a second instance, deliberately, and it keeps the
fallback ports every URL then carries.

### "the Unix socket path is too long"

`MINATO_HOME` is somewhere deep. A socket path is limited to about 100 bytes.
Point it somewhere shorter — the default `~/.minato` is fine.

### Requests reach the wrong application

```console
$ minato doctor
│ …
│ ✗  listening addresses  the HTTPS proxy could not hold [::1]. *.localhost
│                         resolves to both families and clients prefer IPv6,
│                         so requests to that address reach another process
│ …
```

Something else holds one of the loopback addresses. Since `*.localhost`
resolves to both `::1` and `127.0.0.1` and clients prefer IPv6, holding only
one sends traffic somewhere else. Stop the other process, or move Minato with
`MINATO_HTTP_PORT`.

**The two proxies are reported separately** because they bind separately: HTTP
can hold both families while HTTPS has lost one, and it is the named one that
needs looking at.

## Apple Container

### `MINATO_HOST_<SERVICE>` is unset

The peer was not running when this service started. Add `depends_on` so it
starts first.

The variable is deliberately left unset rather than pointing at a hostname —
Apple Container has no container-to-container DNS, so a name would never
resolve and you would go looking for the wrong problem. See
[Runtimes](./runtimes).

### `container system status` says it is not running

```console
$ container system start
```

Minato does not start it for you, the same way it does not start Docker.

## Looking deeper

```console
$ tail -f ~/.minato/logs/minatod.log
$ MINATO_LOG=debug minatod          # in the foreground
```

If you do end up inspecting containers directly, only read:

```console
$ docker ps --filter label=dev.minato.managed=1
$ container ls --all
```

Everything Minato manages carries `dev.minato.*` labels, and those labels are
the source of truth. Changing containers behind its back is what makes the two
disagree.

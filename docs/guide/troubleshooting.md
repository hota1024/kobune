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
  ! local CA trust    not trusted; browsers and curl will warn over HTTPS
    sudo security add-trusted-cert -d -r trustRoot \
      -k /Library/Keychains/System.keychain ~/.minato/ca/minato-ca.crt
```

This is the most common first stumble. Beware that plain `curl -s` swallows the
error and looks like an empty response — use `-sS --fail-with-body`.

### The URL does not resolve

```console
$ minato doctor
  ✗ DNS resolver (/etc/resolver/localhost)   not installed
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
`minato setup` prints how to install it.

### "the Unix socket path is too long"

`MINATO_HOME` is somewhere deep. A socket path is limited to about 100 bytes.
Point it somewhere shorter — the default `~/.minato` is fine.

### Requests reach the wrong application

```console
$ minato doctor
  ✗ listening addresses   [::1] could not be held. *.localhost resolves to
                          both, so requests to that address reach another
                          process
```

Something else holds one of the loopback addresses. Since `*.localhost`
resolves to both `::1` and `127.0.0.1` and clients prefer IPv6, holding only
one sends traffic somewhere else. Stop the other process, or move Minato with
`MINATO_HTTP_PORT`.

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

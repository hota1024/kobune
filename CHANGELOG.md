# Changelog

Notable changes, in the format of [Keep a Changelog](https://keepachangelog.com/1.1.0/).

**Nothing has been released.** `nightly` is a rolling build of `main` and
carries no version, so everything below is unreleased by definition. What this
file is for is the moment that stops being true.

Started on 2026-08-12. For anything before the entries below, `git log` is the
record — backfilling it would mean writing down what commits already say,
less accurately.

## Unreleased

### Security

- The local CA is narrowed to `localhost` with an X.509 name constraint, so a
  key that escaped could sign nothing anyone could be fooled by. `minato setup` puts this certificate in the system trust store, and
  without the constraint the key behind it signed any host at all. A CA
  generated before this is left alone rather than replaced — swapping a
  trusted certificate breaks every URL until somebody notices — and
  `minato doctor` reports it
- The daemon's control socket is the owner's alone. `MINATO_HOME` is `0700`
  and the socket `0600`, narrowed on every start rather than only at
  creation, and the uid on the other end is checked on connect. The socket
  asks its callers for nothing, and `minato exec` prints the secrets resolved
  from 1Password and the Keychain — so who can reach it was the whole of the
  access control, and it was whatever the umask allowed (#51)

### Added

- A container can verify Minato's own HTTPS URLs. The CA is mounted read-only
  and named as `MINATO_CA_FILE`, so a service reaching another over
  `MINATO_URL_<SERVICE>` need not turn verification off
- A service URL resolves from inside a container as well as from the browser,
  so one hostname works for both halves of an application and cookies and CORS
  need know about only one
- `MINATO_HOSTNAME_<SERVICE>` — the host with no scheme or port, which is what
  a CORS origin, `allowedDevOrigins` and a cookie domain want (#40)
- `env_file`, which writes a service's settled environment into the worktree
  before it starts, for tools that read a file rather than their own
  environment: `wrangler dev`, Vite, dotenvx. Secrets are left out (#38)
- `${ANOTHER_KEY}` in an environment value, so `MINATO_URL_API` can be handed
  to an application under the name it already reads (#37)
- `minato logs` is an interactive terminal where a service has `tty = true`,
  so a task runner's own interface works (#33)

### Fixed

- Waking a service starts what it `depends_on`. A request is the only thing
  that wakes a service and only an exposed one has a URL for a request to
  name, so an unexposed dependency was left stopped behind whatever needed it.
  The same edges are now read backwards when stopping: an `expose = false`
  service follows the exposed ones that depend on it, where before it stayed
  up for as long as the daemon did (#49)
- `env_file` is written only for the services being started. `minato up web`
  used to fail over `api`'s, and `minato exec` left files behind as a side
  effect of running a command (#48)
- `minato env ls` lists when a value will not settle, marking that value and
  saying why, rather than refusing the whole listing — it is the tool for
  finding the one at fault (#44)
- A service is not called ready before the application inside it answers (#32)

### Changed

- Relicensed from MIT to Apache-2.0. Everything published before it went out
  under MIT, and that grant stands (#41)

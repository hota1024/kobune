# Changelog

Notable changes, in the format of [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

**Nothing has been released.** `nightly` is a rolling build of `main` and
carries no version, so everything below is unreleased by definition. What this
file is for is the moment that stops being true.

Started on 2026-08-12. For anything before the entries below, `git log` is the
record — backfilling it would mean writing down what commits already say,
less accurately.

## Unreleased

### Security

- The local CA is narrowed to `localhost` with an X.509 name constraint, so a
  key that escaped could sign nothing anyone could be fooled by. `kobune setup` puts this certificate in the system trust store, and
  without the constraint the key behind it signed any host at all. A CA
  generated before this is left alone rather than replaced — swapping a
  trusted certificate breaks every URL until somebody notices — and
  `kobune doctor` reports it
- The daemon's control socket is the owner's alone. `KOBUNE_HOME` is `0700`
  and the socket `0600`, narrowed on every start rather than only at
  creation, and the uid on the other end is checked on connect. The socket
  asks its callers for nothing, and `kobune exec` prints the secrets resolved
  from 1Password and the Keychain — so who can reach it was the whole of the
  access control, and it was whatever the umask allowed (#51)
- `install.sh` stops rather than warning when it cannot verify a download.
  With no `sha256sum`, `shasum` or `openssl` it used to carry on, so the
  `curl … | sh` the README recommends had no integrity check at all and said
  so only in scrollback nobody reads. A checksum file that is not a checksum
  — an error page, a truncated download — is refused for the same reason
  (#53)

### Added

- `kobune url --qr` draws the URL as a QR code, for opening an environment on
  a phone. It uses the tunnel URL when there is one, since a `.localhost` name
  resolves through this machine's resolver and nowhere else, and says so when
  the local URL is all there is. Drawn black on white rather than in the
  terminal's own colours: a code in a dark theme's foreground is inverted, and
  an inverted code is one iOS' camera will not read

- The mouse and the trackpad reach a service you have `kobune logs -f`
  attached to, so turborepo's log pane scrolls under the pointer as it does
  when you run it yourself. A full-screen program asks for mouse reports once,
  in the first bytes it writes, and an attachment that arrives later missed
  the request entirely: the terminal sent nothing and the wheel did nothing,
  and the program's frames landed on the normal screen rather than the
  alternate one. What the program makes of its terminal is now watched from
  before the container starts — the modes only, not the picture, which the
  program redraws anyway — replayed to you when you attach and put back when
  you leave

- The daemon says which build it is: a `Ping` — and so `kobune daemon status` —
  now carries `0.1.0 (abc1234)` rather than the crate version alone, which is
  the same string in every nightly and could tell no two builds apart

- An update says what is left to run, worked out from the machine rather than
  stated: a daemon still on the previous build, a Skill in this repository that
  is not this build's, a LaunchDaemon written to an older shape. `kobune
  update` prints the one it can be sure of and carries it as `next` under
  `--json`; the build that lands prints the rest on its first run, which is
  what covers the updates `kobune update` never sees — `install.sh` again, a
  package manager, a build of your own. Once per build, on stderr, never under
  `--json`, and silent when there is nothing to do

- The documentation site serves itself to agents: `/llms.txt` indexes the
  pages, `/llms-full.txt` is all of them in one file, and each page has a
  `.md` beside it, so `/guide/installation.md` is `/guide/installation` as
  Markdown. English only — these files are not translated — and without the
  `/vX.Y/` snapshots, superseded documentation being the last thing to hand
  an agent

- `kobune init --from-compose` converts a `docker-compose.yml`, turning the
  entry cost from rewriting a working file into reviewing a generated one.
  Deliberately not a complete conversion: what has no equivalent is named per
  service, and what compose cannot express is left as a `TODO` beside the
  service it belongs to. Compose's `env_file` becomes `carry` — the key means
  the opposite in the two formats, and mapping it across would have
  overwritten the file it names

- `docs/AGENT-RUN.md` — the record of driving a real two-service task with
  nothing but the Skill, and the four places the instructions did not say
  enough. All four are fixed: verifying with `--cacert` rather than waiting on
  `sudo`, what to do after editing source, that there is no `kobune restart`,
  and that `state` in `--json` is an object rather than the string its example
  showed

- A container verifies Kobune's own HTTPS URLs without being told to. The CA is
  mounted read-only, named as `KOBUNE_CA_FILE`, and handed to Node as
  `NODE_EXTRA_CA_CERTS`, so a service reaching another over
  `KOBUNE_URL_<SERVICE>` need not turn verification off. Naming the file and
  leaving the wiring to the service left `SELF_SIGNED_CERT_IN_CHAIN` in
  projects that had the certificate mounted and unused (#68)
- A service URL resolves from inside a container as well as from the browser,
  so one hostname works for both halves of an application and cookies and CORS
  need know about only one
- `KOBUNE_HOSTNAME_<SERVICE>` — the host with no scheme or port, which is what
  a CORS origin, `allowedDevOrigins` and a cookie domain want (#40)
- `env_file`, which writes a service's settled environment into the worktree
  before it starts, for tools that read a file rather than their own
  environment: `wrangler dev`, Vite, dotenvx. Secrets are left out (#38)
- `${ANOTHER_KEY}` in an environment value, so `KOBUNE_URL_API` can be handed
  to an application under the name it already reads (#37)
- `kobune logs` is an interactive terminal where a service has `tty = true`,
  so a task runner's own interface works (#33)

### Fixed

- **The mouse and the alternate screen reach a Docker service after all.**
  The modes were to be found by reading the container's log back, and that
  can never work: Docker holds everything after the last newline until the
  process ends, and a program that draws by moving the cursor has no reason
  ever to end a line — so the one thing being looked for was the one thing
  the log would not give up, and every attachment got an empty preamble.
  Measured on a live container, which is where it had to be measured: it
  answers `starting` alone while it runs and the announcement as well once
  it exits, and `docker logs --follow` waits alongside it. The daemon now
  watches the terminal from before the container starts, which is what the
  Apple Container backend already did, and inherits its one limitation —
  a daemon that restarted since a container started has nothing to replay

- **`kobune daemon restart` calls it a restart only when one happened.** The
  stop gets five seconds to take, and a daemon that outlasts it is shaken hands
  with rather than replaced — which exited 0, so `kobune setup`'s wake step went
  on to write `/etc/resolver/localhost` for :53 with DNS still on the fallback
  port. The daemon that legitimately answers there, launchd's job woken by a
  request arriving in the gap, looks identical from the socket, so the uptime on
  the handshake is read before the stop and compared after it: the daemon that
  was replaced started during the wait, and the one that was not carries that
  wait on top of the uptime it already had. Still there after both rounds, it
  exits 1 and says how long what it met has been running (#105)

- **`kobune daemon start` and `restart` say whether launchd took the
  sockets.** A start that fell back to a daemon of its own leaves 80, 443 and
  53 with the job that did not come up, so no URL answers — and every caller
  reading the exit code was told it had worked. They exit 1 now, with a hint
  naming which of the states the machine is in, and `kobune setup`'s wake step
  reads that rather than asking launchd a second question of its own. Nothing
  else changes its exit code over this: `kobune up` and the rest did what they
  were asked, and keep printing the same wording as a notice (#103)

- **launchd's job serving a different `KOBUNE_HOME` is its own state.** It
  holds 80, 443 and 53 for a daemon that is not this one, and every command
  the other states take is wrong here — a restart falls back, a `kickstart`
  starts that same job again, and `kobune setup` would ask launchd to
  bootstrap a label it already has. `doctor` names the home the job serves and
  the one this daemon runs under, `setup` offers no launchd step and leaves
  the resolver on the port DNS is actually on, and the notice after a direct
  start says there is nothing to run (#102)

- **`kobune setup` no longer reports a wake that did not happen.** Its wake
  step is `kobune daemon restart`, which exits 0 whether launchd's job came up
  or a daemon started directly in its place — throttled after the stop,
  disabled, its program moved — so setup asks launchd rather than reading that
  exit code. Where the job is still not running it says so, marks the step
  failed, leaves the `sudo launchctl kickstart` that forces it as what is left
  to run, and exits non-zero. It also stops writing `/etc/resolver/localhost`
  for :53 on a machine whose DNS is still on the fallback port, which is the
  failure the step exists to prevent (#101)

- A daemon that started outside launchd names the step that has not been taken
  yet. Starting one already reaches for :80 to wake launchd's job, so
  `kobune daemon restart` was being offered as the way back from a machine
  where that had just failed. Where launchd has the job it now names the
  kickstart that forces it, and where it has only the plist — copied in, or
  with its `bootstrap` declined — it names `kobune setup`, since `kickstart`
  there has no service to name (#101)

- `kobune doctor` walks a port launchd is holding down one ladder rather than
  two: `kobune daemon restart` first, and the `sudo launchctl kickstart` that
  needs root only after it, which is what its launchd check and
  `docs/guide/troubleshooting.md` already said. It reads whether launchd has
  the job rather than whether the plist is on disk, so a machine that was
  never bootstrapped no longer has launchd blamed for a port some other
  process holds (#101)

- **`doctor` and `setup` give the same answer as everything else** for a
  daemon holding the socket launchd's job wants: `kobune daemon restart`.
  Both still said `kobune daemon stop` and then a `sudo launchctl kickstart`,
  and `setup` ran the pair with `all(...)` — so declining the password prompt
  ran only the stop and left the machine with no daemon, which is the harm
  the step exists to prevent. Restarting does both halves and needs no root

- `kobune daemon restart` survives a slow shutdown. It skips the handshake on
  the way out, deliberately, because the usual reason to restart is a daemon
  too old to talk to — but starting again does shake hands, so an old daemon
  still answering after the five-second wait came back as the protocol
  mismatch error, telling the user to run the command that had just failed.
  The wait is given a second run instead

- The step after an update reads `kobune daemon restart` on every machine,
  rather than `kobune daemon stop` where launchd holds the job. Stopping was
  once the only safe way back — restarting by hand started a daemon outside
  launchd, and 80 and 443 stayed with launchd — but starting one has gone
  through launchd since #17, so the two end at the same daemon. Stopping is
  the worse half of it: a clean exit is not restarted, so the daemon stays
  down until something arrives on a port, and `kobune daemon status` reports
  it stopped in the meantime. It was also the one answer to a daemon from
  another build that did not say `restart`, which is what the error a stale
  daemon produces has always said

- `kobune setup` says `kobune daemon restart` afterwards, in place of
  `kobune daemon stop` and the promise that "launchd starts it again". It
  does not: a clean exit is not restarted, and neither is the job launchd
  started the moment it was handed the plist, which exits cleanly too when it
  finds the socket already owned. Following it left the machine with no daemon
  until something arrived on a port — an odd thing to be left with by the
  command you ran to make the URLs work

- Ctrl-P Ctrl-Q detaches. It stopped passing keys on and then waited: the
  window watcher held a second sender for the channel whose closing *is* the
  message that somebody left, so the message was never sent and `kobune logs`
  sat there with nobody reading the terminal, needing Ctrl-C — which is the
  one key the sequence exists to avoid

- `kobune uninstall` takes the named volumes with it, and lists them before
  asking. A project volume is shared between worktrees and outlives all of
  them, so nothing on the `kobune rm` path ever removed one — a command that
  says it takes Kobune off the machine left behind storage only Kobune knew
  the name of. They are found by label rather than from the daemon's records,
  which also reaches the storage of a project whose repository has already
  been deleted. A project whose containers could not be taken down keeps its
  data *where keeping it is possible*: Apple Container's volumes are
  directories under `KOBUNE_HOME`, which the uninstaller removes as one, so
  there they are listed as going rather than promised to somebody as kept.
  Storage that could not be listed, or would not go, is named in the plan and
  in what the command reports at the end — a runtime that cannot be asked
  answers exactly as one holding nothing does, and the difference decides
  whether an uninstall that left volumes behind says so (#84)
- `kobune init --from-compose` rewrites a sibling's URL — compose's
  `http://api:8080` — into `${KOBUNE_URL_API}`. Carried across verbatim it
  bypasses the proxy, hands the application a different URL from the
  browser's, and does not resolve under Apple Container at all. Found by an
  agent that had never seen this codebase: it got this right writing the
  configuration by hand, where the converter did not
- The Skill tells an agent to look for a compose file before writing a
  configuration by hand. `--from-compose` had shipped without being mentioned
  in the one file an agent reads, so the first independent run of it never
  used the feature built for exactly its situation

- Waking a service starts what it `depends_on`. A request is the only thing
  that wakes a service and only an exposed one has a URL for a request to
  name, so an unexposed dependency was left stopped behind whatever needed it.
  The same edges are now read backwards when stopping: an `expose = false`
  service follows the exposed ones that depend on it, where before it stayed
  up for as long as the daemon did (#49)
- `env_file` is written only for the services being started. `kobune up web`
  used to fail over `api`'s, and `kobune exec` left files behind as a side
  effect of running a command (#48)
- `kobune env ls` lists when a value will not settle, marking that value and
  saying why, rather than refusing the whole listing — it is the tool for
  finding the one at fault (#44)
- A service is not called ready before the application inside it answers (#32)

### Changed

- **The project is called Kobune**, where it was Minato. Every name it answers
  to moved with it: the `minato` and `minatod` binaries, `minato.toml`,
  `~/.minato` and the `MINATO_*` environment variables, the launchd label
  `dev.minato.daemon`, the `dev.minato.*` container labels, the local CA's
  common name, and the Skill and its install path. Nothing reads the old names
  and nothing migrates. An existing installation has to be removed with the old
  `minato uninstall` before the new binary is installed — after this the
  launchd job, the trusted certificate, `~/.minato` and every running container
  carry names nothing can find, and the old daemon goes on holding ports 80,
  443 and 53. The documentation is still served from `minato.1024.works`; that
  moves separately
- **A tunnel hostname is one label**: `web-feat-1-myapp.example.com`, where it
  was `web-feat-1.myapp.example.com`. Cloudflare's Universal SSL covers the
  apex and first-level subdomains only, so the old two-level hostname had no
  certificate at the edge and every tunnel URL failed the TLS handshake —
  while plain HTTP through the same tunnel answered 200, which pointed away
  from the certificate. Nothing free covers the second level: Total TLS
  refuses hostnames used with Cloudflare Tunnel and subdomain zones are
  Enterprise-only, so the hostname moved instead. A link shared before this
  has to be shared again. The DNS record follows: one wildcard for the zone
  (`*.example.com`) rather than one per project

- **`tunnel enable` checks the wildcard resolves** before saying it points
  here, and reports it when it does not. `cloudflared tunnel route dns` takes
  a hostname outside the zone your login covers as a name relative to that
  zone — `--domain other.com` on a login for `example.com` creates
  `*.other.com.example.com` and exits 0 — so a tunnel can report `running`,
  print a DNS record, and be unreachable at every URL. Found by running it

- **`kobune url` with no service named lists every service**, where it used to
  print the first reachable one's URL on a bare line. Answering "which URL"
  with one of several is how a request ends up at the wrong service. Naming a
  service is unchanged — one bare line, for `curl "$(kobune url web)/"` — so
  the substitution to fix is the one that named nothing. `--json` returns an
  array of the same objects the single form returns

- A warning is yellow and carries `!`; red is kept for something that failed.
  `kobune tunnel enable --public` printed "this environment is reachable from
  the internet" in red on its way out of a command that had worked, and it was
  read as the command having failed

- **`state` in `--json` is a plain string**, and `reason` sits beside it
  rather than inside it, so `.state == "ready"` is true where it used to be
  compared against `{"state":"ready"}`. On the first command the Skill tells
  an agent to run. `PROTOCOL_VERSION` is 6; the daemon and the CLI ship
  together and say so on a mismatch

- `rcgen` 0.14. It removed the only public way to read a certificate's name
  constraints back, which is how `kobune doctor` tells a CA made under the
  `localhost` rule from one made before it, so that now reads the certificate
  directly. A CA generated by the older version still loads, keeps its bytes,
  and reports what it actually carries

- Relicensed from MIT to Apache-2.0. Everything published before it went out
  under MIT, and that grant stands (#41)

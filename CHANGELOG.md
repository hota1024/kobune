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
- `install.sh` stops rather than warning when it cannot verify a download.
  With no `sha256sum`, `shasum` or `openssl` it used to carry on, so the
  `curl … | sh` the README recommends had no integrity check at all and said
  so only in scrollback nobody reads. A checksum file that is not a checksum
  — an error page, a truncated download — is refused for the same reason
  (#53)

### Added

- `minato url --qr` draws the URL as a QR code, for opening an environment on
  a phone. It uses the tunnel URL when there is one, since a `.localhost` name
  resolves through this machine's resolver and nowhere else, and says so when
  the local URL is all there is. Drawn black on white rather than in the
  terminal's own colours: a code in a dark theme's foreground is inverted, and
  an inverted code is one iOS' camera will not read

- The mouse and the trackpad reach a service you have `minato logs -f`
  attached to, so turborepo's log pane scrolls under the pointer as it does
  when you run it yourself. A full-screen program asks for mouse reports once,
  in the first bytes it writes, and an attachment that arrives later missed
  the request entirely: the terminal sent nothing and the wheel did nothing,
  and the program's frames landed on the normal screen rather than the
  alternate one. What the program made of its terminal is now read back when
  you attach — the modes only, not the picture, which the program redraws
  anyway — and put back when you leave

- The daemon says which build it is: a `Ping` — and so `minato daemon status` —
  now carries `0.1.0 (abc1234)` rather than the crate version alone, which is
  the same string in every nightly and could tell no two builds apart

- An update says what is left to run, worked out from the machine rather than
  stated: a daemon still on the previous build, a Skill in this repository that
  is not this build's, a LaunchDaemon written to an older shape. `minato
  update` prints the one it can be sure of and carries it as `next` under
  `--json`; the build that lands prints the rest on its first run, which is
  what covers the updates `minato update` never sees — `install.sh` again, a
  package manager, a build of your own. Once per build, on stderr, never under
  `--json`, and silent when there is nothing to do

- The documentation site serves itself to agents: `/llms.txt` indexes the
  pages, `/llms-full.txt` is all of them in one file, and each page has a
  `.md` beside it, so `/guide/installation.md` is `/guide/installation` as
  Markdown. English only — these files are not translated — and without the
  `/vX.Y/` snapshots, superseded documentation being the last thing to hand
  an agent

- `minato init --from-compose` converts a `docker-compose.yml`, turning the
  entry cost from rewriting a working file into reviewing a generated one.
  Deliberately not a complete conversion: what has no equivalent is named per
  service, and what compose cannot express is left as a `TODO` beside the
  service it belongs to. Compose's `env_file` becomes `carry` — the key means
  the opposite in the two formats, and mapping it across would have
  overwritten the file it names

- `docs/AGENT-RUN.md` — the record of driving a real two-service task with
  nothing but the Skill, and the four places the instructions did not say
  enough. All four are fixed: verifying with `--cacert` rather than waiting on
  `sudo`, what to do after editing source, that there is no `minato restart`,
  and that `state` in `--json` is an object rather than the string its example
  showed

- A container verifies Minato's own HTTPS URLs without being told to. The CA is
  mounted read-only, named as `MINATO_CA_FILE`, and handed to Node as
  `NODE_EXTRA_CA_CERTS`, so a service reaching another over
  `MINATO_URL_<SERVICE>` need not turn verification off. Naming the file and
  leaving the wiring to the service left `SELF_SIGNED_CERT_IN_CHAIN` in
  projects that had the certificate mounted and unused (#68)
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

- The step after an update reads `minato daemon restart` on every machine,
  rather than `minato daemon stop` where launchd holds the job. Stopping was
  once the only safe way back — restarting by hand started a daemon outside
  launchd, and 80 and 443 stayed with launchd — but starting one has gone
  through launchd since #17, so the two end at the same daemon. Stopping is
  the worse half of it: a clean exit is not restarted, so the daemon stays
  down until something arrives on a port, and `minato daemon status` reports
  it stopped in the meantime. It was also the one answer to a daemon from
  another build that did not say `restart`, which is what the error a stale
  daemon produces has always said

- `minato setup` says `minato daemon restart` afterwards, in place of
  `minato daemon stop` and the promise that "launchd starts it again". It
  does not: a clean exit is not restarted, and neither is the job launchd
  started the moment it was handed the plist, which exits cleanly too when it
  finds the socket already owned. Following it left the machine with no daemon
  until something arrived on a port — an odd thing to be left with by the
  command you ran to make the URLs work

- Ctrl-P Ctrl-Q detaches. It stopped passing keys on and then waited: the
  window watcher held a second sender for the channel whose closing *is* the
  message that somebody left, so the message was never sent and `minato logs`
  sat there with nobody reading the terminal, needing Ctrl-C — which is the
  one key the sequence exists to avoid

- `minato uninstall` takes the named volumes with it, and lists them before
  asking. A project volume is shared between worktrees and outlives all of
  them, so nothing on the `minato rm` path ever removed one — a command that
  says it takes Minato off the machine left behind storage only Minato knew
  the name of. They are found by label rather than from the daemon's records,
  which also reaches the storage of a project whose repository has already
  been deleted. A project whose containers could not be taken down keeps its
  data *where keeping it is possible*: Apple Container's volumes are
  directories under `MINATO_HOME`, which the uninstaller removes as one, so
  there they are listed as going rather than promised to somebody as kept.
  Storage that could not be listed, or would not go, is named in the plan and
  in what the command reports at the end — a runtime that cannot be asked
  answers exactly as one holding nothing does, and the difference decides
  whether an uninstall that left volumes behind says so (#84)
- `minato init --from-compose` rewrites a sibling's URL — compose's
  `http://api:8080` — into `${MINATO_URL_API}`. Carried across verbatim it
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
- `env_file` is written only for the services being started. `minato up web`
  used to fail over `api`'s, and `minato exec` left files behind as a side
  effect of running a command (#48)
- `minato env ls` lists when a value will not settle, marking that value and
  saying why, rather than refusing the whole listing — it is the tool for
  finding the one at fault (#44)
- A service is not called ready before the application inside it answers (#32)

### Changed

- **`minato url` with no service named lists every service**, where it used to
  print the first reachable one's URL on a bare line. Answering "which URL"
  with one of several is how a request ends up at the wrong service. Naming a
  service is unchanged — one bare line, for `curl "$(minato url web)/"` — so
  the substitution to fix is the one that named nothing. `--json` returns an
  array of the same objects the single form returns

- A warning is yellow and carries `!`; red is kept for something that failed.
  `minato tunnel enable --public` printed "this environment is reachable from
  the internet" in red on its way out of a command that had worked, and it was
  read as the command having failed

- **`state` in `--json` is a plain string**, and `reason` sits beside it
  rather than inside it, so `.state == "ready"` is true where it used to be
  compared against `{"state":"ready"}`. On the first command the Skill tells
  an agent to run. `PROTOCOL_VERSION` is 6; the daemon and the CLI ship
  together and say so on a mismatch

- `rcgen` 0.14. It removed the only public way to read a certificate's name
  constraints back, which is how `minato doctor` tells a CA made under the
  `localhost` rule from one made before it, so that now reads the certificate
  directly. A CA generated by the older version still loads, keeps its bytes,
  and reports what it actually carries

- Relicensed from MIT to Apache-2.0. Everything published before it went out
  under MIT, and that grant stands (#41)

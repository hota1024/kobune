# Minato design notes

A development environment manager built around git worktrees. Agent-friendly
by design. The goal is that creating a git worktree is all it takes to have a
preview environment running.

## 1. The idea

**One worktree, one environment. That correspondence is the invariant.**

An environment appears with its worktree and goes with it. Knowing that much,
an agent never has to wonder which environment it is looking at or how to check
a change.

```
~/ghq/github.com/hota1024/myapp/            # main worktree   → myapp.localhost
~/ghq/github.com/hota1024/myapp.wt/feat-1/  # feat-1 worktree → feat-1.myapp.localhost
                                  /feat-2/  # feat-2 worktree → feat-2.myapp.localhost

https://web.feat-1.myapp.localhost   → feat-1's web container, :3000
https://api.feat-1.myapp.localhost   → feat-1's api container, :8080
```

What an agent gets comes down to three things.

1. `minato new feat-1` grows a branch and its environment together
2. `minato url web` hands back the URL to check
3. `minato logs` and `minato exec` see inside — without ever touching `docker`

## 2. Vocabulary

| Term | Meaning |
| --- | --- |
| **Project** | A git repository, identified by the main worktree's `origin` URL, or its absolute path when there is none |
| **Workspace** | The environment for one worktree. Named after the worktree, which is the branch name sanitised |
| **Service** | One process in a workspace (web, api, db, …) |
| **Runtime** | A virtualisation backend (Docker, Firecracker, Apple Container) |
| **Supervisor** | The component inside the daemon that owns a workspace's lifecycle |

The word "environment" is avoided: it collides with environment variables.

## 3. Architecture

```
  minato (CLI) ──────┐
  minato-desktop ────┼─── Unix socket / JSON-RPC ───┐
  SKILL.md (agent) ──┘                              │
        └── all of them through minato-client       ▼
                                            ┌───────────┐
                                            │  minatod  │
                                            │ Supervisor│
                                            └─────┬─────┘
              ┌──────────────┬───────────────┼──────────────┬──────────────┐
              ▼              ▼               ▼              ▼              ▼
         DNS (:53)     Proxy (:80/:443)   Runtime      Env resolver     Tunnel
      hickory-server   hyper + rustls    ┌──trait──┐   3-layer merge   cloudflared
      *.localhost →     routes on Host   │ Docker  │   + secret refs     ingress
        127.0.0.1                        │Firecracker│
                                         │ Apple   │
                                         └─────────┘
```

**There is a daemon.** The port ledger, the reverse proxy, DNS, the tunnel and
idle sweeping all need something to stay running.

### Principle: the daemon's API is the product

The CLI, the GUI and Skills are **equal clients** of the daemon, and nothing
more than surface. No logic lives on the client side.

This has to hold from M0. Letting CLI-specific concerns — formatting for
people, interactive confirmation, progress display — into the daemon's API
guarantees a reckoning when the GUI arrives. Concretely:

- A daemon response is always structured data. It never returns a string meant
  for a person
- Long-running work — starting, building — **returns an event stream**. The CLI
  turns it into progress lines; the GUI turns the same thing into a progress bar
- The daemon never prompts for confirmation. A destructive operation takes a
  `force` flag from the client

Having the event stream from M0 matters most. Bolted on later, `minato up`
would have been designed to block until completion, and the GUI could never
show a start in progress.

### The CLI draws rather than prints

Everything the CLI shows a person is a **ratatui widget** (`apps/cli/src/ui`).
A view turns one response into a panel of tables and lines; a surface draws it
into a buffer and writes that buffer out as ordinary scrollback lines.

Two things follow from that split, and they are the reason for it.

- **A pipe is not a terminal.** The same views drop their frame and their
  colour, and are given whatever width they ask for, so nothing that reaches a
  log, a `grep` or an agent is wrapped or truncated. `--json` is untouched
- **`minato` can grow a TUI without rewriting any of it.** The views never
  reach for the screen and do not know whether they are being printed once or
  repainted sixty times a second. A full-screen mode hands them a
  `ratatui::Frame` instead of a one-shot buffer

Long-running commands already use the second half of this: `up` and `new` hold
the bottom line in an inline viewport for what is happening now, and let
finished steps scroll into the history above it. That display is built from the
event stream and nothing else — which is the same requirement the GUI is held
to.

### Who may connect

The socket is the whole API and it asks for nothing — no token, no
handshake. That is the right shape for something a person's own CLI, GUI and
agent all speak, and it means **the only access control there is, is who can
reach the socket at all**.

Which matters more than it looks. `minato exec <service> -- env` prints the
values [§8's secrets](#secrets) resolved from 1Password and the Keychain, so
keeping a resolved secret out of every file buys nothing if another account on
the machine can ask for it.

Two answers, one behind the other.

- **`MINATO_HOME` is `0700` and the socket is `0600`.** The directory is the
  one that matters: a path nobody else may traverse cannot be reached, which
  also covers the instant between creating the socket and setting its mode.
  Narrowed on every daemon start rather than only at creation — an
  installation made before this rule would otherwise stay open for as long as
  it lives
- **The uid on the other end is checked on accept**, with `getpeereid` and,
  on Linux, `SO_PEERCRED`. This is what is left if `MINATO_HOME` names
  somewhere shared

A refused connection is dropped rather than answered. An error would say what
is here to somebody with no business knowing.

The CA certificate stays `0644` inside that directory ([§5](#proxy-and-tls)),
and is still mounted into containers: the runtime reads it as the user who
owns the directory, so the mode on the directory changes nothing there.

### Why a daemon

| Responsibility | Why it has to stay running |
| --- | --- |
| Proxy | :80 and :443 have to stay held |
| DNS | :53 has to stay held |
| Supervisor | Idle detection for scale-to-zero needs a clock |
| Port ledger | One source of truth against collisions between workspaces |
| Tunnel | Something has to own the cloudflared process |

## 4. Defining an environment: `minato.toml`

The schema is Minato's own and comes first. Docker is one backend among
several, and compose is an implementation detail generated internally. That way
Firecracker support does not mean redesigning the format.

It sits at the project root — the main worktree — and is committed.
Worktree-specific overrides go in `minato.local.toml`, which is gitignored.

```toml
[project]
name = "myapp"
# domain = "myapp.localhost"   # derived from name when left out

[runtime]
default = "docker"

[services.web]
build = "./web"                 # or image = "node:22"
port = 3000
command = "pnpm dev"
health = "http://localhost:3000/healthz"   # readiness, for scale-to-zero
idle_timeout = "30m"
env = { NODE_ENV = "development" }

[services.api]
build = "./api"
port = 8080
depends_on = ["db"]
health = "tcp://localhost:8080"

[services.db]
image = "postgres:16"
port = 5432
scope = "project"               # shared across worktrees (default is "workspace")
volumes = ["pgdata:/var/lib/postgresql/data"]
expose = false                  # no URL; internal traffic only
```

### Coming from compose

The schema being Minato's own is right for the long run and is the whole of
the entry cost: trying it means throwing away a working `docker-compose.yml`
and hand-writing a new format, for URLs. Most people stop there.

`minato init --from-compose` makes that a review rather than a rewrite. It is
deliberately **not** a complete conversion — compose is enormous and half of
it means nothing here — so every key lands in one of three places and none of
them is silence:

- **converted**: `image`, `build`, `ports`, `expose`, `command`,
  `environment`, `depends_on`, `volumes`, `healthcheck`, `working_dir`, `tty`
- **left as a `TODO` beside its service**: what compose cannot express —
  whether a database is `scope = "project"`, what `setup` should run
- **named in the report**: `restart`, `deploy`, `networks`, `profiles`,
  `logging` and the rest, per service

A generated file that looks finished and is not costs more than no conversion:
the failure arrives later, somewhere else, as a service behaving differently
from the one that ran yesterday.

**`env_file` means the opposite in the two formats**, and mapping it across
would have destroyed data. Compose *reads* the file into the environment;
Minato *writes* the settled environment out to it, so the first `up` would
overwrite the user's `.env`. It converts to `carry`, which is what compose's
`env_file` actually implies here: a file the worktree needs and git does not
bring.

`ports: ["3000:8000"]` takes the **container** side. Minato publishes on a
port it chooses; what it needs to know is where the app listens inside. And
compose's `expose` — reachable by other services, not by the host — is exactly
`expose = false`.

### The parts that matter

**`scope`**: `workspace`, the default, gives each worktree its own instance.
`project` shares one instance across every worktree of a project. A database
per worktree is painful for both seeding and resources, so the room to share
one is there from the start.

**`expose`**: true by default when there is a `port`. An internal service like a
database sets it to `false` and gets no URL.

**`build`**: a context to build instead of an image to pull. See
[§7's note on building](#building-rather-than-pulling).

**`health`**: what scale-to-zero rests on. Without it the proxy 502s a service
that has started but is not answering yet. Three forms are supported:
`http://`, `tcp://` and `cmd:`. The last runs inside the container, which is
the only way to tell a database that accepts connections from one that will
answer a query — `postgres` listens well before it has finished initialising.

**Resolving service names**: within a workspace, the runtime's network resolves
service names (`db:5432`). Across scopes — a workspace's api reaching a
project's db — the daemon sets up an alias.

## 5. Naming and routing

### How a hostname is built

```
{service}.{workspace}.{project}.localhost
```

The main worktree drops the workspace label: `{service}.{project}.localhost`.

### Sanitising

A branch name contains characters a DNS label cannot.

1. Lowercase, and replace anything outside `[a-z0-9-]` with `-`
   (`feature/user-auth` → `feature-user-auth`)
2. Collapse runs of `-` into one, and trim `-` from both ends
3. Over 63 characters, truncate to 55 and append the first 7 characters of the
   original's SHA-256, separated by `-`
4. Append the same hash when the result collides with an existing workspace

**A hash is also appended when anything but a separator was dropped.** `/`, `_`,
`-`, `.` and whitespace count as separators; anything else disappearing means
information was lost. If `feature/デモ環境` and `feature/検証環境` both become
`feature`, their URLs collide. Non-ASCII branch names really do get used, so
this cannot be flattened silently.

The result is persisted in the state store and read back rather than
recomputed, so changing the rules never changes an existing workspace's URL.

### DNS

macOS does not resolve `*.localhost` at the system level. Chrome resolves it to
127.0.0.1 on its own, but `curl`, Safari and Node's fetch do not. **Agents
check connectivity with curl**, so this is fatal.

The answer is a DNS server inside the daemon, plus `nameserver 127.0.0.1` in
`/etc/resolver/localhost`. `minato setup` walks through it — installing it needs
sudo, so it is shown and offered rather than simply run.

```
/etc/resolver/localhost:
  nameserver 127.0.0.1
  port 15353
```

**That `port` line is what keeps DNS out of root's hands.** It can run
unprivileged instead of holding :53.

On Linux `systemd-resolved` resolves `.localhost` already, so the DNS server is
optional — only a different TLD such as `.test` needs it.

**Resolution does not check whether a route exists.** An unknown hostname
resolves to 127.0.0.1 too, and the proxy 404s it. A DNS failure says only "that
name does not resolve"; reaching the proxy means the answer can say which
workspaces are running.

### Proxy and TLS

A hyper reverse proxy listens on :80 and :443 and routes on the Host header, or
SNI over HTTPS.

**WebSocket and SSE must get through.** Dev-server HMR depends on them, and
without it the whole thing is unusable. HTTP/2 is not advertised over ALPN: a
WebSocket upgrade is an HTTP/1.1 mechanism, and offering h2 breaks HMR.

**The Host header is not rewritten.** Vite and friends check Host against an
allowlist, so the app sees the same URL the browser opened.

#### Certificates are issued per SNI name, on demand (settled in M1)

A wildcard certificate covers one label. `*.localhost` cannot cover
`web.feat-1.myapp.localhost`, and every new worktree invents a name at a new
depth, so preparing certificates ahead of time never keeps up.

So there is one local CA in `~/.minato/ca/`, and it **issues a certificate for
whatever name SNI asks for, on the spot, and caches it**. The user only ever
has to trust that single CA.

When the CA is loaded, **the certificate on disk goes into the chain as-is**.
Re-signing it through rcgen produces different bytes every time — ECDSA
signatures are not deterministic — and what goes out would no longer match what
the user trusted.

#### The CA is narrowed to `localhost`

Issuing for whatever SNI asks is the right behaviour for the proxy and the
wrong power for the key behind it. `minato setup` puts this certificate in the
system trust store, so without a limit the key in `~/.minato/ca/` signs
`google.com` as readily as anything of Minato's, and the machine believes it.
`mkcert` asks for the same thing, which makes it usual rather than acceptable.

So the CA carries an X.509 `NameConstraints` extension permitting `localhost`
and nothing else. It is reserved by RFC 6761 and can never be a real public
name, so what a leaked key could sign is nothing anybody could be fooled by.

**No leading dot, and that is not a detail.** RFC 5280 §4.2.1.10 says a DNS
subtree is satisfied by the name itself *and* by anything with labels prepended
— so `localhost` already covers `web.feat-1.myapp.localhost`, which is the
whole requirement. `.localhost` is a different, non-standard form meaning
strictly below, and it excludes `localhost` itself, which
[the DNS server](#dns) answers for. The first version of this used the dot on
the belief that it was what made subdomains work; OpenSSL and rustls-webpki
both refuse `localhost` under it. Nothing caught that until a test asked a
verifier instead of asking rcgen's own parser — the same mistake in the same
shape as [§6's Apple Container fixtures](#what-running-apple-container-turned-up-m7).

**Only what Minato actually serves.** `.test` was in this list briefly, on the
strength of this document anticipating it — but nothing resolves it: the DNS
server serves `localhost` alone and `minato setup` installs only
`/etc/resolver/localhost`. Permitting a suffix that cannot resolve would have
`minato doctor` report a working setup that is not one. It comes back when the
resolver does.

Two consequences, and both are `minato doctor`'s to report rather than
anything's to fix silently.

- **A CA made before this has no constraint**, and is left alone. Replacing a
  certificate the user trusted would break every URL until they noticed and
  trusted the new one, which is a worse day than the one it prevents. The fix
  it prints stops trusting the old one *first*, while the file naming it is
  still on disk
- **`[project] domain` can name a suffix outside it**, and the browser error
  for that names neither Minato nor the constraint. The check says which domain
  and which suffixes — and does not offer regenerating the CA, because what a
  new one permits is compiled in and a replacement would be identical

The proxy still issues for a name outside the constraint rather than refusing
the handshake: the constraint is enforced by whoever verifies, which is what
X.509 is for, and a certificate error says far more than a dropped connection.
It logs a line saying so, once — SNI is unauthenticated, so one line per
distinct name is a way to write into the log for free.

#### Listen on both IPv4 and IPv6 (found in M1)

macOS resolves `*.localhost` to **both** `::1` and `127.0.0.1`, and clients
prefer IPv6. Listening only on IPv4 silently routes traffic to whatever is on
`[::1]` — during development that turned out to be an unrelated Node app.

The proxy listens on both loopback addresses. DNS needs only IPv4: the resolver
configuration names `127.0.0.1` outright, so there is no ambiguity.

### Privileged ports

On macOS an unprivileged process cannot bind below 1024. launchd's socket
activation covers it: launchd, as root, binds :53, :80 and :443 and hands the
file descriptors to the daemon. A `UserName` in the plist means **the daemon
itself runs as the user** — as root, the containers and files it creates would
end up owned by the wrong account.

**macOS does not use systemd's `LISTEN_FDS` convention.** Instead
`launch_activate_socket()` looks descriptors up by the name in the plist's
`Sockets`. It lives in `libSystem`, so it is declared through FFI.

With `localhost` in `SockNodeName`, launchd opens sockets on both `::1` and
`127.0.0.1` and hands over two descriptors, which covers clients that prefer
IPv6.

`KeepAlive` is set to `SuccessfulExit: false`. An unconditional `true` would
have launchd start the daemon again the moment `minato daemon stop` finished.

Started outside launchd — by hand during development, say — everything falls
back to an ordinary bind, and when 80 and 443 are refused the proxy takes
18080 and 18443 rather than binding nothing. `MINATO_HTTP_PORT` and friends
choose the ports instead, and a port named that way is used as given.

With the plist installed the proxy does **not** move: launchd holds 80 whether
or not its job is running, so a refusal there means the job needs waking, and
listening elsewhere would hide that.

On Linux this would be `CAP_NET_BIND_SERVICE` or systemd socket activation,
which is not implemented.

## 6. The Runtime abstraction

A runtime is defined as "something that starts one service and says where it
listens". Wiring the network and routing stays on Minato's side, which keeps
the abstraction from being dragged towards Docker's notions of compose and
networks.

```rust
#[async_trait]
pub trait Runtime: Send + Sync {
    fn id(&self) -> &'static str;

    /// Build images and rootfs; prepare networks and volumes
    async fn prepare(&self, ws: &WorkspaceSpec) -> Result<()>;

    async fn start(&self, svc: &ServiceSpec) -> Result<RunningService>;
    async fn stop(&self, id: &ServiceId) -> Result<()>;
    async fn destroy(&self, ws: &WorkspaceSpec) -> Result<()>;

    async fn exec(&self, id: &ServiceId, cmd: &[String]) -> Result<ExecResult>;
    async fn logs(&self, id: &ServiceId, opts: LogOptions) -> Result<BoxStream<'_, LogLine>>;
    async fn inspect(&self, id: &ServiceId) -> Result<ServiceState>;
}

pub struct RunningService {
    pub id: ServiceId,
    /// Where the proxy forwards. Under Docker, 127.0.0.1:<dynamic port>;
    /// under Firecracker, an IP:port on the VM's tap interface
    pub endpoint: SocketAddr,
}
```

Returning `endpoint: SocketAddr` is the crux. The proxy never learns which
runtime it is talking to.

### Where each backend stands

| Runtime | OS | Role |
| --- | --- | --- |
| Docker | macOS, Linux | The v0 default. Talks to the Docker API through `bollard`, never the compose CLI. Implemented in M0 |
| Apple Container | macOS 26+ | Drives the `container` CLI. Written in M0, made to work in M7 against 1.2.1 |
| Firecracker | **Linux only** | For density and stronger isolation. Does not run on macOS |

### The structural gap between Docker and Apple Container (found in M0)

Implementing both made concrete what the Runtime abstraction has to absorb.

| | Docker | Apple Container |
| --- | --- | --- |
| Interface | HTTP API (bollard) | CLI (`container`) |
| Ports | Forwarded dynamically to the host (`127.0.0.1:49312`) | **Each container gets its own IP** (`192.168.64.3:3000`); nothing is published |
| Filtering | Label filters, server-side | No filters. Fetch everything and narrow it here |
| Networks | Created freely | **macOS 26 and later only.** Before that, everything shares the default |
| Service names | Network aliases give `db:5432` | **No name resolution at all.** A peer's IP is injected instead |
| Named volumes | Native | No such concept. Mapped onto bind mounts under `~/.minato/volumes/` |
| Network membership | A container joins several | One only, and no `network connect` |

**This is where returning `RunningService::endpoint: SocketAddr` paid off.**
Docker returns a forwarded host port and Apple Container the container's own
IP, and neither the proxy nor the supervisor knows the difference. A type built
around port forwarding (`host_port: u16`) would have had to be rebuilt for
Apple Container.

Only service-name resolution resisted abstraction, hence `ServiceSpec::peers`.
Apple Container uses it to inject `MINATO_HOST_<SERVICE>` and tell the app how
to reach its neighbours. Docker ignores it.

### What running Apple Container turned up (M7)

M0 wrote this backend from the CLI's documentation and never ran it. Every
assumption that documentation supported turned out to be wrong, and the unit
tests passed throughout because they were written against the same source.

**The output shape was wrong, so nothing worked at all.** `container ls
--format json` returns `status` as an *object* — `{"state": "running",
"networks": [...]}` — not the bare string the docs showed, and addresses come
back as `ipv4Address` under it. Every record failed to deserialise, which means
every operation failed. The fixture in the tests is now captured from a real
container; a fixture nobody has seen the CLI produce is worth nothing.

**There is no container-to-container name resolution.** Not by alias, and not
by DNS: a container's nameserver is its network gateway, which answers NXDOMAIN
for every container name, on the default network as much as a custom one. The
`.test` domain M0 injected is a *host* resolver — it needs
`sudo container system dns create` and, once created, lets the host reach
containers rather than containers reach each other. So `MINATO_HOST_<SERVICE>`
carried a name that nothing inside the container could resolve. It now carries
the peer's IP address, read after the peer has started.

A peer that is not running yet contributes no variable at all. Failing on a
missing variable points at the ordering; a name that never resolves sends
someone hunting for a DNS problem that does not exist. `depends_on` is what
guarantees the peer is up first.

**Everything shares the default network**, even though macOS 26 can create one
per workspace. A container attaches to exactly one network — `--network` takes
a single value and there is no `network connect` — and containers on different
networks cannot reach each other. A shared service (`scope = "project"`) would
attach to whichever workspace started it and be unreachable from all the
others, which is precisely what that scope exists to prevent. Per-workspace
networks would buy isolation and nothing else, since there is no container DNS
to scope. Correctness for shared services is worth more than isolation between
worktrees the same person owns. Revisit if multi-network attachment arrives.

What does hold up is the part the abstraction was built for: `endpoint` comes
back as the container's own `192.168.64.x:port`, the proxy forwards to it
without knowing which runtime produced it, and scale-to-zero wakes an Apple
Container service in about two seconds.

### Tests that need a runtime

The lesson above is not "write better fixtures". A fixture is only ever as
good as somebody's belief about what the runtime returns, and the whole
failure was that the belief and the test came from one source. **So some
tests have to run against a real runtime**, and the Docker backend — as
many lines as the Apple one, and the default — had none.

`apps/daemon/tests/` holds them. Three decisions shape it.

**`#[ignore]`, not a feature flag or an environment variable.** `cargo test`
stays exactly as fast as it was and needs nothing installed;
`cargo test -- --ignored` runs them. CI gives them their own job, on ubuntu,
where Docker is already there. Not a required check to begin with — the
value is in the signal, and a container runtime in CI is a new way for a
pull request to be blocked by something that is nobody's fault.

**A missing Docker is reported and skipped, never quietly passed.** A suite
that does nothing reads as coverage, which is worse than not having one.

**The daemon is driven, not the CLI.** `apps/daemon/src/lib.rs` exists for
this: a binary crate cannot have integration tests, because `tests/` has
nothing to import. Tests build a `Supervisor` over an inert
[`Gateway`](#3-architecture) — waking and sweeping both go through the
routing table and never through a listener — and ask it for the same things
the proxy does. That leaves out the HTTP half, which `minato-proxy`'s own
end-to-end tests already cover against a stub; what it covers is the part
nothing else could.

The harness clears up after itself in `Drop`, through the `docker` CLI
rather than the runtime under test. The run that most needs cleaning up
after is the one that panicked half-way, and a cleanup that fails for the
same reason the test did is not one.

### Firecracker

Still unimplemented, and not for lack of interest: it needs KVM and cannot run
on macOS at all. Writing it here would mean several hundred lines that have
never executed a single instruction — which is the mistake M0 made with Apple
Container, at greater cost. It waits for a Linux host to develop against.

**Note**: Firecracker depends on KVM and does not run on macOS. Since
development happens on macOS, Firecracker support assumes either Minato running
on a Linux server — a remote host — or Apple Container and krunkit standing in
locally. Absorbing that gap is what the Runtime trait is for.

## 7. Starting strategy: scale-to-zero and on demand

Ten worktrees do not mean ten running environments. This is what sets Minato
apart, and what makes creating a worktree feel cheap.

### The state machine

```
Stopped ──(a request arrives)──> Starting ──(health OK)──> Ready
   ▲                                 │                       │
   └──(idle_timeout elapsed)── Idle <─┘ (health fails/times out)
                                 ▲                           │
                                 └────(still no access)──────┘
```

### A request is the only way in, so it has to carry the dependencies

A service is woken by a request naming it, which means **only a service with a
URL can be woken at all**. `expose = false` — what a database is — leaves one
with no name for a request to carry, and that one fact decides both halves of
this section.

**Waking starts the whole `depends_on` closure**, in the same waves `up`
uses (see §7, "Making starts faster"). Starting
the single service the host named would hand the app a dependency that is not
there, and no request would ever arrive to fix it. `up` had always done this;
the wake path was the one way in that did not, so it worked under `minato up`
and failed only once scale-to-zero had stopped something. Under Apple Container
it decides more than ordering: `MINATO_HOST_<SERVICE>` carries a peer's address
read after that peer has started, so a service woken on its own gets no
variable at all.

**Stopping reads the same edges backwards.** An internal service has no
last-access time of its own — nothing ever requests it — so the idle sweep, which
walks the routing table, never saw it and it stayed up for as long as the daemon
did. One database per worktree, running for ever, is the opposite of what makes
a worktree cheap to create. It now follows the exposed services that depend on
it: stopped ones are not using it, running ones are judged on their own last
access.

**With no exposed dependent it is left alone.** There is then no signal to stop
on and no way back up, and stopping it would be one-way. The configuration
reference says to name it in `depends_on` for exactly this reason.

### Waiting for a start

The first request waits somewhere between seconds and tens of seconds. What
happens next depends on the client.

| Client | Behaviour |
| --- | --- |
| A browser (`Accept: text/html`) | An immediate "starting" page, which waits for readiness over SSE and reloads itself |
| An API call, curl, an agent | Held until ready, then forwarded. 504 after 120 seconds |

Making an agent's `curl` wait is the right answer. A half-hearted error leads it
to the wrong conclusion — "the server is broken".

### Making starts faster

- Base images are built at project scope and shared across worktrees
- The only per-worktree difference is the bind-mounted source
- `node_modules` and the like live in a named volume per workspace, installed
  once
- `minato new` runs `prepare` ahead of time (`--no-warm` turns it off)

**Services start a wave at a time, not one at a time.** A start does not
return until the service answers or gives up waiting on it — up to 15
seconds each — so a `web`, an `api` and a `cache` that know nothing of each
other used to spend that wait three times over for no reason. `depends_on`
is a partial order, and the sequence it was being flattened into invented
constraints it never asked for.

So `startup_waves` groups services by depth in the dependency graph: wave 0
depends on nothing, wave *n* depends only on earlier waves. Everything in a
wave starts at once, and the next wave waits for it. Nothing within a wave
depends on anything else in it, by construction — which is the whole
argument for why this is safe, and what the tests pin. `setup` still runs
immediately before its own service, so a migration against `db` keeps the
ordering it needs. The same grouping drives the wake path, where the wait
saved is one somebody is sitting through.

**Not everything on that path overlaps, and the exceptions are the
interesting part.** Pulling images does, because a pull is a download and
the registry is somebody else's machine. Building them does not: a build
already saturates its cores and BuildKit parallelises within one, so two at
once move the work around rather than remove it. And `setup` does not,
which costs the first `up` after `minato new` and is deliberate — every
service mounts the same project-wide cache volume, so two setups at once
are two arbitrary commands writing into one directory. A package manager's
store is built for that; a `setup` is whatever somebody wrote, so the
documentation promises one at a time and the lock keeps the promise.

**Whether it is safe to overlap is the runtime's answer, not the
supervisor's** (`Runtime::starts_concurrently`, off unless a backend says
otherwise). Docker says yes: a peer is reached by name on the network, so
when it started makes no difference. Apple Container says no — it has no
such DNS and injects a peer's address at creation, read from the peer
running by then, so two services started together would each be handed
nothing for the other. There the sequence *is* the mechanism.

**A backend that says no is not regrouped at all**, which is subtler than it
looks. Flattening the waves gives a valid startup order, but not the same
one `startup_order` gives — bucketing by depth pulls every independent
service ahead of everything one level down, and a depth-first walk does not:

```
startup_order  db, api, cache, web, worker
flattened      db, cache, api, worker, web
```

Where a wave starts at once that difference is invisible. Where it does not,
it decides what a service is told: Apple Container injects
`MINATO_HOST_<PEER>` for every peer already running, and `peers` is every
other service in the workspace rather than only `depends_on`, so a service
reordered past a neighbour loses the variable naming it. Sequential backends
therefore keep the order they have always had, and the two orders being
different is pinned by a test rather than left to be rediscovered.

One consequence worth stating: two services starting at once share a
network, so `ensure_network` — which looks and then creates — had to become
one-caller-at-a-time. Docker does not refuse a name it already has; it makes
a second network with the same name and a different id, and the two services
end up unable to reach each other.

### Building rather than pulling (M0.5)

`build` names a context; the image is built rather than pulled. Three
decisions shape it.

**The context comes from the worktree, not the main checkout.** A branch that
edits its Dockerfile has to get the image that Dockerfile describes —
otherwise the environment does not match the branch, which is the invariant
this whole thing rests on. The path is resolved and refused if it escapes the
worktree; `build = "../.."` would otherwise hand the runtime a build context of
somebody's home directory.

**The tag carries a fingerprint of the inputs**, so an image is
`minato-{project}-{service}:{hash of Dockerfile + build args}`. This settles
two problems with one mechanism. Two worktrees whose Dockerfiles agree land on
the same tag and share one image, which is the "base images are built at
project scope" idea from earlier in this section, arrived at without a separate
scope. A worktree that changes anything lands on a different tag, so neither
overwrites the other. And "does this need building?" reduces to "does this tag
exist?" — no label to compare, no state to keep.

Skipping matters more than it first appears: waking a stopped service goes
through `prepare`, so without the skip a Docker build would sit in the path of
an incoming request.

**A file the Dockerfile copies in is not covered.** Finding those means parsing
the Dockerfile, and a half-correct answer is worse than a stated limitation.
`minato up --build` forces a rebuild. `docker compose` draws the line in the
same place.

#### A running container can be stale too (found while building this)

`up` left a running container alone, on the grounds that starting something
already started is a no-op. With `build` that became wrong: the new image was
built, the old container kept serving, and the output said `✓ building` and
`- starting (already running)` — both true, and together completely
misleading.

A container is now recreated when its image does not match the spec, running or
not. The fingerprint tag is what makes that detectable at all; with a mutable
`:latest` there would have been nothing to compare.

### The runtime's labels are the source of truth (settled in M0)

The daemon **holds no runtime state in a state file**. A container's labels
(`dev.minato.*`) are the only truth, and after a restart the result of
`list_project` alone restores everything.

The state store holds two things: which worktrees Minato manages, and the URL
label issued to each. The label is persisted so that changing the
[naming rules](#5-naming-and-routing) later never changes an existing
workspace's URL.

This design all but removes "reconciling against real containers after a
crash", which was on the open-questions list. There is no second copy of the
state to reconcile against.

### The proxy asks for a start without knowing the runtime (settled in M2)

"Start it when a request arrives" needs the runtime, but the proxy cannot
depend on `minato-runtime` (§13, the direction of dependencies). So an
`Activator` trait forms the boundary and the daemon supplies the
implementation. All the proxy does is ask for a host to be made ready.

`Routes` **keeps stopped services too**. Without telling "stopped" apart from
"does not exist", a stopped service can never be woken and just 404s. The ones
with a target (`endpoint`) are the running ones, and that is the fast path.

**The hostname passed to the activator is always normalised.** `Host` can carry
a port, and passing it raw makes the idle-tracking key disagree with the
routing key. Accesses then go unrecorded, and a service in active use gets shut
down.

### "Started" and "ready to serve" are different (found in M0)

A container being up does not mean the app inside is listening. A `curl` right
after `up` returns fails with connection refused.

A person waits a few seconds and tries again. **An agent decides, on one
failure, that the server is broken.** For a tool aimed at agents that is fatal,
so from M0 there was a wait for a TCP connection to succeed
(`readiness::await_service`, capped at 15 seconds).

Past the cap it carries on and warns. A dev server's first start can take
longer while it resolves dependencies and compiles, and waiting forever means
`up` never returns.

M2 replaced this with the `health` check from `minato.toml`. For `http://`,
only the path is used and the request goes to the host-side address — what the
configuration names is the address from inside the container, which is not what
the host can reach. `cmd:` is unsupported, since it would have to run inside
the container.

### A stopped container is recreated

`up` leaves a running container alone, so running it repeatedly gives the same
result. A **stopped container is deleted and recreated**, though: a
configuration change silently not taking effect causes more confusion than a
start being a few seconds slower.

A side effect is that `down` then `up` changes the host-side port. That is only
visible until M1 pins the URLs.

## 8. Environment variables

### Three layers

Later wins.

```
1. global     ~/.minato/env                     every project
2. project    env in minato.toml + .minato/env  committed
3. workspace  .minato/env.local                 gitignored, per worktree
```

### Secrets

Nothing in plaintext in the repository. A value can be a reference, resolved at
start.

```
DATABASE_PASSWORD = "op://Development/myapp/password"   # 1Password CLI
API_KEY           = "keychain://minato/myapp/api-key"   # macOS Keychain
STRIPE_KEY        = "env://STRIPE_KEY"                  # the daemon's environment
```

A resolved value lives in the daemon's memory and never touches disk.
`minato env ls` masks it.

### What gets injected

Every service receives the following. **Without a way for the frontend to learn
the API's URL, a per-worktree environment cannot hold together**, so this is not
optional.

```
MINATO_PROJECT       = myapp
MINATO_WORKSPACE     = feat-1
MINATO_SERVICE       = web
MINATO_URL_WEB       = https://web.feat-1.myapp.localhost
MINATO_URL_API       = https://api.feat-1.myapp.localhost
MINATO_HOSTNAME_WEB  = web.feat-1.myapp.localhost
MINATO_HOSTNAME_API  = api.feat-1.myapp.localhost
MINATO_CA_FILE       = /etc/minato/ca.crt                      # while HTTPS is served
NODE_EXTRA_CA_CERTS  = /etc/minato/ca.crt                      # the same file, wired in
MINATO_TUNNEL_URL_WEB = https://web-feat-1.myapp.example.com   # with the tunnel on (M4)
```

A `-` in a service name becomes `_` (`api-server` →
`MINATO_URL_API_SERVER`), since a variable name cannot contain one.

**Injection is the bottom layer**, so the user can override it. The other way
round, Minato's conveniences would erase the user's settings.

**The certificate is wired in, not just named.** Naming the file and leaving
`NODE_EXTRA_CA_CERTS` to the service is what this did at first, and what came
back was `SELF_SIGNED_CERT_IN_CHAIN` from projects with the CA mounted and
unused — Node reads its extra certificate from the environment and nowhere
else, so a file nobody assigns to that name is a file nobody trusts. Both
costs are paid rather than avoided: it is kept out of `env_file`, which is read
on the host where that path does not exist and Node would warn about it on
every start, and an image that points it at a corporate bundle keeps that
bundle by saying so in `[services.<name>.env]`, since injection is the bottom
layer. **Only Node's.** `SSL_CERT_FILE`, `CURL_CA_BUNDLE` and
`REQUESTS_CA_BUNDLE` replace a trust store rather than adding to it, so
setting them would leave a container trusting Minato and nothing else.

**A task runner between the container and the process can drop it.**
Turborepo's strict environment mode passes through what its configuration
names and discards the rest, so the variable reaches `turbo` and not the
server it starts. Nothing injected can reach past that, and the guide says
what to add where.

**No URL is injected while the proxy is down**, and no hostname either — the
two go together. An empty string would leave it "set, but unreachable", and the
cause is hard to see.

### Referring to another variable

A value may hold `${ANOTHER_KEY}`, expanded when the layers are resolved.

```toml
[services.web.env]
NEXT_PUBLIC_API_URL = "${MINATO_URL_API}"
```

**Injection alone is not enough**: the URL arrives under Minato's name for it,
and the application reads its own. Without this, every project writes a
start-up script whose whole job is to copy one variable onto another.

- **A reference resolves to the value that won**, not to the layer below the
  one referring to it. An override that applied everywhere except where it was
  being used would be a trap
- **A bare `$NAME` is left as written.** These values have always been passed
  through verbatim, so expanding them would change what existing
  configurations mean. `minato up` warns when one names a variable that exists,
  which turns the trap into a message
- **A name nothing sets is an error**, for the same reason a missing URL is
  left unset rather than emptied
- **A secret cannot be built into another value.** Expanding one would put it
  in `minato env ls`; pasting the reference in would hand the container the
  string `op://…`

### Writing it to a file

Injection reaches the process. `wrangler dev` does not pass its environment to
the Worker, and Vite reads `.env.local` off disk, so `env_file` writes the
settled values into the worktree before the service starts.

```toml
[services.api]
env_file = ".minato/env.api"
```

- **Secrets are left out**, named in a comment instead. A resolved secret
  living only in memory is a guarantee that a file, handed on to whatever
  reads it, would end
- **Written before the start and on every wake**, from the same values the
  container is given — a file that disagreed with the process's environment
  would be worse than no file
- **Only for the services being started.** Settling an environment and writing
  a file are different jobs, and conflating them made `minato up web` answer
  for `api`'s `env_file` and left `minato exec` writing files as a side effect
  of running a command
- **Unchanged is not a write.** A dev server watching the file would otherwise
  restart every time scale-to-zero woke the service
- **Anywhere in the worktree**, because the tools that need this read paths of
  their own choosing. What makes that safe is refusing a path git tracks and
  never overwriting a file without Minato's header

### What M3 turned up

- `minato env ls` **says which layer each value came from**. With three layers,
  not seeing that an unintended one is winning makes the cause impossible to
  find
- Injected values are not masked. They are Minato's own and hold no secrets, and
  checking a URL is common
- A secret stays a reference even under `--reveal`. Showing the value would mean
  resolving it, and that only happens at start
- **`env set` says outright that a restart is needed.** Containers already
  running do not pick it up, and left unsaid that reads as "I set it and nothing
  happened"
- A secret failing to resolve does not take the daemon down. Usually it just
  means nobody is signed in to 1Password, and letting that keep the whole
  environment from starting is the worse outcome. It comes back as a warning,
  and only that key is dropped
- **A listing that cannot settle still lists.** Where starting a service
  refuses over a `${...}` nothing sets, `env ls` marks that value and says why
  underneath: it is the tool for finding the one at fault, and the error alone
  leaves nowhere to look. `env get` does refuse, since it prints a value for a
  script to use
- **Per value, not per listing.** Failing the lot over one bad reference would
  show thirty settled values as unexpanded too, and nothing would tell them
  apart — which is also what made the client guess at `${` to decide whether a
  value was usable
- A listing of no particular service is missing `MINATO_SERVICE` and every
  service's own `env` by design, so a value built from one cannot settle there
  even though the service starts. The reason says that, and names the service
  whose listing does have it — only that one settles it. "Nothing sets it"
  would send someone after a bug that is not there
- The reason travels as **structured data**, not a sentence: which name, and
  which of a handful of causes. The CLI turns it into English, as §3 requires

## 9. Cloudflare Tunnel

### Approach

**One named tunnel per machine.** The ingress rule sends everything to the
local proxy and leaves routing on Host to it.

On the DNS side that is one wildcard CNAME per project,
`*.{project}.example.com`, so workspaces come and go without touching any
records. It is the simplest arrangement and costs nothing at startup.

```yaml
# the generated cloudflared configuration
tunnel: minato

ingress:
  - hostname: "*.example.com"
    service: http://127.0.0.1:80
  - service: http_status:404
```

Tunnel hostnames sometimes allow only one level of subdomain, so the form is
`{service}-{workspace}.{project}.example.com`.

#### The proxy resolves the tunnel hostname (settled in M4)

The original plan had `originRequest.httpHostHeader` rewrite the Host back to
the `.localhost` name. That does not work: `httpHostHeader` is a fixed string,
so one rule cannot rewrite per request, and a rule per service would mean
regenerating the file and reloading cloudflared every time a worktree appeared
— exactly the churn the wildcard exists to avoid.

Instead **the daemon registers the tunnel hostname in the proxy's routing table
alongside the `.localhost` one**, pointing at the same service. The Host header
arrives untouched and resolves on the other side. The consequences are worth
naming:

- The cloudflared configuration is written once and never changes. Not per
  project either — the rule covers the whole zone, and DNS is what gates which
  hostnames actually arrive
- Scale-to-zero works for a reviewer following a shared link, because the
  tunnel hostname is a route like any other and a request wakes it
- A service with `expose = false` gets no tunnel hostname, so a database cannot
  be reached from outside even by guessing the name

The hop from cloudflared to the proxy is plain HTTP over loopback. Going to the
HTTPS port would mean cloudflared verifying the local CA, which it has no
reason to trust, and TLS is terminated at Cloudflare's edge regardless.

#### Routes are rebuilt at daemon start (found in M4)

The routing table lives in memory, so a daemon restart left it empty until some
command happened to call `refresh`. Locally that self-corrects the first time
anyone runs `status`. A reviewer holding a tunnel link has no such move, and
scale-to-zero cannot rescue them because the route is not registered for a
request to wake. The daemon now rebuilds every registered project's routes at
start. The bug predates the tunnel; the tunnel is what made it unrecoverable.

### Access control

A development environment open to the internet without authentication is an
accident, so the design called for a Cloudflare Access policy by default, with
`--public` to opt out.

**Minato cannot apply that policy.** Access is configured through Cloudflare's
API, and everything here goes through the `cloudflared` CLI so there is no API
token to obtain, scope or store. Since it cannot promise the policy is in
place, it does the next most honest thing: `tunnel enable` refuses without
`--public`, and the flag reads as "I know this is going on the internet". Every
`tunnel status` on a running tunnel repeats that it cannot see whether Access
is in front.

Applying the policy — and so restoring the opt-out the design wanted — needs
the API-token path, which is open.

### Setup is reported, not run

`cloudflared tunnel login` opens a browser and waits. Running it from the
daemon would hang an agent exactly the way an unattended `sudo` does, so it is
reported as a step for the user to take, the same as §14's privileged setup.

Everything after login is not interactive and the daemon does it itself:
`tunnel create` and `tunnel route dns` run on every enable and on every daemon
start, with "it already exists" read as success. The alternative is a flag in
the state file that can disagree with what Cloudflare actually has.

## 10. The CLI

Every command has `--json`, and the exit code plus structured output is all it
takes. An agent never has to parse output written for people.

```
minato init                       # write minato.toml, set up daemon/DNS/CA
minato doctor                     # check the DNS resolver, docker, certificates, ports

minato new <branch> [--from main] # git worktree add, warm the environment, print URLs
minato rm <workspace> [--force]   # destroy the environment, git worktree remove
minato ls [--json]                # workspaces and their state

minato up [--service web]         # start explicitly
minato down [--all] [--service web]
minato status [--json]

minato url [service] [--qr]       # one line named, the listing otherwise
minato open [service]             # open it in a browser
minato logs <service> [-f] [--tail N] [--since 5m]
minato exec <service> -- <cmd>

minato env set KEY=VAL [--scope global|project|workspace]
minato env ls [--json] [--reveal]
minato env unset KEY

minato tunnel enable [--domain example.com] [--public]
minato tunnel disable
minato tunnel status
```

`minato doctor` ranks high because the initial setup involves sudo and outside
dependencies (Docker, cloudflared). On failure it says what to do about it.

#### `state` is a string, and `reason` sits beside it (settled after M7)

`ServiceState` is an enum with a payload on one variant, and serde's
internally tagged form wrote that as `{"state":"ready"}`. Which is a
faithful encoding and the wrong one: `.state == "ready"` is never true, on
the first command [the Skill](#11-skills) tells an agent to run. §10's own
example here said `"state": "ready"`, so an agent reading the design wrote
exactly the comparison that could not work.

Found by running a real task through the Skill rather than by reading the
code — `docs/AGENT-RUN.md`.

**The Rust type is unchanged.** `Failed { reason }` is right for Rust: the
CLI and the GUI match on it and the runtime builds it. What changed is how
it is written down — a plain string — and every API type carrying a state
now carries an optional `reason` next to it. The cost lands on whoever
builds the value, once, instead of on every reader.

A state read back off the wire therefore has an empty reason, which is
pinned by a test rather than left to be discovered.

### An example of the JSON

```jsonc
// minato status --json
{
  "project": "myapp",
  "workspace": "feat-1",
  "branch": "feature/user-auth",
  "path": "/Users/hotaka/ghq/github.com/hota1024/myapp.wt/feat-1",
  "services": [
    {
      "name": "web",
      "state": "ready",
      "url": "https://web.feat-1.myapp.localhost",
      "tunnel_url": "https://web-feat-1.myapp.example.com",
      "endpoint": "127.0.0.1:49312",
      "last_access": "2026-08-07T09:12:44Z"
    },
    { "name": "db", "state": "ready", "url": null, "scope": "project" },
    { "name": "api", "state": "failed", "reason": "the container exited with code 3" }
  ]
}
```

## 11. Skills

`minato skill install` writes `.claude/skills/minato/SKILL.md`. What goes in it
is **judgement**, not a CLI reference. `--help` covers what the commands do;
promises like "never reach for `docker`" and "never guess a port" only land if
they are written down.

The Skill is baked into the binary with `include_str!`. A self-contained
`minato` works however it was installed. Identical content is left alone, so
git stays clean.

### `logs` and `exec` are what Skills rest on (found in M5)

"An agent finishes the work without touching `docker`" needs log viewing and
running commands inside a container. **Without them, debugging forces a return
to `docker`, and nothing written in the Skill survives that.**

`minato exec` passes the command's exit code straight through, because an agent
has to be able to judge `minato exec web -- pnpm test` by exit status alone. No
TTY is requested — hanging on a prompt is the worse outcome.

### Where an agent trips first

Without `minato setup`, `curl` fails with exit code 60: the certificate is not
trusted. With plain `curl -s` the error is swallowed and it looks like nothing
but an empty response. The Skill names this symptom and points at
`minato doctor`.

No MCP server, for now. With `--json` on every command, Bash is enough, and a
second surface is not worth maintaining.

## 12. The GUI (`minato-desktop`)

Pure Rust, on GPUI with gpui-component. It links `minato-client` directly, so
sharing type definitions needs no generation step — the biggest advantage of
not using TypeScript.

### The screen

1. **A sidebar of workspaces** — project, workspace, and each service's state
   (`stopped` / `starting` / `ready` / `idle`), updated continuously
2. **A detail pane** — URLs to copy or open, and start and stop buttons
3. **A log viewer** — tailing across services, filterable
4. **An environment editor** — showing which of the three layers each value came
   from, with secrets masked
5. **doctor** — the DNS resolver, certificates and port checks, with one-click
   fixes

### Living in the menu bar

Minato's GUI is not something to keep open; it is mostly for glancing at which
environments are running and opening one. GPUI cannot do a tray on its own, so
`tray-icon` handles that part:

- Only the tray icon is resident. Its menu reaches the running workspaces and
  their URLs directly
- The window opens only when asked for
- Closing the window does not end the process

### Async

The render loop is synchronous and cannot handle `async` directly. The two are
kept apart:

```
[tokio runtime thread]                      [render thread]
  subscribes to the daemon                    reads AppState and draws
  writes what arrives into AppState   ───>    sends user actions as commands
       (Arc<RwLock<AppState>> + a notification channel)
```

The daemon already provides an event stream (§3), so the GUI never polls. A
redraw is requested only when an event arrives; idle costs nothing.

### Things to watch

- **Fonts**: GPUI goes through font-kit and finds system fonts, including CJK,
  without anything embedded. Under egui this needed explicit handling, and the
  code for it was removed with the GPUI rewrite
- **Fidelity**: it will not feel native. Rather than pour time into the UI, the
  bet is on information density and how fast it updates

### What M6 turned up

- **The GUI never starts the daemon.** Looking after the daemon is launchd's
  job, and a GUI managing it too would split that responsibility. This settles
  the open question in §15
- The tray menu is **rebuilt only when it changes**. Rebuilding every frame
  closes it out from under whoever has it open
- A failed connection is logged. Shown on screen only, it leaves no trace to
  diagnose from when the GUI cannot connect
- A redraw is **requested only on an event**. Redrawing continuously burns CPU
  doing nothing

## 13. Repository layout

One Cargo workspace for the product. With GPUI there is no Node.js in
anything that ships, and no `packages/` for TypeScript.

The one exception is `docs/`, which is a VitePress site and therefore has its
own `package.json`. It is build tooling for the documentation and no part of it
reaches a binary, so the rule it bends — no Node toolchain — still holds where
it was meant to.

```
minato/
├── Cargo.toml            # [workspace] members + workspace.dependencies
├── rust-toolchain.toml
├── crates/               # libraries, not shipped
│   ├── minato-core/      #   spec, config, naming, state store, terminal modes (the bottom of the graph)
│   ├── minato-api/       #   RPC request/response/event types (one source)
│   ├── minato-client/    #   the RPC client, shared by the CLI and the GUI
│   ├── minato-runtime/   #   the Runtime trait plus the Docker implementation
│   ├── minato-proxy/     #   the hyper reverse proxy, rustls, the local CA
│   ├── minato-dns/       #   a hickory-server wrapper
│   └── minato-tunnel/    #   managing the cloudflared process
├── apps/                 # binaries, shipped
│   ├── daemon/           #   minatod — the supervisor and the RPC server
│   ├── cli/              #   minato
│   └── desktop/          #   minato-desktop — the GPUI GUI
├── assets/               # the logo, in one place; see assets/README.md
│   └── logo/             #   copied into docs/public/logo/ by the docs build
├── skills/
│   └── minato/SKILL.md
├── xtask/                # cargo xtask: docs snapshots, packaging, the launchd plist
└── docs/                 # the VitePress site, English and Japanese
    ├── guide/  reference/  tutorials/
    ├── ja/               #   the same tree, translated
    ├── v0.1/             #   a frozen release, made by `cargo xtask docs snapshot`
    └── DESIGN.md         #   this file. Excluded from the site
```

### Versioning the documentation

VitePress has no versioning of its own, but it keys locales by directory, and
`docs/.vitepress/config.ts` generates one locale per (version, language) pair
from `versions.json`. So freezing a release is a copy of the tree plus a line
in that file — no configuration to edit, and the sidebar and version switcher
follow.

Current docs stay at the root, where they are edited. `cargo xtask docs
snapshot 0.1` copies them to `/v0.1/` and rewrites absolute links to point
inside the copy; relative ones already resolve. Snapshots are history and are
not edited afterwards.

The alternative — every page under a version directory from day one — was
rejected because it puts every edit inside a version folder before there is
more than one version to distinguish.

### The direction of dependencies

```
apps/cli ────┐
apps/desktop ┴──> minato-client ──> minato-api ──> minato-core
apps/daemon ─────────────────────>  minato-api ──> minato-core
       └──> minato-runtime / minato-proxy / minato-dns / minato-tunnel ──> minato-core
```

`minato-api` is the only point of contact between the daemon and its clients.
**No client-side crate may depend on `minato-runtime` or its neighbours** —
that would leak Docker logic into the GUI and break the rule that everything
goes through the daemon. `cargo xtask deps check` walks the dependencies of
all three clients — the CLI, `minato-client` and the desktop app — with
`cargo tree` and fails if one of them reaches `minato-runtime`, `-proxy`,
`-dns` or `-tunnel`; CI runs it alongside `fmt` and `clippy`.

It asks for every target rather than the one it happens to run on, and
counts build-dependencies as well as normal ones. Both are ways a crate can
be reached without appearing in an ordinary host build, and a check that
only looks where it is standing is the kind that reports success for years.

### Versioning

Every crate shares one version, inherited from `workspace.package.version`.
None of the internal crates is headed for crates.io individually, so
independent versioning would only add complexity. Only `minato`, the CLI, is
published.

### The main dependencies

| For | Crate |
| --- | --- |
| Async runtime | `tokio` |
| CLI | `clap` (derive) |
| Configuration | `serde`, `toml`, `figment` |
| Docker API | `bollard` |
| HTTP and proxying | `hyper`, `hyper-util`, `axum` (the management API) |
| TLS | `rustls`, `rcgen` (the local CA) |
| DNS | `hickory-server` |
| IPC | `tokio::net::UnixListener` |
| GUI | `gpui`, `gpui-component`, `tray-icon` |
| Logging | `tracing`, `tracing-subscriber` |
| Errors | `thiserror` in libraries, `anyhow` in binaries |
| Git | `gix`, or shelling out to `git` |

## 14. Roadmap

| Milestone | Contents | Done when |
| --- | --- | --- |
| **M0** ✅ | The workspace skeleton, core (config, naming, state), `minato-api` including the event stream, the Docker and Apple Container runtimes, `init` / `new` / `up` / `down` / `rm` / `ls` / `status` / `url` / `daemon` | Creating a worktree starts its containers, reachable at `localhost:<dynamic port>` |
| **M1** ✅ | DNS, the proxy, TLS, `doctor` / `setup`, launchd socket activation | `https://web.feat-1.myapp.localhost` answers curl |
| **M2** ✅ | Scale-to-zero, health checks, idle stop, on-demand start | Ten worktrees, and the only running containers are the ones in use |
| **M3** ✅ | Environment variables: three layers, secret references, injection | The frontend can read `MINATO_URL_API` |
| **M4** ✅ | Cloudflare Tunnel | `https://web-feat-1.myapp.example.com` works from a phone |
| **M5** ✅ | Skills, `logs` / `exec` | An agent finishes the work without touching `docker` |
| **M6** ✅ | The GUI: GPUI plus a tray | The menu bar shows the running workspaces and their URLs, and logs are readable |
| **M7** ✅ | Apple Container made to work on real hardware; `doctor` and `ping` follow `[runtime] default`. Firecracker deferred — it needs a Linux host | Switching `[runtime] default` is all it takes |

M1 is the minimum line worth shipping. M2 is where it becomes usable daily.

The GUI sits at M6 because doing it after the daemon's API had settled meant
less rework. But **the API's event stream is there from M0** — added later, the
design would have assumed blocking, and the GUI could never show progress.

### What M2 turned up

- A stopped service **keeps its URL**. A request is what starts it, so a URL
  that came and went with the state would take the way to wake it along
- Concurrent requests for one host claim a single right to start it. Whoever
  loses waits on the start already running
- A shared service (`scope = "project"`) is referenced from several workspaces.
  One live reference is enough to keep it up
- After a daemon restart, a host that is running with no record gets a
  baseline. Without one it never looks idle and never stops

### Cancellation needed the loop turned inside out (M0.5)

`Request::Cancel` was in the protocol from M0 and ignored by the daemon, and
the reason turned out to be structural rather than missing work. The
connection loop read a message, awaited the whole request, then read the next
one — so a `Cancel` sat unread in the socket until the thing it referred to
had already finished. There was no point at which it could have been
honoured.

Each request now runs in its own task and the read loop keeps going, which is
what the request ids were always for. Requests on one connection can overlap
as a result; the CLI sends one at a time regardless.

Cancelling aborts the task and answers `Cancelled`, and **the client waits for
that answer** rather than dropping the connection. The daemon is the one that
knows how far it got. Work already done is not undone: a cancelled `up` can
leave a container running, which `status` shows and `down` clears. Checking
for cancellation between every step would be a lot of machinery for an
operation someone has already walked away from.

Ctrl-C in the CLI is wired to this, so it asks the daemon to stop instead of
killing the client and leaving the daemon working on something nobody is
waiting for. `logs -f` is exempt: Ctrl-C is how you leave it, and there is
nothing in flight to abandon.

### Running on Linux (checked in M0.5)

The core builds and its 384 tests pass on Linux. Two things only showed up
there, and neither would have been found on macOS:

- **A closed socket reads differently.** macOS gives a clean EOF, Linux an
  ECONNRESET. The client reported the first as "the connection to the daemon
  was closed" and the second as "connection I/O failed: Connection reset by
  peer (os error 104)", so a Linux user whose daemon died got the worse
  message. Both now mean the same thing.
- **A test raced on process-global state.** One test set an environment
  variable that every other test read while building settings. It passed
  consistently on macOS and failed under Linux's scheduling. The variable is
  no longer touched; the function that reads it takes the value as an
  argument.

CI runs both platforms for this reason. launchd socket activation remains
macOS-only, and the fallback path — an ordinary bind on unprivileged ports —
is what Linux uses.

### Handling the privileged setup

**sudo is never run unasked.** An agent starting one hangs at the password
prompt, and from the user's side it looks like a silent privilege escalation.
So `minato setup` walks through its steps only where there is a terminal to
answer at: each step's commands are printed, then it asks, and only what is
agreed to is run. With no terminal — an agent, a pipe, `--json` — it prints the
commands and runs none of them, which is what it has always done. `--yes` runs
every step; `--dry-run` runs none.

The question comes *after* the commands, every time. Agreeing to "trust the
local CA" is not agreeing to whatever that turns out to run as root, and the
two have to be on the screen together for the answer to mean anything.

Three things need privileges, and `minato setup` covers all of them.

1. Installing the LaunchDaemon, which holds 80, 443 and 53
2. Installing `/etc/resolver/localhost`
3. Trusting the local CA

Generating the plist needs no privileges, so it is written to `~/.minato/` and
only the install commands are printed. They can be read before they are run.

**The steps are generated for the state after setup.** Installing launchd moves
DNS to :53, so that is the port the resolver gets. Writing the current port
would leave nothing resolving once it lands.

Which is only true if launchd *does* land, and being able to say no to one step
means it might not. So the resolver step is rewritten mid-walk for the port DNS
is actually on when the launchd step was declined or failed. Otherwise saying no
to the first question would quietly break resolution through the second.

### Deferred at M0

| Item | Why | When |
| --- | --- | --- |
| `minato.local.toml` overrides | Environment variables cover most of what it was for, so it waits for demand | Undecided |

### What ships, and what does not

`nightly` carries `minato` and `minatod` for macOS — Apple Silicon and Intel —
and Linux x86_64. Both Apple targets are built on one runner: the second is a
cross-compile that costs a minute, where a second matrix entry pays for a
whole runner again, and a macOS runner bills at ten times the rate on a
private repository.

Nothing is signed. macOS quarantines the CLI and the daemon on first run,
which `xattr -d com.apple.quarantine` clears. **The desktop app is not
shipped at all**, because Gatekeeper stops an unsigned `.app` outright rather
than warning about it — an archive nobody can open would promise more than it
delivers. Signing and notarisation stay open below.

### Branching: trunk on `main`, and when that changes

`main` is the trunk. Short-lived branches, a pull request into it, CI as the
gate, and merging replaces the rolling `nightly` build. There is no `develop`.

`develop` was considered and left out. What it solves is keeping a released
version maintainable while unreleased work continues — a report against v1.2
that has to ship while 1.3 is in progress. Nothing here is in that position:
no tags, no versioned release, nobody running one. Adding it now would mean
two merges per change, two protected branches, and three workflows each
having to decide which branch they follow, in exchange for reviewing one's
own already-reviewed work a second time.

`main` is always releasable because CI gates it, not because a second branch
absorbs the risk.

**Revisit when 0.1.0 is tagged and someone is running it.** Even then a
`develop` is not the only answer: cutting `release/0.1` from the tag at the
moment a patch is actually needed costs nothing until that moment, which is
how Rust and Go handle it. If a `develop` does arrive, `nightly` moves to it
and the docs site follows it — `main` carrying the nightly while also meaning
"released" would answer neither question.

## 15. Open questions

- **Firecracker**: needs KVM, so it cannot be developed or run on macOS. It
  waits for a Linux host rather than being written blind
- **Isolation on Apple Container**: everything shares the default network, so
  one worktree's containers can reach another's. Acceptable for local
  development by one person, and the alternative breaks shared services
  outright, but multi-network attachment would let both hold
- **Cloudflare Access**: applying the policy needs the API rather than the
  CLI, which means an API token to obtain, scope and store. Until then
  `--public` is an acknowledgement rather than an alternative to a policy
- **Verifying the tunnel end to end**: everything up to and including the
  hostname routing is exercised, the last against a stub `cloudflared`. What
  has not run is a real named tunnel against a real zone, which needs a
  Cloudflare account and a domain
- **Migration conflicts on a shared database**: several worktrees applying
  different migrations to a `scope = "project"` database will break it. A
  database per worktree — separate database names inside one instance — is the
  leading idea, but how to implement it without depending on the runtime is
  undecided
- **How far `minato init` should infer**: reading compose is done —
  `--from-compose`, and §4's note on it. Inferring from `package.json` and
  Dockerfiles is still open, and a harder question, because neither says
  what a *service* is
- **Worktree directory conventions**: `{repo}.wt/{name}` is the default, but how
  it should sit alongside existing habits — under ghq, next to `.git/worktrees`
  — is unsettled
- **One daemon across projects**: the assumption is that one daemon watches
  every project, but the state store's schema and locking strategy are
  undecided
- **The length of `MINATO_HOME`**: `sun_path` is 104 bytes on macOS, so a deep
  path fails to bind. `Paths::check_socket_length()` catches it up front, but
  moving the socket to `$TMPDIR` is another option
- **How the GUI is distributed**: a signed and notarised `.app` bundle, or just
  `cargo install`. Living in the tray argues for the former, but notarisation
  has a cost

# Security

## Reporting a vulnerability

**[Report it privately here.](https://github.com/hota1024/minato/security/advisories/new)**

Not a public issue, and not a pull request — either one publishes the
vulnerability before there is anything to update to.

Minato is written by one person in their own time. There is no rota and no
response-time commitment; what there is, is an acknowledgement as soon as the
report is read, and being told what happens next rather than being left to
wonder. If a week goes by in silence, a second nudge on the same advisory is
welcome and not a nuisance.

Useful in a report, in rough order: what an attacker gets, the version
(`minato --version`, which carries the commit for a build that came from a
release), the OS and runtime, and the shortest sequence that shows it. A proof
of concept is welcome and never required — a clear description of the flaw is
worth more than a working exploit.

You will be credited in the advisory unless you would rather not be.

## What is supported

| | |
| --- | --- |
| `nightly` | The rolling build of `main`. **The only thing that receives fixes.** |
| Anything older | Not supported. There are no releases yet |

There is no released version. Until there is, a fix means the next merge to
`main`, and `minato update` is how you get it.

## What Minato asks of a machine

Worth reading before `curl … | sh`, and the context a report lands in.

### A certificate authority in your system trust store

`minato setup` offers to trust a CA it generates in `~/.minato/ca/`, which is
what makes `https://web.feat-1.myapp.localhost` work without a warning. The
private key is `0600` and never leaves the machine.

**That CA is not constrained to Minato's own hostnames.** Anything that can
read the key can mint a certificate for any name at all, and your browser will
believe it. This is the same bargain `mkcert` and every other local-HTTPS tool
asks for, and narrowing it with a name constraint is known work that has not
been done — but it is a bargain, so it is written down here rather than left
to be discovered.

`minato doctor` says whether it is currently trusted, and `minato uninstall`
hands you the command to stop — printed for you to run, since removing it
needs root and nothing here runs `sudo` unasked.

### A daemon that runs commands for whoever reaches its socket

`minatod` listens on a Unix socket and asks its callers for nothing — no
token, no handshake. That is the right shape for something your own CLI, GUI
and coding agent all speak, and it means access is decided entirely by who can
reach the socket: `MINATO_HOME` is `0700`, the socket is `0600`, and the uid on
the other end is checked on connect.

It matters because the API is not read-only. `minato exec` runs commands inside
your containers, and prints the secrets Minato resolved from 1Password or the
Keychain to give them.

Point `MINATO_HOME` somewhere other accounts can reach and you have given those
accounts the daemon.

### A flag that puts an environment on the public internet

`minato tunnel enable --public` publishes a workspace through Cloudflare Tunnel.
`--public` is required and means what it says: **Minato cannot see whether a
Cloudflare Access policy is in front of the hostname, and does not add one.**
Put one there yourself. A service with `expose = false` is never given a tunnel
hostname.

### Binaries nobody has signed

The release archives are checked against a `.sha256` published beside them,
which catches a corrupted download and nothing else — an attacker who can
replace one can replace the other. Nothing is code-signed or notarised, and
macOS quarantines the binaries on first run. Signing is open work.

If you are installing somewhere this matters, build from source.

## Not vulnerabilities

Reported often enough to be worth naming, and all of them things Minato is
doing on purpose:

- **A service is reachable from another worktree's containers.** Under Apple
  Container everything shares one network, because the alternative cuts off
  services shared across worktrees. Documented in `docs/DESIGN.md` §6
- **An environment published with `--public` is reachable by anyone.** That is
  what the flag is for, and why it has to be typed
- **A local user with your account can drive the daemon.** So can they run
  `docker`, and read `~/.minato` — the boundary Minato draws is between
  accounts, not within one
- **`minato.toml` runs the commands it says it does.** It is code from the
  repository you checked out, and it is trusted exactly as much as that
  repository is
- Findings from an automated scanner with no path to an attacker behind them

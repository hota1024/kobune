//! Cloudflare's quick tunnel: no account, no domain, no login.
//!
//! `cloudflared tunnel --url` opens a tunnel to a hostname Cloudflare
//! invents, under `trycloudflare.com`, and prints it. Nothing is
//! registered anywhere and nothing outlives the process.
//!
//! **It is the other shape of tunnel**, and the reason this exists is that
//! implementing it is what made the shape concrete — the same way writing
//! Apple Container is what settled what `Runtime` had to absorb
//! (`docs/DESIGN.md` §6). Everything Cloudflare's named tunnel takes for
//! granted is missing here: there is no zone, so no wildcard; no wildcard,
//! so one hostname reaches exactly one origin; one origin per hostname, so
//! **one `cloudflared` per service**. The name arrives on stderr thirty
//! seconds after the process starts rather than being worked out in
//! advance.
//!
//! What that costs is written into [`Needs::targets`]: a quick tunnel
//! covers the services it was told about when it was enabled, and a
//! worktree made afterwards is not reachable until it is enabled again. A
//! zone covers what does not exist yet; this cannot.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::provider::{
    Access, Hostnames, Leftover, Missing, Needs, Readiness, StartOutcome, Started, TunnelProvider,
    TunnelRequest,
};
use crate::{Result, RunningTunnel, TunnelError};

use super::cloudflare;

/// The identifier stored in `TunnelRecord.provider`.
pub const ID: &str = "quick";

/// The domain Cloudflare hands names out under.
const DOMAIN: &str = "trycloudflare.com";

/// How long to wait for a hostname to be printed.
///
/// The tunnel is up before it says so, but a hostname that has not
/// arrived is a service nothing can reach, and waiting forever would hang
/// `tunnel enable` with no way to tell it from a working one.
const HOSTNAME_TIMEOUT: Duration = Duration::from_secs(30);

/// Cloudflare's quick tunnel, one process per published service.
#[derive(Debug, Clone)]
pub struct QuickProvider {
    program: String,
}

impl Default for QuickProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl QuickProvider {
    pub fn new() -> Self {
        // The same binary as the named tunnel, found the same way.
        Self {
            program: cloudflare::program(),
        }
    }

    /// Points it at a different binary. For tests, and for the override.
    pub fn with_program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

#[async_trait]
impl TunnelProvider for QuickProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn display_name(&self) -> &'static str {
        "Cloudflare quick tunnel"
    }

    fn needs(&self) -> Needs {
        Needs {
            // There is nothing for the user to name. The hostname comes
            // from Cloudflare and is theirs, which is also why it cannot
            // be kept.
            domain: false,
            // One hostname reaches one origin, so a service that was not
            // named when this was enabled has nothing pointing at it.
            targets: true,
        }
    }

    fn access(&self) -> Access {
        // **Not "Kobune cannot see one" — there is none.** The hostname
        // is Cloudflare's, handed out to anyone who asks and guessable in
        // the sense that it is published to Cloudflare's own certificate
        // transparency logs the moment it exists. There is nothing here a
        // user could attach a policy to, so advising one would be advice
        // that cannot be taken.
        Access::Open
    }

    fn readiness(&self, _request: &TunnelRequest) -> Readiness {
        // **No login.** That is the whole point of a quick tunnel, and it
        // is why this is the provider that answers on a machine where
        // nothing has been set up.
        if kobune_core::program::find(&self.program).is_none() {
            return Readiness::NotInstalled;
        }

        Readiness::Ready
    }

    fn missing(&self, readiness: &Readiness) -> Option<Missing> {
        match readiness {
            Readiness::NotInstalled => Some(Missing {
                summary: format!("{} is not installed", cloudflare::PROGRAM),
                commands: vec![format!("brew install {}", cloudflare::PROGRAM)],
            }),
            // Unreachable, and said so rather than left to a wildcard arm
            // that would report "log in" about the one provider that never
            // asks anyone to.
            Readiness::NeedsLogin | Readiness::Ready => None,
        }
    }

    fn dns_record(&self, _request: &TunnelRequest) -> Option<String> {
        // Nothing was written to anybody's zone. Naming a record here
        // would send someone looking in a dashboard for a name that has
        // never existed.
        None
    }

    async fn start(&self, request: &TunnelRequest) -> Result<Started> {
        if request.targets.is_empty() {
            return Err(TunnelError::failed(
                "starting a quick tunnel",
                "no services to publish. A quick tunnel reaches one service \
                 per hostname, so it has to be told which",
            ));
        }

        // **Started together, not one after another.** Each waits on its
        // own banner from Cloudflare's edge, and the waits have nothing
        // to do with each other: in a row, four services is four round
        // trips end to end, and up to two minutes before `enable` reports
        // a failure that was visible after thirty seconds.
        let mut starting = tokio::task::JoinSet::new();
        for target in &request.targets {
            let program = self.program.clone();
            let port = request.local_port;
            let target = target.clone();

            starting.spawn(async move { (target, spawn(&program, port).await) });
        }

        let mut children = Vec::new();
        let mut names = BTreeMap::new();
        let mut failed = None;

        while let Some(joined) = starting.join_next().await {
            match joined {
                Ok((target, Ok((child, hostname)))) => {
                    names.insert(target, hostname);
                    children.push(child);
                }
                Ok((_, Err(err))) => failed = failed.or(Some(err)),
                Err(err) => {
                    failed = failed.or(Some(TunnelError::failed("starting a quick tunnel", err)));
                }
            }
        }

        // One of them not arriving is the whole tunnel not arriving: a
        // service with no hostname is one nothing can reach, and the
        // others would otherwise be left running with nothing holding
        // them.
        if let Some(err) = failed {
            QuickTunnel {
                children,
                hostnames: Hostnames::Assigned(names),
            }
            .kill_all()
            .await;

            return Err(err);
        }

        let notes = notes_for(request, names.len());
        let hostnames = Hostnames::Assigned(names);

        Ok(Started {
            tunnel: Box::new(QuickTunnel {
                children,
                hostnames,
            }),
            outcome: StartOutcome {
                notes,
                // Nothing was set up, so there is nothing to have
                // confirmed and nothing to go quiet about later. The
                // notes below are true on every run and say so every
                // time — which is right, because what they describe is
                // true again every time.
                settled: false,
            },
        })
    }

    fn leftovers(&self, _request: &TunnelRequest) -> Leftover {
        // **Nothing to clean up, and that is worth the empty struct.**
        // No account was touched, no record written, no tunnel named. The
        // hostnames stopped existing when the processes did.
        Leftover::default()
    }
}

/// What is worth saying about a quick tunnel, every time.
///
/// Unlike a zone's, none of this stops being true on a later run, so none
/// of it is gated on [`TunnelRequest::settled`].
fn notes_for(request: &TunnelRequest, published: usize) -> Vec<String> {
    if !request.explain {
        return Vec::new();
    }

    vec![
        "these URLs are Cloudflare's and last only as long as".to_string(),
        "this tunnel: restarting gives out different ones.".to_string(),
        format!(
            "{published} service{} published; anything made later",
            if published == 1 { "" } else { "s" }
        ),
        "needs `kobune tunnel enable` again to be reachable.".to_string(),
    ]
}

/// The processes carrying a quick tunnel, and the names they were given.
#[derive(Debug)]
pub struct QuickTunnel {
    children: Vec<Child>,
    hostnames: Hostnames,
}

impl QuickTunnel {
    async fn kill_all(mut self) {
        for child in &mut self.children {
            if let Err(err) = child.kill().await {
                tracing::debug!("cannot stop a quick tunnel: {err}");
            }
        }
    }
}

#[async_trait]
impl RunningTunnel for QuickTunnel {
    fn hostnames(&self) -> &Hostnames {
        &self.hostnames
    }

    /// Whether **every** process is still alive.
    ///
    /// Not "any". Each one carries a hostname somebody may be holding a
    /// link to, and a tunnel reporting `running` while one of the URLs it
    /// printed has silently stopped answering is exactly the lie this
    /// exists to catch. Reading as down takes the routes with it, which
    /// is the honest state.
    fn is_running(&mut self) -> bool {
        !self.children.is_empty()
            && self
                .children
                .iter_mut()
                .all(|child| matches!(child.try_wait(), Ok(None)))
    }

    async fn stop(self: Box<Self>) {
        (*self).kill_all().await;
    }
}

/// Starts one quick tunnel and waits for the hostname it is given.
async fn spawn(program: &str, local_port: u16) -> Result<(Child, String)> {
    let mut child = Command::new(program)
        .args(["tunnel", "--no-autoupdate", "--url"])
        .arg(format!("http://127.0.0.1:{local_port}"))
        .stdout(Stdio::null())
        // **Piped rather than inherited**, unlike the named tunnel's. The
        // hostname is only ever announced here, so this is the one place
        // it can be read from.
        .stderr(Stdio::piped())
        // The child goes when nothing is holding it, rather than outliving
        // the daemon as a tunnel nobody is managing.
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                TunnelError::NotInstalled(program.to_string())
            } else {
                TunnelError::failed(format!("running {program}"), err)
            }
        })?;

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| TunnelError::failed("starting a quick tunnel", "no stderr to read"))?;
    let mut lines = BufReader::new(stderr);

    match tokio::time::timeout(HOSTNAME_TIMEOUT, read_hostname(&mut lines)).await {
        Ok(Some(hostname)) => {
            // **The pipe has to outlive the read.** cloudflared writes to
            // stderr for as long as it is up — a line per edge connection,
            // then heartbeats — and Go terminates a program on SIGPIPE for
            // fd 2. Dropping the reader here would close the far end and
            // kill the tunnel seconds after `enable` printed its URL, with
            // nothing anywhere to say why.
            //
            // Draining it is also where a quick tunnel's log finally goes.
            // The named tunnel inherits the daemon's stderr; this one
            // cannot, because the hostname had to be read out of it first.
            tokio::spawn(async move {
                let mut line = Vec::new();
                while let Ok(read) = lines.read_until(b'\n', &mut line).await {
                    if read == 0 {
                        break;
                    }
                    tracing::debug!("cloudflared: {}", String::from_utf8_lossy(&line).trim_end());
                    line.clear();
                }
            });

            Ok((child, hostname))
        }
        Ok(None) => {
            let _ = child.kill().await;
            Err(TunnelError::failed(
                "starting a quick tunnel",
                format!("{program} exited without giving out a hostname"),
            ))
        }
        Err(_) => {
            let _ = child.kill().await;
            Err(TunnelError::failed(
                "starting a quick tunnel",
                format!("no hostname after {} seconds", HOSTNAME_TIMEOUT.as_secs()),
            ))
        }
    }
}

/// Reads stderr until a hostname goes past.
///
/// Bytes rather than lines, decoded lossily. `AsyncBufReadExt::lines`
/// fails a whole read on one byte that is not UTF-8, and treating that as
/// the end of the output would kill a working cloudflared and report that
/// it "exited without giving out a hostname" — which is not what happened.
async fn read_hostname(reader: &mut BufReader<tokio::process::ChildStderr>) -> Option<String> {
    let mut line = Vec::new();

    loop {
        line.clear();

        match reader.read_until(b'\n', &mut line).await {
            // End of output, and no hostname in any of it.
            Ok(0) => return None,
            Ok(_) => {}
            Err(err) => {
                tracing::debug!("cannot read cloudflared's output: {err}");
                return None;
            }
        }

        if let Some(hostname) = hostname_in(&String::from_utf8_lossy(&line)) {
            return Some(hostname);
        }
    }
}

/// Picks the hostname out of a line of cloudflared's banner.
///
/// Matched on the domain rather than on the words around it: the banner
/// is decoration cloudflare changes at will, and it arrives wrapped in box
/// characters and log prefixes. What does not change is that the only
/// `trycloudflare.com` name in the output is the one that was handed out.
///
/// **Every URL on the line is considered, not just the first.** The same
/// banner carries a link to Cloudflare's documentation, and a line that
/// happened to hold both would otherwise read as holding neither — the
/// tunnel then waits out its timeout and is killed for saying nothing.
fn hostname_in(line: &str) -> Option<String> {
    line.match_indices("https://").find_map(|(at, marker)| {
        let rest = &line[at + marker.len()..];

        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))
            .unwrap_or(rest.len());

        // A label under the domain, so that a name merely *ending* in it —
        // `nottrycloudflare.com` — is not taken for one of Cloudflare's.
        rest[..end]
            .strip_suffix(DOMAIN)
            .filter(|prefix| prefix.ends_with('.'))
            .map(|_| rest[..end].to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hostname_is_read_out_of_the_banner() {
        // What cloudflared actually prints, box characters and all.
        let line = "2026-08-16T00:00:00Z INF |  https://restless-mode-1234.trycloudflare.com   \
                    |";

        assert_eq!(
            hostname_in(line).as_deref(),
            Some("restless-mode-1234.trycloudflare.com")
        );
    }

    #[test]
    fn a_url_that_is_not_a_handed_out_name_is_ignored() {
        // cloudflared prints its own links on the way past — a release
        // note, the documentation. Taking one of those as the hostname
        // would publish a route to Cloudflare's website.
        for line in [
            "INF Thank you for trying Cloudflare Tunnel. See https://developers.cloudflare.com/",
            "INF +---------------------------------------------+",
            "INF Version 2026.1.0",
        ] {
            assert_eq!(hostname_in(line), None, "got a hostname from: {line}");
        }
    }

    #[test]
    fn the_name_is_found_past_another_url_on_the_line() {
        // cloudflared's banner carries a link to its own documentation.
        // Reading only the first URL on a line that held both would find
        // no hostname, wait out the timeout, and kill a working tunnel
        // for saying nothing.
        let line = "INF See https://developers.cloudflare.com/ |                     https://restless-mode-1234.trycloudflare.com |";

        assert_eq!(
            hostname_in(line).as_deref(),
            Some("restless-mode-1234.trycloudflare.com")
        );
    }

    #[test]
    fn a_name_merely_ending_in_the_domain_is_not_one_of_them() {
        // `nottrycloudflare.com` is somebody else's.
        assert_eq!(hostname_in("INF https://nottrycloudflare.com/x"), None);
    }

    #[test]
    fn trailing_decoration_is_not_part_of_the_name() {
        // The banner pads to a fixed width and closes with `|`, and a
        // hostname carrying either is one no request will ever match.
        let host = hostname_in("| https://a-b-1.trycloudflare.com |").expect("found");

        assert!(!host.contains(' '), "got: {host}");
        assert!(!host.contains('|'), "got: {host}");
        assert!(host.ends_with(DOMAIN), "got: {host}");
    }

    #[test]
    fn it_asks_for_targets_and_not_for_a_domain() {
        // The two halves of what makes this the other shape of tunnel.
        let needs = QuickProvider::with_program("/bin/sh").needs();

        assert!(needs.targets, "it can only cover what it is told about");
        assert!(!needs.domain, "there is no zone of the user's involved");
    }

    #[test]
    fn there_is_no_record_to_go_and_look_for() {
        let provider = QuickProvider::with_program("/bin/sh");
        let request = TunnelRequest::new("/tmp", 80);

        assert!(provider.dns_record(&request).is_none());
    }

    #[test]
    fn nothing_is_left_in_anybody_s_account() {
        let provider = QuickProvider::with_program("/bin/sh");
        let leftover = provider.leftovers(&TunnelRequest::new("/tmp", 80));

        assert!(leftover.commands.is_empty(), "got: {leftover:?}");
        assert!(leftover.notes.is_empty(), "got: {leftover:?}");
    }

    #[test]
    fn it_never_asks_anyone_to_log_in() {
        // No account is involved, so `NeedsLogin` cannot be reached — and
        // reporting it would be advice about a different provider.
        let provider = QuickProvider::with_program("/bin/sh");

        assert_eq!(
            provider.readiness(&TunnelRequest::new("/tmp", 80)),
            Readiness::Ready,
            "an installed cloudflared is all it takes"
        );
        assert!(provider.missing(&Readiness::NeedsLogin).is_none());
    }

    /// A stand-in that prints a banner the way cloudflared does, then
    /// stays up. `$$` differs per process, which is what makes each
    /// target's name its own.
    fn announcing_stub(dir: &std::path::Path) -> String {
        crate::testing::stub(
            dir,
            r#"echo "2026-08-16T00:00:00Z INF |  https://stub-$$.trycloudflare.com  |" >&2
sleep 30"#,
        )
    }

    fn targets(services: &[&str]) -> Vec<crate::TunnelTarget> {
        services
            .iter()
            .map(|service| crate::TunnelTarget::new("myapp", Some("feat-1"), *service))
            .collect()
    }

    #[tokio::test]
    async fn every_published_service_gets_a_name_of_its_own() {
        // The whole shape of this provider in one test: a process per
        // service, a hostname read back off each one's stderr, and a map
        // the routing table can be built from. None of it is derivable —
        // the names did not exist a second before.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = QuickProvider::with_program(announcing_stub(dir.path()));

        let request = TunnelRequest::new(dir.path(), 80).with_targets(targets(&["web", "api"]));
        let started = provider.start(&request).await.expect("starts");

        let Hostnames::Assigned(names) = started.tunnel.hostnames().clone() else {
            panic!("a quick tunnel hands names out; it has no zone");
        };

        assert_eq!(names.len(), 2, "one per service: {names:?}");
        for (target, host) in &names {
            assert!(host.ends_with(DOMAIN), "{}: got {host}", target.service);
        }

        let distinct: std::collections::BTreeSet<&String> = names.values().collect();
        assert_eq!(
            distinct.len(),
            2,
            "each service is reached on its own name: {names:?}"
        );

        started.tunnel.stop().await;
    }

    #[tokio::test]
    async fn a_service_that_never_announced_a_name_takes_the_tunnel_down() {
        // A hostname that never arrives is a service nothing can reach.
        // Reporting the tunnel as up would advertise a URL that does not
        // exist, so the start fails — and the processes that did come up
        // go with it rather than being left running unreferenced.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = QuickProvider::with_program(crate::testing::stub(dir.path(), "exit 1"));

        let request = TunnelRequest::new(dir.path(), 80).with_targets(targets(&["web"]));
        let Err(err) = provider.start(&request).await else {
            panic!("a tunnel with no hostname must not report success");
        };

        assert!(err.to_string().contains("hostname"), "got: {err}");
    }

    #[tokio::test]
    async fn a_tunnel_that_keeps_logging_keeps_running() {
        // **cloudflared writes to stderr for as long as it is up** — a
        // line per edge connection, then heartbeats. Reading the hostname
        // and letting the pipe go closes the read end under it, and the
        // next write kills the process: Go terminates on SIGPIPE for fd 2.
        // Every URL `tunnel enable` had just printed would stop answering
        // seconds after it returned, with nothing to say why.
        //
        // The stub keeps writing the way the real thing does, and `sh`
        // dies on SIGPIPE just as readily.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = QuickProvider::with_program(crate::testing::stub(
            dir.path(),
            r#"echo "INF https://stub-$$.trycloudflare.com" >&2
while :; do echo "INF still here" >&2; sleep 0.1; done"#,
        ));

        let request = TunnelRequest::new(dir.path(), 80).with_targets(targets(&["web"]));
        let mut started = provider.start(&request).await.expect("starts");

        // Long enough for several writes to have gone past.
        tokio::time::sleep(Duration::from_millis(600)).await;

        assert!(
            started.tunnel.is_running(),
            "a tunnel still logging must still be up"
        );

        started.tunnel.stop().await;
    }

    #[tokio::test]
    async fn a_byte_that_is_not_utf8_is_not_the_end_of_the_output() {
        // Reading by line fails the whole read on one such byte, and
        // treating that as end-of-output would kill a healthy cloudflared
        // and report that it "exited without giving out a hostname" —
        // which is not what happened.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = QuickProvider::with_program(crate::testing::stub(
            dir.path(),
            r#"printf 'INF \377\376 banner
' >&2
echo "INF https://stub-$$.trycloudflare.com" >&2
sleep 30"#,
        ));

        let request = TunnelRequest::new(dir.path(), 80).with_targets(targets(&["web"]));
        let started = provider
            .start(&request)
            .await
            .expect("reads past the bad line");

        started.tunnel.stop().await;
    }

    #[tokio::test]
    async fn services_are_published_together_rather_than_in_a_queue() {
        // Each waits on its own banner from Cloudflare's edge, and the
        // waits are independent. In a row, four services would be four
        // round trips end to end.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = QuickProvider::with_program(crate::testing::stub(
            dir.path(),
            r#"sleep 1
echo "INF https://stub-$$.trycloudflare.com" >&2
sleep 30"#,
        ));

        let request =
            TunnelRequest::new(dir.path(), 80).with_targets(targets(&["web", "api", "docs"]));

        let began = tokio::time::Instant::now();
        let started = provider.start(&request).await.expect("starts");
        let took = began.elapsed();

        assert!(
            took < Duration::from_millis(2500),
            "three one-second waits overlapped into one: took {took:?}"
        );

        started.tunnel.stop().await;
    }

    #[tokio::test]
    async fn a_process_that_dies_takes_running_with_it() {
        // The hostname was announced, so the tunnel started — and then
        // the process went. Somebody is holding a link to that name, and
        // reporting `running` while nothing answers it is the lie this
        // exists to catch. The daemon drops the routes on the back of it.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = QuickProvider::with_program(crate::testing::stub(
            dir.path(),
            r#"echo "INF https://stub-$$.trycloudflare.com" >&2"#,
        ));

        let request = TunnelRequest::new(dir.path(), 80).with_targets(targets(&["web"]));
        let mut started = provider
            .start(&request)
            .await
            .expect("announces, then exits");

        // The stub exits as soon as it has printed; give it a moment to
        // be reaped.
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert!(
            !started.tunnel.is_running(),
            "a quick tunnel whose process has gone reads as down"
        );
    }

    #[test]
    fn notes_are_short_enough_to_sit_in_a_panel() {
        // The same rule the zone's notes follow: a note sets the panel's
        // preferred width and the wrap breaks at the column rather than
        // at a space, so a long line renders hyphen-free mid-word.
        for published in [1, 4] {
            for note in notes_for(&TunnelRequest::new("/tmp", 80), published) {
                assert!(note.len() <= 64, "{} chars: {note}", note.len());
            }
        }
    }

    #[test]
    fn a_restart_is_told_nothing_it_will_not_read() {
        let quiet = TunnelRequest::new("/tmp", 80).explain(false);
        assert!(notes_for(&quiet, 2).is_empty());
    }

    #[tokio::test]
    async fn publishing_nothing_is_refused_rather_than_started() {
        // A quick tunnel with no targets is a process nobody can reach,
        // and it would report `running`.
        let provider = QuickProvider::with_program("/bin/sh");
        let Err(err) = provider.start(&TunnelRequest::new("/tmp", 80)).await else {
            panic!("a quick tunnel with nothing to publish must not start");
        };

        assert!(err.to_string().contains("one service"), "got: {err}");
    }
}

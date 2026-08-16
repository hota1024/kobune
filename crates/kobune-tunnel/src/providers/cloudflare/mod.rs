//! Cloudflare Tunnel.
//!
//! One named tunnel per machine carries every project. Its ingress sends
//! everything to the local proxy and the proxy routes on Host, so
//! workspaces come and go without the tunnel's configuration or DNS being
//! touched (`docs/DESIGN.md` §9).
//!
//! **Nothing interactive runs from here.** `cloudflared tunnel login`
//! opens a browser and waits, which would hang an agent exactly the way an
//! unattended `sudo` does. Login is reported as a step for the user to
//! take; everything after it — creating the tunnel, routing DNS, running
//! it — the daemon does itself.
//!
//! Setup goes through the CLI rather than Cloudflare's HTTP API so there
//! is no API token to obtain, store or scope. The one thing the CLI cannot
//! do is apply an Access policy, which is why exposing a tunnel without
//! one has to be asked for explicitly.

pub mod config;
pub mod process;

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::provider::{
    Access, Leftover, Missing, Needs, Readiness, StartOutcome, Started, TunnelProvider,
    TunnelRequest,
};
use crate::{Result, TunnelError};

use process::StepOutcome;

/// The identifier stored in `TunnelRecord.provider`.
pub const ID: &str = kobune_core::DEFAULT_TUNNEL_PROVIDER;

/// The CLI this drives.
pub const PROGRAM: &str = "cloudflared";

/// Overrides which binary is run.
///
/// For a cloudflared that is installed somewhere [`kobune_core::program`]
/// does not think to look, and for exercising the daemon's tunnel path
/// without a Cloudflare account.
pub const PROGRAM_ENV: &str = "KOBUNE_CLOUDFLARED";

/// The command to run, honouring [`PROGRAM_ENV`].
///
/// Looked up rather than left as a name: the daemon is started by
/// launchd, whose `PATH` holds nothing a package manager can install
/// into, so a bare `cloudflared` reads as missing on a machine that has
/// it.
///
/// **Both providers that drive cloudflared come through here.** The quick
/// tunnel runs the same binary, and a second copy of this would be the
/// one that did not get the next change to how the override is honoured.
pub fn program() -> String {
    kobune_core::program::resolve_with(std::env::var(PROGRAM_ENV).ok().as_deref(), PROGRAM)
}

/// Where `cloudflared tunnel login` leaves its certificate.
///
/// Its presence is what tells us whether login has happened; there is no
/// other way to ask without making a network call.
pub fn login_cert_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| Path::new(&home).join(".cloudflared").join("cert.pem"))
}

/// Cloudflare Tunnel, driven through `cloudflared`.
#[derive(Debug, Clone)]
pub struct CloudflareProvider {
    /// The command to run.
    ///
    /// Looked up rather than left as a name: the daemon is started by
    /// launchd, whose `PATH` holds nothing a package manager can install
    /// into, so a bare `cloudflared` reads as missing on a machine that
    /// has it.
    program: String,
}

impl Default for CloudflareProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CloudflareProvider {
    pub fn new() -> Self {
        Self { program: program() }
    }

    /// Points it at a different binary. For tests, and for [`PROGRAM_ENV`].
    pub fn with_program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    /// The wildcard record that routes the zone's hostnames here.
    ///
    /// One for the whole zone. A tunnel hostname is a single label
    /// ([`kobune_core::naming::tunnel_host`]), so there is no per-project
    /// prefix left to cut a record at — and the ingress rule already
    /// claims the zone, so this is the DNS side saying the same thing.
    /// Projects and worktrees come and go without a DNS write.
    fn wildcard(&self, request: &TunnelRequest) -> Option<String> {
        request
            .domain
            .as_deref()
            .map(|domain| format!("*.{domain}"))
    }
}

#[async_trait]
impl TunnelProvider for CloudflareProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn display_name(&self) -> &'static str {
        "Cloudflare Tunnel"
    }

    fn needs(&self) -> Needs {
        Needs {
            // The zone is the user's and there is no default worth
            // guessing at. Everything else follows from it: the wildcard
            // record, the ingress rule, and every hostname.
            domain: true,
            // The zone covers what does not exist yet, so there is
            // nothing to be told about.
            targets: false,
        }
    }

    fn access(&self) -> Access {
        // **Kobune cannot apply the policy and cannot see one.** Access is
        // configured through Cloudflare's API, and everything here goes
        // through the CLI so there is no token to obtain, scope or store
        // (`docs/DESIGN.md` §9). What it can say is that the hostname is
        // under a zone of yours, so there is something to put a policy on.
        Access::Unknown {
            policy: "a Cloudflare Access policy".to_string(),
        }
    }

    fn readiness(&self, _request: &TunnelRequest) -> Readiness {
        // Looked up rather than spawned: readiness is reported before
        // anything is run, and "not installed" has to be answerable
        // without running it.
        if kobune_core::program::find(&self.program).is_none() {
            return Readiness::NotInstalled;
        }

        match login_cert_path() {
            Some(path) if path.is_file() => Readiness::Ready,
            // Without a home directory there is nowhere for the
            // certificate to be, so treat it as absent rather than guess.
            _ => Readiness::NeedsLogin,
        }
    }

    fn missing(&self, readiness: &Readiness) -> Option<Missing> {
        match readiness {
            Readiness::NotInstalled => Some(Missing {
                summary: format!("{PROGRAM} is not installed"),
                // The bare name, not [`Self::program`]: there is nothing
                // on the machine for the lookup to have resolved, so what
                // it holds is the name back again — and this is a command
                // to install it, where the name is the argument.
                commands: vec![format!("brew install {PROGRAM}")],
            }),
            Readiness::NeedsLogin => Some(Missing {
                summary: format!("{PROGRAM} is not logged in"),
                // The resolved path, because this one is run. A machine
                // with cloudflared outside its `PATH` is exactly the case
                // the lookup exists for, and a bare name would be a
                // command that does not work when copied.
                commands: vec![format!("{} tunnel login", self.program)],
            }),
            Readiness::Ready => None,
        }
    }

    fn dns_record(&self, request: &TunnelRequest) -> Option<String> {
        self.wildcard(request)
    }

    async fn start(&self, request: &TunnelRequest) -> Result<Started> {
        let domain = zone(request)?;
        let (tunnel, dns) = process::start(&self.program, request, domain).await?;
        let outcome = confirm(request, domain, dns).await;

        Ok(Started {
            tunnel: Box::new(tunnel),
            outcome,
        })
    }

    fn leftovers(&self, request: &TunnelRequest) -> Leftover {
        let Some(wildcard) = self.wildcard(request) else {
            // The named tunnel is still there to delete even when the
            // record has lost its domain; the DNS note is what cannot be
            // written without one.
            return Leftover {
                commands: vec![format!("{PROGRAM} tunnel delete --force {}", request.name)],
                notes: Vec::new(),
            };
        };

        Leftover {
            commands: vec![format!("{PROGRAM} tunnel delete --force {}", request.name)],
            // The DNS record is named as well as the tunnel. Left behind
            // pointing at a tunnel that no longer exists, a record answers
            // with Cloudflare's error 1033 — worse than the NXDOMAIN it
            // replaced, and it does not expire.
            notes: vec![format!(
                "the DNS record {wildcard} has no command — `{PROGRAM} tunnel \
                 route dns` only creates. Remove it in the Cloudflare dashboard"
            )],
        }
    }
}

/// The zone every step here needs.
///
/// [`Needs::domain`] says this provider has to be given one, so the daemon
/// refuses before it gets here. This is the guard for the path that does
/// not go through the daemon — a record edited by hand, or a caller yet to
/// be written — and it names the fix rather than panicking on an `unwrap`
/// somebody would have to read a backtrace to understand.
fn zone(request: &TunnelRequest) -> Result<&str> {
    request.domain.as_deref().ok_or_else(|| {
        TunnelError::failed(
            "starting the tunnel",
            "no Cloudflare zone. Name it with `--domain example.com`",
        )
    })
}

/// Maps a failure onto something the daemon can classify.
///
/// Kept here rather than in the daemon: which failures mean "install
/// something" and which mean "log in" is the provider's to know, and the
/// hint names its own program.
pub fn error_hint(err: &TunnelError) -> Option<String> {
    match err {
        TunnelError::NotInstalled(_) => Some(format!("install {PROGRAM} (brew install {PROGRAM})")),
        TunnelError::NotLoggedIn => Some(format!("run `{PROGRAM} tunnel login`")),
        _ => None,
    }
}

/// What to report about a run, asking the resolver only if it will be read.
///
/// **The lookup is the point of the guard, not an optimisation.** A zone
/// Kobune has already confirmed has nothing left to say, and a daemon
/// bringing a tunnel back at start-up has nobody to say it to and nowhere
/// to persist the answer — so in both cases the round trip buys nothing.
async fn confirm(request: &TunnelRequest, domain: &str, dns: StepOutcome) -> StartOutcome {
    if request.settled || !request.explain {
        return StartOutcome::unchanged(request);
    }

    let resolves = process::wildcard_resolves(domain).await;

    StartOutcome {
        notes: zone_notes(domain, dns, resolves),
        // **Only when the route took effect**, which means both that
        // cloudflared accepted it and that the name answers. Either half
        // alone silences the warning for a zone that is still broken:
        // `AlreadyThere` would claim a record Kobune did not put there,
        // and `Done` on its own is what a domain outside the login's zone
        // returns while resolving nowhere. The warning has to outlast one
        // run, because so does the problem.
        settled: request.settled || (dns == StepOutcome::Done && resolves),
    }
}

/// What `enable` should say about the zone, given what routing did.
///
/// Only about the transition. Both setup steps run on every enable, so
/// repeating any of this once a zone is known good turns it into noise,
/// and a warning that is always there is a warning nobody reads.
///
/// **One short line per element**, not a paragraph. A line here sets the
/// preferred width of the panel it lands in, and that wrap breaks at the
/// column rather than at a space — deliberately, since what usually
/// overflows a panel is a path or a command. Prose has to arrive
/// pre-broken.
fn zone_notes(domain: &str, dns: StepOutcome, resolves: bool) -> Vec<String> {
    let wildcard = format!("*.{domain}");

    // **The name does not answer, whatever cloudflared said.** This is
    // what a domain that is not the login's zone looks like from here:
    // `route dns` takes the hostname as relative to the zone the
    // certificate covers, creates `*.{domain}.{that zone}`, and exits 0.
    // Nothing else in the response can tell — `running` is true, the
    // tunnel is up, and no URL under `domain` will ever arrive.
    if !resolves {
        return vec![
            format!("{wildcard} does not resolve, so nothing arrives."),
            "The likely cause is that it is not the zone your".to_string(),
            "`cloudflared tunnel login` covers: the record is then".to_string(),
            format!("created as {wildcard}.<that zone> and this reports"),
            "success. Check the zone in the Cloudflare dashboard.".to_string(),
        ];
    }

    let mut notes = match dns {
        // The record reaches this tunnel and the name answers. Worth
        // saying once what that covers: it answers for every name in the
        // zone with none of its own, so a name that used to be NXDOMAIN
        // now reaches this machine — including the ones an ACME HTTP-01
        // challenge uses.
        StepOutcome::Done => vec![
            format!("{wildcard} now points here."),
            "Names with a record of their own are unaffected;".to_string(),
            "any other name in the zone reaches this machine.".to_string(),
        ],

        // Someone else's record, or one from an earlier install Kobune has
        // no memory of. It resolves, but cloudflared only says the name is
        // taken, not what it points at, and if it is not this tunnel then
        // nothing arrives and everything above still reports `running`.
        StepOutcome::AlreadyThere => vec![
            format!("a DNS record for {wildcard} was already there,"),
            "and Kobune did not create it. If it does not point".to_string(),
            "at this tunnel, no hostname will arrive.".to_string(),
        ],
    };

    // A resolving wildcard says the zone is right, but not that the
    // certificate reaches. Universal SSL covers one level below the zone,
    // so a domain that is itself a subdomain puts every hostname out of
    // range — a TLS handshake failure with everything here still saying
    // `running`. Kobune cannot tell a zone from a subdomain of one without
    // the public suffix list, so this asks rather than refuses: getting
    // `example.co.uk` wrong would be worse than the question.
    if domain.split('.').count() > 2 {
        notes.push(format!(
            "if {domain} is not the zone itself, https will fail:"
        ));
        notes.push("its certificate covers one level below the zone.".to_string());
    }

    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(domain: &str) -> TunnelRequest {
        TunnelRequest::new("/tmp/kobune-tunnel", 80).with_domain(Some(domain.to_string()))
    }

    /// The wildcard answers — the case where the note is about scope.
    const RESOLVES: bool = true;

    fn joined(notes: &[String]) -> String {
        notes.join(" ")
    }

    #[test]
    fn dns_record_is_one_wildcard_for_the_zone() {
        // Per project or per workspace would mean a DNS write every time
        // one appeared, which is what the wildcard exists to avoid — and
        // a flat hostname has no label to cut a narrower record at.
        let provider = CloudflareProvider::with_program("/bin/sh");

        assert_eq!(
            provider.dns_record(&request("example.com")).as_deref(),
            Some("*.example.com")
        );
    }

    #[test]
    fn setup_only_asks_for_the_interactive_step() {
        // Everything else the daemon does itself. Asking the user to run
        // more than they must is how setup instructions go stale.
        let provider = CloudflareProvider::with_program("/bin/sh");
        let missing = provider
            .missing(&Readiness::NeedsLogin)
            .expect("something is missing");

        assert_eq!(missing.commands.len(), 1);
        assert!(
            missing.commands[0].contains("tunnel login"),
            "got: {missing:?}"
        );
    }

    #[test]
    fn a_ready_provider_asks_for_nothing() {
        let provider = CloudflareProvider::with_program("/bin/sh");
        assert!(provider.missing(&Readiness::Ready).is_none());
    }

    #[test]
    fn missing_program_is_reported_before_anything_runs() {
        let provider = CloudflareProvider::with_program("kobune-definitely-not-a-real-program");

        assert_eq!(
            provider.readiness(&request("example.com")),
            Readiness::NotInstalled
        );
    }

    #[test]
    fn an_absolute_program_path_is_used_as_given() {
        // Tests point the program at a stub script, which is not on PATH.
        let provider = CloudflareProvider::with_program("/bin/sh");

        assert_ne!(
            provider.readiness(&request("example.com")),
            Readiness::NotInstalled
        );
    }

    #[test]
    fn the_program_can_be_pointed_elsewhere() {
        use kobune_core::program::resolve_with;

        // For a cloudflared installed somewhere the lookup does not think
        // to look, and the hook the daemon's own tunnel path is exercised
        // through. Read through the helper rather than the variable: a test
        // that set one would race every other test in the crate, since they
        // all build a provider and building one reads it.
        assert_eq!(
            resolve_with(Some("/opt/custom/cloudflared"), PROGRAM),
            "/opt/custom/cloudflared"
        );

        // Whichever way it lands, it is cloudflared: an absolute path on a
        // machine that has one, the bare name on a machine that does not.
        assert!(resolve_with(None, PROGRAM).ends_with(PROGRAM));
        assert!(resolve_with(Some(""), PROGRAM).ends_with(PROGRAM));
    }

    #[test]
    fn an_uninstall_names_the_record_it_cannot_remove() {
        let provider = CloudflareProvider::with_program("/bin/sh");
        let leftover = provider.leftovers(&request("example.com"));

        assert!(
            leftover.commands.iter().any(|cmd| cmd.contains("delete")),
            "got: {leftover:?}"
        );
        assert!(
            joined(&leftover.notes).contains("*.example.com"),
            "the record has no command, so it is described: {leftover:?}"
        );
    }

    #[test]
    fn a_record_someone_else_owns_is_called_out() {
        // The failure this prevents is total and silent: the tunnel runs,
        // `status` says running, and every hostname resolves to whatever
        // the pre-existing record points at instead.
        let notes = zone_notes("example.com", StepOutcome::AlreadyThere, RESOLVES);
        let text = joined(&notes);

        assert!(text.contains("*.example.com"), "got: {notes:?}");
        assert!(text.contains("did not create it"), "got: {notes:?}");
    }

    #[test]
    fn a_wildcard_that_does_not_resolve_outranks_cloudflared_s_success() {
        // Seen in the wild: `--domain` naming a zone the cloudflared login
        // does not cover. `route dns` takes the hostname as relative to
        // the zone the certificate is scoped to, creates
        // `*.other.example.com`, and exits 0 — so `Done` here means
        // nothing, and every URL under `other` is unreachable while the
        // tunnel reports `running`.
        let notes = zone_notes("other", StepOutcome::Done, false);
        let text = joined(&notes);

        assert!(text.contains("does not resolve"), "got: {notes:?}");
        assert!(
            text.contains("login"),
            "it names the likely cause: {notes:?}"
        );
        assert!(
            !text.contains("now points here"),
            "it must not also claim success: {notes:?}"
        );
    }

    #[test]
    fn notes_are_short_enough_to_sit_in_a_panel() {
        // A note sets the panel's preferred width, and the wrap breaks at
        // the column rather than at a space, so a long line is rendered
        // hyphen-free mid-word.
        for outcome in [StepOutcome::Done, StepOutcome::AlreadyThere] {
            for resolves in [true, false] {
                for note in zone_notes("example.com", outcome, resolves) {
                    assert!(note.len() <= 64, "{} chars: {note}", note.len());
                }
            }
        }
    }

    #[test]
    fn a_domain_below_the_zone_is_questioned() {
        // Getting this wrong reproduces the handshake failure the flat
        // hostname exists to avoid, and nothing else would show it.
        let notes = zone_notes("dev.example.com", StepOutcome::Done, RESOLVES);

        assert!(
            joined(&notes).contains("is not the zone itself"),
            "got: {notes:?}"
        );
    }

    #[test]
    fn a_two_label_domain_is_not_questioned() {
        let notes = zone_notes("example.com", StepOutcome::Done, RESOLVES);

        assert!(
            !joined(&notes).contains("is not the zone"),
            "got: {notes:?}"
        );
    }

    #[test]
    fn routing_the_record_says_what_it_now_covers() {
        let notes = zone_notes("example.com", StepOutcome::Done, RESOLVES);

        assert!(joined(&notes).contains("*.example.com"), "got: {notes:?}");
    }

    #[tokio::test]
    async fn a_zone_already_routed_by_kobune_says_nothing() {
        // Both setup steps run on every enable and on every daemon start.
        // A warning that appears every time is a warning nobody reads.
        //
        // These return before the resolver is asked, which is the other
        // half of the guarantee: no lookup happens for an answer nobody
        // would read.
        for outcome in [StepOutcome::Done, StepOutcome::AlreadyThere] {
            let settled = confirm(
                &request("example.com").settled(true),
                "example.com",
                outcome,
            )
            .await;

            assert!(settled.notes.is_empty(), "the usual case is silent");
            assert!(settled.settled, "and it stays settled");
        }
    }

    #[tokio::test]
    async fn a_restart_is_not_something_to_explain_to() {
        // The daemon brings a tunnel back with nobody watching and does
        // not write the answer down, so working one out is a round trip
        // for nobody.
        let restoring = request("example.com").explain(false);
        let outcome = confirm(&restoring, "example.com", StepOutcome::Done).await;

        assert!(outcome.notes.is_empty(), "got: {outcome:?}");
        assert!(
            !outcome.settled,
            "and nothing is claimed about a zone it did not check"
        );
    }
}

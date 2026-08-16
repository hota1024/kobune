//! The interface every tunnel service shares.
//!
//! A tunnel is defined as "something that makes the local proxy reachable
//! from outside, and **says which hostnames arrive**". That second half is
//! the crux, the way `RunningService::endpoint` is for `kobune-runtime`:
//! ask [`RunningTunnel::hostnames`] where a service answers and the
//! difference between a zone that was always yours and a name handed out
//! thirty seconds ago stops being visible from the outside.
//!
//! The two shapes are genuinely different, and it is [`Hostnames`] that
//! absorbs them:
//!
//! | | Wildcard | Assigned |
//! | --- | --- | --- |
//! | Where names come from | derived, free | handed out, one call each |
//! | Whose domain | the user's | the service's |
//! | A new workspace | costs nothing | is not covered until asked for |

use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;

use crate::Result;

/// One thing a tunnel publishes.
///
/// Enough to name a service in a workspace of a project, which is what a
/// hostname identifies either way — derived into one for a wildcard, or
/// looked up in what the service handed back.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TunnelTarget {
    pub project: String,
    /// `None` for the main worktree, whose label is left out of URLs.
    pub workspace: Option<String>,
    pub service: String,
}

impl TunnelTarget {
    pub fn new(
        project: impl Into<String>,
        workspace: Option<&str>,
        service: impl Into<String>,
    ) -> Self {
        Self {
            project: project.into(),
            workspace: workspace.map(str::to_string),
            service: service.into(),
        }
    }
}

/// What arrives from outside, and for what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hostnames {
    /// Every name under the zone reaches here.
    ///
    /// Names are derived rather than requested, so a workspace that
    /// appears a minute from now is already covered.
    Wildcard { domain: String },

    /// One name per target, as the service handed them out.
    ///
    /// A target that is not in the map is not reachable. That is not a
    /// failure — it is what a service that gives out one name at a time
    /// means, and it is why the free ones cannot cover a workspace nobody
    /// had made yet.
    Assigned(BTreeMap<TunnelTarget, String>),
}

impl Hostnames {
    /// Where this target answers from outside, if it does.
    ///
    /// **The one call the daemon makes about hostnames.** Deriving them
    /// instead would be the layer above deciding a thing only the service
    /// knows: Cloudflare's free certificate is what flattens a name to one
    /// label, and a service handing out `restless-mode-1234.example.net`
    /// has no rule to apply at all.
    pub fn host_for(&self, target: &TunnelTarget) -> Option<String> {
        match self {
            Self::Wildcard { domain } => Some(kobune_core::naming::tunnel_host(
                &target.service,
                target.workspace.as_deref(),
                &target.project,
                domain,
            )),
            Self::Assigned(names) => names.get(target).cloned(),
        }
    }

    /// The zone, for a provider that has one.
    ///
    /// For the places that describe the tunnel rather than route through
    /// it. `None` says there is no zone to name, not that nothing works.
    pub fn domain(&self) -> Option<&str> {
        match self {
            Self::Wildcard { domain } => Some(domain),
            Self::Assigned(_) => None,
        }
    }
}

/// What to run a tunnel for.
///
/// The provider-neutral half of the ask. How to do any of it — a CLI to
/// drive, a certificate to find, an API to call — belongs to the provider
/// and never appears here.
#[derive(Debug, Clone)]
pub struct TunnelRequest {
    /// What this machine's tunnel goes by.
    ///
    /// One per machine, carrying every project, so reusing the name is
    /// what makes `tunnel enable` idempotent across them.
    pub name: String,

    /// The zone the hostnames live under (`example.com`).
    ///
    /// `None` for a service that has no zone of yours to put names under
    /// — it hands out names in its own domain instead, and there is
    /// nothing for the user to have named. [`Needs::domain`] says which.
    pub domain: Option<String>,

    /// What to publish, for a provider that cannot work it out.
    ///
    /// Empty for a wildcard, which covers what does not exist yet and so
    /// has nothing to be told. [`Needs::targets`] says which.
    pub targets: Vec<TunnelTarget>,

    /// Where generated configuration and logs go.
    pub dir: PathBuf,

    /// The local proxy's plain-HTTP port.
    ///
    /// The hop from the tunnel to the proxy stays on loopback and TLS is
    /// terminated at the provider's edge, so it is plain HTTP. Pointing
    /// at the HTTPS port instead would mean the provider verifying
    /// Kobune's local CA, which it has no reason to trust.
    pub local_port: u16,

    /// Whether this provider has already confirmed its own setup took.
    ///
    /// Persisted as `TunnelRecord.zone_routed`, whose name is
    /// Cloudflare's and survives only because renaming a field strands
    /// every state file already on disk. What it means here is the part
    /// that generalises: a provider that has once seen its setup work
    /// says nothing more about it, and a warning that appears on every
    /// run is one nobody reads.
    pub settled: bool,

    /// Whether anyone is waiting to be told what happened.
    ///
    /// False when the daemon is bringing a tunnel back at start-up, where
    /// there is no reply for a note to travel in and nothing persists the
    /// answer. A provider that would spend a round trip working out what
    /// to say can skip it and return [`StartOutcome::unchanged`].
    pub explain: bool,
}

impl TunnelRequest {
    pub fn new(dir: impl Into<PathBuf>, local_port: u16) -> Self {
        Self {
            name: crate::DEFAULT_TUNNEL_NAME.to_string(),
            domain: None,
            targets: Vec::new(),
            dir: dir.into(),
            local_port,
            settled: false,
            explain: true,
        }
    }

    pub fn with_domain(mut self, domain: Option<String>) -> Self {
        self.domain = domain;
        self
    }

    pub fn with_targets(mut self, targets: Vec<TunnelTarget>) -> Self {
        self.targets = targets;
        self
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn settled(mut self, settled: bool) -> Self {
        self.settled = settled;
        self
    }

    pub fn explain(mut self, explain: bool) -> Self {
        self.explain = explain;
        self
    }
}

/// How far along setup is.
///
/// Everything before [`Self::Ready`] needs the user, so each is reported
/// rather than attempted — `cloudflared tunnel login` opens a browser and
/// waits, which would hang an agent exactly the way an unattended `sudo`
/// does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// What the provider needs to run is not on the machine.
    NotInstalled,
    /// It is there, but nothing has authorised it yet.
    NeedsLogin,
    /// The daemon can take it from here.
    Ready,
}

impl Readiness {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// What stands between the internet and the environment.
///
/// **The `--public` gate reads this rather than deciding it.** Kobune
/// refuses to publish an environment nothing is guarding without being
/// told to, and until there were two providers that refusal came with one
/// piece of advice — put a Cloudflare Access policy in front of it. On a
/// quick tunnel that advice cannot be followed: the hostname is
/// Cloudflare's, not yours, and there is nothing to attach a policy to.
/// Telling somebody to do a thing they cannot do is worse than saying
/// nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// Kobune puts authentication in front of it itself.
    ///
    /// **Nothing produces this yet**, and it is here because it is the
    /// one shape the gate has to have room for: a service Kobune runs can
    /// promise what a CLI cannot, and the promise is the difference
    /// between "acknowledge the risk" and "there is no risk to
    /// acknowledge". The daemon's branch for it is tested against a
    /// stand-in rather than left to be discovered.
    Managed,

    /// None that Kobune can see, on a hostname that is yours to protect.
    ///
    /// The string is what to go and do, in the provider's own words.
    Unknown { policy: String },

    /// None to be had. Anyone with the URL reaches the environment.
    ///
    /// The hostname belongs to the service, so there is nothing the user
    /// could put a policy on even if they wanted to.
    Open,
}

impl Access {
    /// Whether publishing has to be acknowledged out loud.
    pub fn needs_acknowledging(&self) -> bool {
        !matches!(self, Self::Managed)
    }
}

/// What a provider has to be given before it can be asked for anything.
///
/// **This is the structural gap, written down.** A wildcard is handed a
/// zone and works out the rest; a service that gives out one name at a
/// time has a domain of its own and can only cover what it was told
/// about. Everything else about the two is the same, and everything above
/// reads these two booleans rather than knowing which is which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Needs {
    /// A zone of the user's, named on the command line.
    pub domain: bool,
    /// The list of services to publish, because a name has to be asked
    /// for per service and a workspace made later is not covered.
    pub targets: bool,
}

/// What is missing, in the provider's own words.
///
/// **The provider says this, not the daemon.** "cloudflared is not
/// installed" is the right sentence for exactly one provider, and a
/// `doctor` that printed it about another would send someone looking for
/// a binary that has nothing to do with their problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    /// One line: what is not there.
    pub summary: String,
    /// What the user runs themselves to fix it.
    pub commands: Vec<String>,
}

/// A tunnel that is up.
#[async_trait]
pub trait RunningTunnel: Send + Sync {
    /// Which names arrive, now that it is up.
    ///
    /// **Only answerable once it is running**, which is why it is here and
    /// not on the provider: a service that hands out names has not handed
    /// out any until something connected.
    fn hostnames(&self) -> &Hostnames;

    /// Whether traffic can still flow.
    ///
    /// A tunnel dying on its own — a revoked credential, a deleted
    /// tunnel — otherwise goes unnoticed while every URL through it stops
    /// working and `status` still says `running`.
    fn is_running(&mut self) -> bool;

    /// Takes it down.
    async fn stop(self: Box<Self>);
}

/// A tunnel that has just been started, and what setting it up did.
pub struct Started {
    pub tunnel: Box<dyn RunningTunnel>,
    pub outcome: StartOutcome,
}

/// What `start` should be reported as having done.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartOutcome {
    /// What to say about this run, once.
    ///
    /// **One short line per element**, not a paragraph: a note sets the
    /// width of the panel it is printed in, and the wrap there breaks at
    /// the column rather than at a space. Prose has to arrive pre-broken.
    pub notes: Vec<String>,

    /// Whether the provider now considers its own setup confirmed.
    ///
    /// Stored, and handed back as [`TunnelRequest::settled`] next time.
    /// Only ever moves towards `true`: the problem a note describes
    /// outlasts the run that found it, so the silence has to be earned
    /// once rather than assumed every time.
    pub settled: bool,
}

impl StartOutcome {
    /// Nothing to report, and nothing learned.
    pub fn unchanged(request: &TunnelRequest) -> Self {
        Self {
            notes: Vec::new(),
            settled: request.settled,
        }
    }
}

/// What stays behind in the user's own account after an uninstall.
///
/// Kobune takes down the local half and reports this rather than reaching
/// into somebody's account uninvited — no other command in the project
/// does, and an uninstaller is the last place to start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Leftover {
    /// What to run to remove it, if that is wanted.
    pub commands: Vec<String>,
    /// What is left that no command here removes.
    ///
    /// Kept apart from [`Self::commands`] so that list stays runnable: a
    /// prose line with a `#` in front of it is not a command and reads as
    /// one.
    pub notes: Vec<String>,
}

/// A tunnel service.
#[async_trait]
pub trait TunnelProvider: Send + Sync {
    /// The identifier stored in `TunnelRecord.provider`.
    fn id(&self) -> &'static str;

    /// What to call it in front of a person.
    fn display_name(&self) -> &'static str;

    /// What has to be in the request before this provider can run.
    fn needs(&self) -> Needs;

    /// What guards the environment once this is up.
    fn access(&self) -> Access;

    /// How far setup has got. **Runs nothing.**
    ///
    /// `status` reports readiness before anything is attempted, so
    /// "not installed" has to be answerable without attempting it.
    fn readiness(&self, request: &TunnelRequest) -> Readiness;

    /// What is missing, for a readiness that is not [`Readiness::Ready`].
    fn missing(&self, readiness: &Readiness) -> Option<Missing>;

    /// The DNS record this provider routes, when it routes one at all.
    ///
    /// Shown so that "nothing resolves" has an answer — the record to go
    /// and look for. A provider that hands out its own hostnames has no
    /// such record and says `None`.
    fn dns_record(&self, request: &TunnelRequest) -> Option<String>;

    /// Sets the tunnel up and starts it.
    ///
    /// **Idempotent.** This runs on every enable and on every daemon
    /// start, so "it is already there" has to read as success — the
    /// alternative is a stored flag trusted over what the service
    /// actually has.
    async fn start(&self, request: &TunnelRequest) -> Result<Started>;

    /// What an uninstall cannot remove for the user.
    fn leftovers(&self, request: &TunnelRequest) -> Leftover;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(workspace: Option<&str>) -> TunnelTarget {
        TunnelTarget::new("myapp", workspace, "web")
    }

    #[test]
    fn a_wildcard_derives_a_name_for_anything_asked_of_it() {
        // Nothing was requested and nothing has to be: the zone already
        // reaches here, so a workspace made a minute from now answers too.
        let hostnames = Hostnames::Wildcard {
            domain: "example.com".into(),
        };

        assert_eq!(
            hostnames.host_for(&target(Some("feat-1"))).as_deref(),
            Some("web-feat-1-myapp.example.com")
        );
        assert_eq!(
            hostnames.host_for(&target(None)).as_deref(),
            Some("web-myapp.example.com"),
            "the main worktree keeps its shorter name"
        );
    }

    #[test]
    fn an_assigned_name_is_used_exactly_as_handed_out() {
        // There is no rule to apply. Deriving anything here would invent a
        // hostname the service has never heard of.
        let hostnames = Hostnames::Assigned(BTreeMap::from([(
            target(Some("feat-1")),
            "restless-mode-1234.trycloudflare.com".to_string(),
        )]));

        assert_eq!(
            hostnames.host_for(&target(Some("feat-1"))).as_deref(),
            Some("restless-mode-1234.trycloudflare.com")
        );
    }

    #[test]
    fn a_target_nobody_handed_out_a_name_for_is_unreachable() {
        // **Not a failure.** It is what a service that gives out one name
        // at a time means, and the routing table has to leave the name off
        // rather than guess one that would 404 at the edge.
        let hostnames = Hostnames::Assigned(BTreeMap::from([(
            target(Some("feat-1")),
            "restless-mode-1234.trycloudflare.com".to_string(),
        )]));

        assert!(hostnames.host_for(&target(Some("feat-2"))).is_none());
        assert!(
            hostnames
                .host_for(&TunnelTarget::new("myapp", Some("feat-1"), "api"))
                .is_none(),
            "and it is per service, not per workspace"
        );
    }

    #[test]
    fn only_a_wildcard_has_a_zone_to_name() {
        assert_eq!(
            Hostnames::Wildcard {
                domain: "example.com".into()
            }
            .domain(),
            Some("example.com")
        );
        assert_eq!(Hostnames::Assigned(BTreeMap::new()).domain(), None);
    }
}

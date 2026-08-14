//! What `minato doctor` asks, and how each answer is worded.
//!
//! **A check is not a status line.** Every one of these carries what it
//! found *and* what to do about it, because the whole reason someone runs
//! `doctor` is that something is wrong and the error they already saw did
//! not say enough. A check that reports a problem with no fix has failed
//! at its job.

use minato_api::{ApiError, Check, Diagnostics, Pong, Response, Target, TunnelState};

use crate::gateway::BindFailure;
use crate::tunnel;

use super::Supervisor;

impl Supervisor {
    pub(super) async fn ping(&self) -> Result<Response, ApiError> {
        // Every reachable runtime, not just Docker. Which one a project
        // uses is its own business, and a handshake that named Docker on a
        // machine running Apple Container was simply wrong.
        let mut reachable = Vec::new();
        for id in minato_runtime::AVAILABLE_RUNTIMES {
            if let Some(info) = self.probe_runtime(id).await {
                reachable.push(format!("{} {}", info.id, info.version));
            }
        }

        Ok(Response::Pong(Pong {
            // With the commit, not just the crate version: it is what the
            // CLI compares against its own to notice a daemon left running
            // from the build an update replaced.
            version: crate::version(),
            protocol: minato_api::PROTOCOL_VERSION,
            runtime: if reachable.is_empty() {
                "none reachable".to_string()
            } else {
                reachable.join(", ")
            },
            uptime_secs: self.started_at.elapsed().as_secs(),
        }))
    }
    /// Probes a runtime, treating any failure as "not reachable".
    ///
    /// Both a runtime that cannot be constructed and one that cannot be
    /// reached mean the same thing to a caller, and neither is worth an
    /// error of its own.
    async fn probe_runtime(&self, id: &str) -> Option<minato_runtime::RuntimeInfo> {
        self.runtime(id).await.ok()?.probe().await.ok()
    }
    /// Diagnoses what the daemon can see.
    ///
    /// System-side settings — `/etc/resolver`, whether the CA is trusted —
    /// are hard to judge from here, so the CLI covers those. This looks at
    /// the listeners and the runtime.
    pub(super) async fn doctor(&self, target: Target) -> Result<Response, ApiError> {
        let mut checks = Vec::new();

        // Read once: it decides both what the proxy checks advise and what
        // the launchd check says, and those two have to agree.
        //
        // **`is_loaded`, not `is_installed`**, which is what the launchd
        // check below has always asked. A plist copied in without a
        // `bootstrap` behind it leaves launchd holding nothing, so a port
        // in use is somebody else's — and the two checks in one `doctor`
        // used to answer that machine with "launchd may be holding this
        // port" and "launchd does not have the job" at once.
        let launchd_has_the_job = minato_core::launchd::is_loaded();

        // **Resolved once.** This walks git, finds the configuration and
        // registers the project in the state store — a write, under the
        // state lock — so asking twice would do all of it twice to learn
        // two fields of the same answer.
        //
        // Diagnosing a machine with no project is still worth doing —
        // that is often *why* someone runs doctor — so failing to resolve
        // is not an error here.
        let project = self.resolve_project_only(&target).await.ok();

        let configured = project
            .as_ref()
            .map(|context| context.config.runtime.default.clone())
            .unwrap_or_else(|| "docker".to_string());

        checks.extend(self.runtime_checks(&configured).await);

        checks.push(match self.gateway.http_port() {
            Some(port) => Check::ok(
                "proxy-http",
                "HTTP proxy",
                listening_detail(port, self.gateway.http_fell_back()),
            ),
            None => {
                let failure = self.gateway.http_failure();
                Check::fail("proxy-http", "HTTP proxy", detail_for(failure)).with_fix(bind_fix(
                    failure,
                    crate::gateway::HTTP_PORT_ENV,
                    launchd_has_the_job,
                ))
            }
        });

        // With only one address family bound, requests to the other reach
        // some different process. Passing over that silently leaves the
        // cause impossible to find.
        let missing = self.gateway.missing_families();
        if !missing.is_empty() {
            // Which proxy is short, not just which address. They bind
            // separately, so "[::1] could not be held" leaves you looking
            // at the wrong one half the time.
            let gaps: Vec<String> = missing
                .iter()
                .map(|(proxy, family)| format!("{proxy} could not hold {}", bracketed(*family)))
                .collect();

            checks.push(
                Check::fail(
                    "proxy-families",
                    "listening addresses",
                    format!(
                        "{}. *.localhost resolves to both families and clients \
                         prefer IPv6, so requests to that address reach \
                         another process",
                        gaps.join("; ")
                    ),
                )
                .with_fix(
                    "stop whatever else is on that address, or name free ports \
                     with MINATO_HTTP_PORT and MINATO_HTTPS_PORT",
                ),
            );
        }

        checks.push(match self.gateway.https_port() {
            Some(port) => Check::ok(
                "proxy-https",
                "HTTPS proxy",
                listening_detail(port, self.gateway.https_fell_back()),
            ),
            None => {
                let failure = self.gateway.https_failure();
                Check::warn(
                    "proxy-https",
                    "HTTPS proxy",
                    format!("{}; HTTP only", detail_for(failure)),
                )
                .with_fix(bind_fix(
                    failure,
                    crate::gateway::HTTPS_PORT_ENV,
                    launchd_has_the_job,
                ))
            }
        });

        // Apple Container's containers reach the host at their network's
        // gateway and nowhere else, so a proxy that is not listening there
        // is one no container can call. Only worth saying where that is
        // the runtime in use: the address exists on any machine with Apple
        // Container installed, and a Docker project never goes near it.
        let unreachable = self.gateway.unreachable_from_containers();
        if !unreachable.is_empty() && minato_runtime::display_name(&configured) == "Apple Container"
        {
            checks.push(
                Check::warn(
                    "container-reach",
                    "reachable from containers",
                    format!(
                        "the proxy is not listening on {}, where containers                          reach the host, so a MINATO_URL_<SERVICE> resolves                          to nothing from inside one",
                        unreachable
                            .iter()
                            .map(|ip| bracketed(*ip))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
                .with_fix(
                    "run `minato setup` again: the launchd job holds the                      privileged ports, and its plist names the addresses it                      holds them on",
                ),
            );
        }

        checks.push(match self.gateway.dns_port() {
            Some(port) => Check::ok("dns", "DNS server", format!("127.0.0.1:{port}")),
            None => {
                let failure = self.gateway.dns_failure();
                Check::fail(
                    "dns",
                    "DNS server",
                    format!("{}; *.localhost will not resolve", detail_for(failure)),
                )
                .with_fix(bind_fix(
                    failure,
                    crate::gateway::DNS_PORT_ENV,
                    launchd_has_the_job,
                ))
            }
        });

        // Whether privileged ports work comes down to whether launchd
        // handed over any descriptors.
        //
        // **A job launchd has, sitting idle, is its own state**, and the one
        // `minato daemon stop` leaves behind. Telling that apart from "never
        // set up" is the difference between a fix that works and being sent
        // back to a `minato setup` that is already done.
        //
        // The plist being on disk is not enough to say which it is: one
        // copied in without a `bootstrap` behind it leaves launchd knowing
        // nothing about the job, and `kickstart` no service to name. That is
        // the install case, so `is_loaded` rather than `is_installed` — and
        // it keeps this in step with what `minato setup` offers.
        checks.push(if crate::activation::is_active() {
            Check::ok(
                "launchd",
                "launchd socket activation",
                "active (privileged ports are available)".to_string(),
            )
        } else if launchd_has_the_job {
            Check::warn(
                "launchd",
                "launchd socket activation",
                "inactive, though launchd has the LaunchDaemon".to_string(),
            )
            // This daemon got no descriptors from launchd, so it is not
            // launchd's — which makes it **the reason launchd's job is not
            // running**: that one stands down when it finds the socket
            // taken, and a clean exit is not restarted. Waking it without
            // this one going first only repeats that.
            //
            // Restarting is the whole fix in the ordinary case: stopping
            // hands the socket over, and starting reaches for :80, which
            // is launchd's to answer. It needs no root, where a kickstart
            // does — but it also cannot force the job up. Reaching :80
            // only wakes something listening there, so if anything else
            // holds it the start falls through to a daemon of its own and
            // says so by leaving this check exactly as it was. That is
            // what the kickstart is still here for.
            .with_fix(format!(
                "this daemon was not started by launchd, so it holds the \
                 socket launchd's job wants. `{}` hands it over and lets \
                 launchd start its own; if it stays inactive, run `{}`",
                minato_core::launchd::RESTART_COMMAND,
                minato_core::launchd::kickstart_command()
            ))
        } else {
            Check::warn(
                "launchd",
                "launchd socket activation",
                "inactive; 80 and 443 are out, so it listens elsewhere".to_string(),
            )
            .with_fix("follow `minato setup` to install the LaunchDaemon")
        });

        checks.push(match self.gateway.ca_path() {
            Some(path) => Check::ok("ca", "local CA", path.display().to_string()),
            None => Check::warn("ca", "local CA", "not generated".to_string()),
        });

        // What that CA may sign for, which is the difference between a
        // key that is worth stealing and one that is not.
        if let Some(check) =
            self.ca_scope_check(project.as_ref().map(|context| context.config.domain()))
        {
            checks.push(check);
        }

        // Only worth reporting once a tunnel has been set up. An unused
        // feature showing up as a warning on every `doctor` run trains
        // people to skim past the output.
        if let Some(check) = self.tunnel_check().await {
            checks.push(check);
        }

        Ok(Response::Diagnostics(Diagnostics::new(checks)))
    }
    /// Checks the configured runtime, and mentions the alternatives.
    ///
    /// The configured one is the only one that can fail: an unreachable
    /// Docker on a machine that runs Apple Container is not a problem, and
    /// reporting it as one trains people to skim past the output. The
    /// others appear only when reachable, so `[runtime] default` can be
    /// switched to something known to work.
    async fn runtime_checks(&self, configured: &str) -> Vec<Check> {
        let title = "container runtime";
        let mut checks = Vec::new();

        checks.push(match self.runtime(configured).await {
            Ok(runtime) => match runtime.probe().await {
                Ok(info) => Check::ok("runtime", title, format!("{} {}", info.id, info.version)),
                Err(err) => Check::fail("runtime", title, err.to_string())
                    .with_fix(minato_runtime::start_hint(configured)),
            },
            // An unknown identifier in `[runtime] default` is a
            // configuration mistake, not an unreachable runtime.
            Err(err) => Check::fail("runtime", title, err.to_string()).with_fix(format!(
                "set [runtime] default to one of: {}",
                minato_runtime::AVAILABLE_RUNTIMES.join(", ")
            )),
        });

        for id in minato_runtime::AVAILABLE_RUNTIMES {
            if *id == configured {
                continue;
            }

            if let Some(info) = self.probe_runtime(id).await {
                checks.push(Check::ok(
                    "runtime-available",
                    format!("{} (available)", minato_runtime::display_name(id)),
                    format!("{} {}", info.id, info.version),
                ));
            }
        }

        checks
    }
    /// What the local CA is allowed to sign for, and whether that covers
    /// this project.
    ///
    /// **Two different problems, and they are not the same severity.**
    ///
    /// A CA with no constraint at all is every installation made before
    /// the rule existed. It works perfectly; it is simply worth more to
    /// an attacker than it needs to be. A warning, because nothing is
    /// broken and replacing a trusted certificate is the user's call.
    ///
    /// A project whose domain falls outside the constraint is broken —
    /// every HTTPS URL it issues will be refused — and the browser error
    /// for it names neither Minato nor the constraint. A failure, with
    /// the two ways out.
    fn ca_scope_check(&self, domain: Option<String>) -> Option<Check> {
        // Nothing to say before there is a CA at all; the check above
        // has already said that.
        self.gateway.ca_path()?;

        let permitted = self.gateway.ca_permitted();
        let title = "what the local CA may sign for";

        if permitted.is_empty() {
            return Some(
                Check::warn(
                    "ca-scope",
                    title,
                    "anything at all — this CA predates the name constraint, \
                     so whoever can read its key can sign for any host and be \
                     believed"
                        .to_string(),
                )
                .with_fix(
                    "stop trusting it first — `minato uninstall` prints the \
                     command, and it names the file, so it has to run while \
                     the file is still there. Then delete ~/.minato/ca/, \
                     restart the daemon, and run `minato setup` to trust the \
                     replacement. Leaving the old one trusted keeps every \
                     certificate its key ever signed working",
                ),
            );
        }

        // A machine with no project is a perfectly ordinary thing to run
        // doctor on, and there is no domain to check against.
        let Some(domain) = domain else {
            return Some(Check::ok("ca-scope", title, permitted.join(", ")));
        };

        if minato_proxy::permits(permitted, &domain) {
            return Some(Check::ok(
                "ca-scope",
                title,
                format!("{} — which covers {domain}", permitted.join(", ")),
            ));
        }

        Some(
            Check::fail(
                "ca-scope",
                title,
                format!(
                    "{} — which does not cover {domain}, so every HTTPS URL \
                     of this project will be refused",
                    permitted.join(", ")
                ),
            )
            .with_fix(format!(
                "put [project] domain under {}. Regenerating the CA will not \
                 help: what it may sign for is compiled in, so a replacement \
                 carries the same constraint",
                permitted.join(" or ")
            )),
        )
    }
    /// Diagnoses the tunnel, or nothing when there is none to diagnose.
    async fn tunnel_check(&self) -> Option<Check> {
        let record = self.tunnel_record().await.ok().flatten()?;

        let settings = self.tunnel_settings(&record).ok();
        let info = tunnel::info(Some(&record), &self.tunnel, settings.as_ref()).await;
        let title = "Cloudflare Tunnel";
        let domain = record.domain.clone();

        Some(match info.state {
            TunnelState::Running => Check::ok("tunnel", title, format!("running for *.{domain}")),
            TunnelState::Disabled => Check::ok("tunnel", title, "disabled".to_string()),
            TunnelState::NotInstalled => {
                Check::fail("tunnel", title, "cloudflared is not installed".to_string())
                    .with_fix("brew install cloudflared")
            }
            TunnelState::NeedsLogin => {
                Check::fail("tunnel", title, "cloudflared is not logged in".to_string())
                    .with_fix("cloudflared tunnel login")
            }
            // Enabled but not up. Everything published through it is
            // unreachable, and nothing local would show that.
            TunnelState::Stopped => Check::fail(
                "tunnel",
                title,
                format!("enabled for *.{domain}, but not running"),
            )
            .with_fix("run `minato tunnel enable --public`, or `minato tunnel status` for why"),
        })
    }
}

/// An address as it is written down: `[::1]`, not `::1`.
///
/// `Display` for an `IpAddr` gives the bare form, which reads as a stray
/// colon run in a sentence and does not match how the docs or the URLs
/// write it.
fn bracketed(address: std::net::IpAddr) -> String {
    match address {
        std::net::IpAddr::V4(address) => address.to_string(),
        std::net::IpAddr::V6(address) => format!("[{address}]"),
    }
}
/// How a listener that did come up is described.
///
/// **Says when it had to settle.** Landing on the fallback is not a failure
/// — URLs work — but they carry a port from then on, and without a word
/// here that reads as an oddity rather than the consequence of a privilege
/// the daemon never had.
///
/// Keyed on having fallen back rather than on the port not being 80: a port
/// named with `MINATO_HTTP_PORT` is what was asked for, and calling that
/// unexpected would be wrong.
fn listening_detail(port: u16, fell_back: bool) -> String {
    if !fell_back {
        return format!("127.0.0.1:{port}");
    }

    format!("127.0.0.1:{port} (a fallback, so every URL carries the port)")
}
/// How a missing listener is described.
fn detail_for(failure: Option<BindFailure>) -> String {
    failure.unwrap_or(BindFailure::Other).detail().to_string()
}
/// What to do about a listener that could not be held.
///
/// **The launchd case comes first.** After `minato daemon stop` the job is
/// idle while launchd keeps holding 80, so the bind fails with the port in
/// use — and the old advice, "a port below 1024 needs privileges, follow
/// `minato setup`", names neither the cause nor a step that helps.
///
/// `launchd_has_the_job` is passed in rather than read here, so the advice
/// can be checked without a LaunchDaemon on the machine running the tests.
fn bind_fix(failure: Option<BindFailure>, port_env: &str, launchd_has_the_job: bool) -> String {
    if failure == Some(BindFailure::InUse) && launchd_has_the_job {
        return format!(
            "launchd may be holding this port for a job it is not running. \
             `{}` hands the socket back and starts the job, and needs no \
             root; if it stays in use, `{}` forces the job up. If something \
             unrelated has the port, name another with {port_env}",
            minato_core::launchd::RESTART_COMMAND,
            minato_core::launchd::kickstart_command()
        );
    }

    match failure.unwrap_or(BindFailure::Other) {
        BindFailure::Privileged => format!(
            "a port below 1024 needs privileges. Follow `minato setup`, \
             or name another port with {port_env}"
        ),
        BindFailure::InUse => {
            format!("stop whatever else holds the port, or name another with {port_env}")
        }
        BindFailure::Other => format!("name another port with {port_env}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::Gateway;
    use crate::supervisor::tests::supervisor;
    use minato_api::CheckStatus;

    #[test]
    fn no_ca_means_nothing_to_say_about_its_scope() {
        // The check above it already reports that there is no CA. Two
        // lines about the same absence is noise on a screen people are
        // meant to read.
        let supervisor = supervisor(Gateway::inert());

        assert!(
            supervisor
                .ca_scope_check(Some("myapp.localhost".to_string()))
                .is_none()
        );
    }
    #[test]
    fn an_unconstrained_ca_is_a_warning_that_says_how_to_replace_it() {
        // Every installation made before the rule. Nothing is broken, so
        // not a failure — but the key is worth more to an attacker than
        // it needs to be, and replacing it is the user's call.
        let supervisor = supervisor(Gateway::inert().with_ca("/tmp/ca.crt"));

        let check = supervisor
            .ca_scope_check(Some("myapp.localhost".to_string()))
            .expect("reports");

        assert_eq!(check.status, CheckStatus::Warn);
        let fix = check.fix.expect("has a fix");

        // **The order matters and the text has to carry it.** The
        // command names the file, so it cannot run after the delete.
        assert!(fix.contains("uninstall"), "got: {fix}");
        assert!(
            fix.find("trusting it first").unwrap() < fix.find("delete").unwrap(),
            "untrusting has to come before deleting: {fix}"
        );
    }
    #[test]
    fn a_domain_the_ca_covers_is_reported_as_covered() {
        let supervisor = supervisor(
            Gateway::inert()
                .with_ca("/tmp/ca.crt")
                .with_ca_permitting(&["localhost"]),
        );

        let check = supervisor
            .ca_scope_check(Some("myapp.localhost".to_string()))
            .expect("reports");

        assert_eq!(check.status, CheckStatus::Ok);
        assert!(check.detail.contains("myapp.localhost"), "{}", check.detail);
    }
    #[test]
    fn a_domain_outside_the_constraint_fails_with_a_fix_that_can_be_carried_out() {
        // The fix used to offer regenerating the CA. It cannot help:
        // what a new one permits is compiled in, so the replacement is
        // as constrained as the one deleted and the user loops.
        let supervisor = supervisor(
            Gateway::inert()
                .with_ca("/tmp/ca.crt")
                .with_ca_permitting(&["localhost"]),
        );

        let check = supervisor
            .ca_scope_check(Some("myapp.example.com".to_string()))
            .expect("reports");

        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.detail.contains("myapp.example.com"));

        let fix = check.fix.expect("has a fix");
        assert!(fix.contains("[project] domain"), "got: {fix}");
        assert!(
            !fix.contains("delete"),
            "offering to regenerate the CA sends them somewhere that cannot work: {fix}"
        );
    }
    #[test]
    fn no_project_still_reports_what_the_ca_covers() {
        // `doctor` on a machine with no project is a perfectly ordinary
        // thing to run, and often why somebody runs it.
        let supervisor = supervisor(
            Gateway::inert()
                .with_ca("/tmp/ca.crt")
                .with_ca_permitting(&["localhost"]),
        );

        let check = supervisor.ca_scope_check(None).expect("reports");

        assert_eq!(check.status, CheckStatus::Ok);
        assert_eq!(check.detail, "localhost");
    }
    #[test]
    fn a_port_in_use_under_launchd_points_at_launchd() {
        // The state `minato daemon stop` leaves behind: the job is idle,
        // launchd still holds 80. Advising `minato setup` here sends
        // someone to re-run what they have already done.
        let fix = bind_fix(Some(BindFailure::InUse), "MINATO_HTTP_PORT", true);

        assert!(fix.contains("launchd"), "name the cause: {fix}");
        assert!(fix.contains(minato_core::launchd::RESTART_COMMAND), "{fix}");
        assert!(
            !fix.contains("minato setup"),
            "setup is already done in this state: {fix}"
        );
        // **The order is the whole point.** The launchd check says the same
        // two commands in the same order, `setup` offers the restart as its
        // wake step with the kickstart as the note under it, and
        // docs/guide/troubleshooting.md walks the reader down the same
        // ladder — one that asks for a password only where the step that
        // does not has already failed.
        assert!(
            fix.find(minato_core::launchd::RESTART_COMMAND).unwrap()
                < fix.find("kickstart").unwrap(),
            "the answer that needs no root comes first: {fix}"
        );
    }
    #[test]
    fn a_privileged_port_still_points_at_setup() {
        let fix = bind_fix(Some(BindFailure::Privileged), "MINATO_HTTP_PORT", false);

        assert!(fix.contains("minato setup"), "{fix}");
        assert!(fix.contains("MINATO_HTTP_PORT"), "{fix}");
    }
    #[test]
    fn a_port_in_use_without_launchd_blames_the_other_process() {
        let fix = bind_fix(Some(BindFailure::InUse), "MINATO_DNS_PORT", false);

        assert!(!fix.contains("launchd"), "{fix}");
        assert!(fix.contains("MINATO_DNS_PORT"), "{fix}");
    }
    #[test]
    fn the_detail_says_which_kind_of_failure_it_was() {
        // "could not be held" was all it ever said, whatever happened.
        assert!(detail_for(Some(BindFailure::Privileged)).contains("privileges"));
        assert!(detail_for(Some(BindFailure::InUse)).contains("another process"));
    }
    #[test]
    fn an_address_reads_the_way_it_is_written_down() {
        // `Display` gives `::1`, which reads as a stray colon run in a
        // sentence and matches neither the docs nor a URL.
        assert_eq!(
            bracketed(std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)),
            "[::1]"
        );
        assert_eq!(
            bracketed(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            "127.0.0.1"
        );
    }
    #[test]
    fn a_fallback_port_is_reported_as_such() {
        // Landing on the fallback is not a failure, but the URLs carry a
        // port from then on. Left unsaid that reads as an oddity rather
        // than the consequence of a privilege the daemon never had.
        let detail = listening_detail(crate::gateway::FALLBACK_HTTPS_PORT, true);

        assert!(detail.contains("18443"), "{detail}");
        assert!(detail.contains("fallback"), "{detail}");
        assert!(detail.contains("carries the port"), "{detail}");
    }
    #[test]
    fn a_port_that_was_asked_for_is_reported_plainly() {
        // MINATO_HTTPS_PORT=8443 got exactly what it named. Calling that a
        // fallback would present the user's own choice as an anomaly.
        assert_eq!(listening_detail(8443, false), "127.0.0.1:8443");
        assert_eq!(listening_detail(443, false), "127.0.0.1:443");
    }
}

//! A local CA, issuing a certificate per SNI name on demand.
//!
//! A wildcard certificate covers exactly one label. `*.localhost` cannot
//! cover `web.feat-1.myapp.localhost`, and every new worktree invents a
//! name at a new depth, so preparing certificates ahead of time never
//! keeps up.
//!
//! So there is one CA, and it **issues a certificate for whatever name SNI
//! asks for, on the spot, and caches it**. The user only ever has to trust
//! that single CA.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, GeneralSubtree, IsCa, Issuer,
    KeyPair, KeyUsagePurpose, NameConstraints,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;

/// The CA certificate. This is the file the user tells the system to trust.
pub const CA_CERT_FILE: &str = "kobune-ca.crt";

/// The CA's private key.
pub const CA_KEY_FILE: &str = "kobune-ca.key";

/// How long the CA certificate is valid, in years.
const CA_VALIDITY_YEARS: i64 = 10;

/// The DNS suffixes a CA created from now on may sign for.
///
/// **This is what keeps a stolen key from being worth anything.** The
/// certificate goes into the system trust store, so without a constraint
/// whoever reads `kobune-ca.key` can mint `google.com` and be believed.
/// `mkcert` asks for the same bargain, which makes it usual rather than
/// good, and X.509 has had the answer since 1999.
///
/// **No leading dot.** RFC 5280 §4.2.1.10 says a DNS subtree is satisfied
/// by the name itself and by anything with labels prepended, so
/// `localhost` already covers `web.feat-1.myapp.localhost` — which is the
/// whole requirement, since every worktree invents a name at a new depth.
/// `.localhost` is a different, non-standard form that means *strictly*
/// below, and it excludes `localhost` itself. Measured, not assumed:
/// against OpenSSL, `DNS:.localhost` refuses a leaf for `localhost` with
/// "permitted subtree violation" while `DNS:localhost` accepts both. And
/// `localhost` is a name Kobune really serves — [`kobune_dns`] answers for
/// the apex — so the dot broke a working URL and bought nothing.
///
/// `localhost` is reserved by RFC 6761 and can never be a real public
/// name, so what a leaked key could sign is nothing anybody could be
/// fooled by.
///
/// **Only what Kobune actually serves.** `.test` was here for a moment,
/// on the strength of `docs/DESIGN.md` §5 anticipating it — but nothing
/// resolves it today: the DNS server serves `localhost` alone, and the
/// CLI only ever installs `/etc/resolver/localhost`. Permitting a suffix
/// that cannot resolve would let `kobune doctor` report a working setup
/// that is not one. It comes back when the resolver does.
pub const PERMITTED_SUFFIXES: [&str; 1] = ["localhost"];

/// How long an issued leaf certificate is valid, in days.
///
/// Browsers reject certificates that live too long: Safari and Chrome
/// refuse anything over 398 days. This stays comfortably under that.
const LEAF_VALIDITY_DAYS: i64 = 300;

#[derive(Debug, thiserror::Error)]
pub enum CaError {
    #[error("cannot generate the CA: {0}")]
    Generate(#[source] rcgen::Error),

    #[error("cannot read the CA ({path}): {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("cannot write the CA ({path}): {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("the CA is not readable as a certificate ({path}): {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: rcgen::Error,
    },
}

/// The local CA.
pub struct LocalCa {
    /// What signs leaves.
    ///
    /// **Not the certificate on disk.** An issuer carries the signing key
    /// and the name to put in `issuer`, and nothing of the certificate's
    /// own bytes — which is the right shape, since an ECDSA signature is
    /// different every time and re-signing would produce a CA that is not
    /// the one the user trusted. What goes out as the chain is always
    /// [`Self::der`].
    issuer: Issuer<'static, KeyPair>,
    /// The CA certificate exactly as it is on disk. Sent as the chain
    /// alongside each leaf.
    der: CertificateDer<'static>,
    /// The PEM on disk, guaranteed identical to what the user trusted.
    pem: String,
    /// The DNS suffixes this CA may sign for, read from the certificate's
    /// own name constraints.
    ///
    /// **Empty means unconstrained**, which is what a CA created before
    /// [`PERMITTED_SUFFIXES`] existed looks like. Those are left alone
    /// rather than replaced — swapping a certificate the user trusted
    /// would break every URL until they noticed and trusted the new one
    /// — so `kobune doctor` reports them instead.
    permitted: Vec<String>,
    dir: PathBuf,
}

impl LocalCa {
    /// Loads the CA in `dir`, creating one if there is none.
    pub fn load_or_create(dir: &Path) -> Result<Self, CaError> {
        let cert_path = dir.join(CA_CERT_FILE);
        let key_path = dir.join(CA_KEY_FILE);

        if cert_path.is_file() && key_path.is_file() {
            match Self::load(dir) {
                Ok(ca) => return Ok(ca),
                Err(err) => {
                    // Starting up with a corrupt CA means TLS never works.
                    // Regenerate and carry on — the user has to trust the
                    // new one again.
                    tracing::warn!("cannot read the existing CA, regenerating: {err}");
                }
            }
        }

        Self::create(dir)
    }

    fn load(dir: &Path) -> Result<Self, CaError> {
        let cert_path = dir.join(CA_CERT_FILE);
        let key_path = dir.join(CA_KEY_FILE);

        let key_pem = std::fs::read_to_string(&key_path).map_err(|source| CaError::Read {
            path: key_path.clone(),
            source,
        })?;
        let cert_pem = std::fs::read_to_string(&cert_path).map_err(|source| CaError::Read {
            path: cert_path.clone(),
            source,
        })?;

        let key_pair = KeyPair::from_pem(&key_pem).map_err(|source| CaError::Parse {
            path: key_path,
            source,
        })?;

        // What the user trusted is the certificate on disk. Send that.
        let der = pem_to_der(&cert_pem, dir.join(CA_CERT_FILE))?;

        // Read from the certificate, which is the only place it exists:
        // an `Issuer` does not carry name constraints.
        let permitted = permitted_from(&der);

        let issuer = Issuer::from_ca_cert_der(&der, key_pair).map_err(|source| CaError::Parse {
            path: cert_path,
            source,
        })?;

        Ok(Self {
            permitted,
            issuer,
            der,
            pem: cert_pem,
            dir: dir.to_path_buf(),
        })
    }

    fn create(dir: &Path) -> Result<Self, CaError> {
        std::fs::create_dir_all(dir).map_err(|source| CaError::Write {
            path: dir.to_path_buf(),
            source,
        })?;

        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];

        // What this CA may ever sign for. See PERMITTED_SUFFIXES.
        params.name_constraints = Some(NameConstraints {
            permitted_subtrees: PERMITTED_SUFFIXES
                .iter()
                .map(|suffix| GeneralSubtree::DnsName((*suffix).to_string()))
                .collect(),
            excluded_subtrees: Vec::new(),
        });

        let mut name = DistinguishedName::new();
        // A name the user can find in Keychain.
        name.push(DnType::CommonName, "Kobune Local CA");
        name.push(DnType::OrganizationName, "Kobune");
        params.distinguished_name = name;

        params.not_before = now();
        params.not_after = now() + time::Duration::days(365 * CA_VALIDITY_YEARS);

        let key_pair = KeyPair::generate().map_err(CaError::Generate)?;
        let certificate = params.self_signed(&key_pair).map_err(CaError::Generate)?;

        let cert_path = dir.join(CA_CERT_FILE);
        let key_path = dir.join(CA_KEY_FILE);
        let pem = certificate.pem();

        write_public(&cert_path, pem.as_bytes())?;

        // Only the owner may read the private key.
        write_private(&key_path, key_pair.serialize_pem().as_bytes())?;

        let der = CertificateDer::from(certificate.der().to_vec());

        // **Read out of the certificate, not taken from the constant.**
        // Taking the constant would have `create` report a constraint
        // whose presence in the bytes nothing checked — and rcgen omits
        // the extension entirely when both subtree lists are empty, so
        // the two really can differ. `load` reads the same way, from the
        // same place, so a fresh CA and a reloaded one cannot disagree
        // about the same file.
        let permitted = permitted_from(&der);

        let issuer = Issuer::from_ca_cert_der(&der, key_pair).map_err(CaError::Generate)?;

        Ok(Self {
            issuer,
            der,
            pem,
            permitted,
            dir: dir.to_path_buf(),
        })
    }

    /// The DNS suffixes this CA may sign for.
    ///
    /// Empty for a CA that carries no name constraint at all — one made
    /// before the rule existed. `kobune doctor` is what says so.
    pub fn permitted_suffixes(&self) -> &[String] {
        &self.permitted
    }

    /// Whether this CA may sign for `host`.
    ///
    /// An unconstrained CA may sign for anything, which is exactly the
    /// problem and exactly what it will do — so this answers `true` for
    /// one rather than pretending otherwise.
    pub fn permits(&self, host: &str) -> bool {
        permits(&self.permitted, host)
    }

    /// The path to the CA certificate — what the user tells the system
    /// to trust.
    pub fn certificate_path(&self) -> PathBuf {
        self.dir.join(CA_CERT_FILE)
    }

    /// The CA certificate on disk, identical to what the user trusted.
    pub fn certificate_pem(&self) -> &str {
        &self.pem
    }

    /// Issues a certificate for `host`.
    pub fn issue(&self, host: &str) -> Result<CertifiedKey, CaError> {
        let mut params =
            CertificateParams::new(vec![host.to_string()]).map_err(CaError::Generate)?;

        let mut name = DistinguishedName::new();
        name.push(DnType::CommonName, host);
        params.distinguished_name = name;

        params.not_before = now();
        params.not_after = now() + time::Duration::days(LEAF_VALIDITY_DAYS);

        let key_pair = KeyPair::generate().map_err(CaError::Generate)?;
        let leaf = params
            .signed_by(&key_pair, &self.issuer)
            .map_err(CaError::Generate)?;

        let leaf_der = CertificateDer::from(leaf.der().to_vec());
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));

        let signing_key = rustls::crypto::ring::sign::any_supported_type(&key_der)
            .map_err(|_| CaError::Generate(rcgen::Error::UnsupportedSignatureAlgorithm))?;

        // Send the leaf and the CA together: trusting the CA is enough
        // for verification to pass.
        Ok(CertifiedKey::new(
            vec![leaf_der, self.der.clone()],
            signing_key,
        ))
    }
}

/// Writes the certificate so that anyone may read it.
///
/// **Explicitly, rather than by umask.** The certificate is public by
/// nature — it is what everything is asked to trust — and it is mounted
/// into containers that run as their own users. Under a hardened umask it
/// would land 0600 owned by the host user, and a service running as
/// `node` or `nobody` would fail to read it through a read-only mount it
/// cannot change, with an error naming a path that exists nowhere on the
/// host.
#[cfg(unix)]
fn write_public(path: &Path, contents: &[u8]) -> Result<(), CaError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o644)
        .open(path)
        .map_err(|source| CaError::Write {
            path: path.to_path_buf(),
            source,
        })?;

    file.write_all(contents).map_err(|source| CaError::Write {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn write_public(path: &Path, contents: &[u8]) -> Result<(), CaError> {
    std::fs::write(path, contents).map_err(|source| CaError::Write {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &[u8]) -> Result<(), CaError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| CaError::Write {
            path: path.to_path_buf(),
            source,
        })?;

    file.write_all(contents).map_err(|source| CaError::Write {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &[u8]) -> Result<(), CaError> {
    std::fs::write(path, contents).map_err(|source| CaError::Write {
        path: path.to_path_buf(),
        source,
    })
}

/// The DNS suffixes a certificate's name constraints permit.
///
/// **Parsed out of the certificate itself.** rcgen 0.14 has no public way
/// to ask: `CertificateParams::from_ca_cert_der` went `pub(crate)`, and
/// the `Issuer` that replaced it keeps the signing key and the
/// distinguished name and drops the constraints. So this reads the
/// extension directly, which is where rcgen was reading it from anyway.
///
/// Only `DnsName` subtrees: an IP or a directory name says nothing about
/// which hostnames this CA covers, which is the only question here.
///
/// An unreadable certificate answers the same as an unconstrained one —
/// empty. Both mean "nothing here says what this may sign for", and a CA
/// that will not parse has worse problems than this function.
fn permitted_from(der: &CertificateDer<'_>) -> Vec<String> {
    let Ok((_, certificate)) = x509_parser::parse_x509_certificate(der) else {
        return Vec::new();
    };

    let Ok(Some(constraints)) = certificate.name_constraints() else {
        return Vec::new();
    };

    constraints
        .value
        .permitted_subtrees
        .iter()
        .flatten()
        .filter_map(|subtree| match subtree.base {
            x509_parser::extensions::GeneralName::DNSName(name) => Some(name.to_ascii_lowercase()),
            _ => None,
        })
        .collect()
}

/// Whether `permitted` covers `host`.
///
/// Free of `LocalCa` so that the daemon can ask the same question of a
/// suffix it read from a configuration, without a CA in hand.
pub fn permits(permitted: &[String], host: &str) -> bool {
    // No constraint, so nothing is out of bounds.
    if permitted.is_empty() {
        return true;
    }

    let host = host.trim_end_matches('.').to_ascii_lowercase();

    permitted.iter().any(|suffix| {
        // RFC 5280 §4.2.1.10: the subtree name itself, and anything with
        // labels prepended. A leading dot would be the non-standard form
        // that drops the first half, and is tolerated here only so that a
        // CA carrying one is read the way its verifier will read it.
        let bare = suffix.trim_start_matches('.').to_ascii_lowercase();
        let strictly_below = suffix.starts_with('.');

        host.strip_suffix(&bare).is_some_and(|rest| match rest {
            "" => !strictly_below,
            rest => rest.ends_with('.'),
        })
    })
}

/// Extracts the first certificate in a PEM as DER.
fn pem_to_der(pem: &str, path: PathBuf) -> Result<CertificateDer<'static>, CaError> {
    let mut reader = std::io::BufReader::new(pem.as_bytes());

    rustls_pemfile::certs(&mut reader)
        .next()
        .and_then(|entry| entry.ok())
        .map(|der| der.into_owned())
        .ok_or_else(|| CaError::Read {
            path,
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, "contains no certificate"),
        })
}

fn now() -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc()
}

/// Resolves a certificate from SNI, issuing one on the spot for names it
/// has not seen.
pub struct DynamicCertResolver {
    ca: Arc<LocalCa>,
    cache: RwLock<HashMap<String, Arc<CertifiedKey>>>,
    /// Whether the out-of-scope warning has already been said.
    ///
    /// **SNI is unauthenticated and attacker-chosen.** Anything that can
    /// reach the HTTPS port can ask for a name outside the constraint,
    /// and one log line per distinct name is a way to write into the
    /// daemon's log for free. The first one carries everything a person
    /// needs; the rest are the same sentence.
    warned_out_of_scope: AtomicBool,
}

impl DynamicCertResolver {
    pub fn new(ca: Arc<LocalCa>) -> Self {
        Self {
            ca,
            cache: RwLock::new(HashMap::new()),
            warned_out_of_scope: AtomicBool::new(false),
        }
    }

    /// The certificate for `host`, issued and cached if it is new.
    pub fn certificate_for(&self, host: &str) -> Option<Arc<CertifiedKey>> {
        let key = host.to_ascii_lowercase();

        if let Some(existing) = self
            .cache
            .read()
            .expect("the certificate cache lock is poisoned")
            .get(&key)
        {
            return Some(existing.clone());
        }

        if !self.ca.permits(&key) && !self.warned_out_of_scope.swap(true, Ordering::Relaxed) {
            // Issued anyway: the constraint is enforced by whoever
            // verifies, which is correct X.509, and refusing here would
            // turn a legible certificate error into a bare handshake
            // failure. But it is worth saying once, where somebody
            // debugging can find it.
            tracing::warn!(
                "{key} is outside what this CA may sign for ({}), so the \
                 certificate will be refused by anything that checks it. \
                 `kobune doctor` says what to do about it",
                self.ca.permitted_suffixes().join(", ")
            );
        }

        let issued = match self.ca.issue(&key) {
            Ok(certified) => Arc::new(certified),
            Err(err) => {
                tracing::warn!("cannot issue a certificate for {key}: {err}");
                return None;
            }
        };

        self.cache
            .write()
            .expect("the certificate cache lock is poisoned")
            .insert(key, issued.clone());

        Some(issued)
    }
}

impl std::fmt::Debug for DynamicCertResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynamicCertResolver")
            .finish_non_exhaustive()
    }
}

impl ResolvesServerCert for DynamicCertResolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        // A client that sends no SNI gets nothing: there is no way to know
        // which name the certificate should be verified against.
        let name = hello.server_name()?;
        self.certificate_for(name)
    }
}

/// Builds a TLS config that issues a certificate per SNI name.
pub fn server_config(ca: Arc<LocalCa>) -> Arc<rustls::ServerConfig> {
    let resolver = Arc::new(DynamicCertResolver::new(ca));

    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(resolver);

    // HTTP/2 is not supported yet. WebSocket upgrades happen over
    // HTTP/1.1, so advertising h2 here would break HMR.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];

    Arc::new(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_ca() -> (tempfile::TempDir, LocalCa) {
        let dir = tempfile::tempdir().expect("tempdir");
        let ca = LocalCa::load_or_create(dir.path()).expect("creates a CA");
        (dir, ca)
    }

    #[test]
    fn creates_ca_files_with_restricted_key() {
        let (dir, ca) = temp_ca();

        assert!(ca.certificate_path().is_file());
        assert!(dir.path().join(CA_KEY_FILE).is_file());
        assert!(ca.certificate_pem().contains("BEGIN CERTIFICATE"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(CA_KEY_FILE))
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "only the owner may read the private key"
            );

            // The certificate is the other half of that rule: it is
            // mounted into containers running as their own users, and a
            // umask-dependent 0600 would leave them unable to read a
            // read-only mount they cannot change.
            let mode = std::fs::metadata(ca.certificate_path())
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o644,
                "anyone may read the certificate; that is what it is for"
            );
        }
    }

    #[test]
    fn reuses_an_existing_ca() {
        let dir = tempfile::tempdir().expect("tempdir");

        let first = LocalCa::load_or_create(dir.path()).expect("creates");
        let first_pem = first.certificate_pem().to_string();
        drop(first);

        let second = LocalCa::load_or_create(dir.path()).expect("loads");

        assert_eq!(
            first_pem,
            second.certificate_pem(),
            "regenerating would make the user trust it all over again"
        );
    }

    #[test]
    fn presents_the_certificate_that_is_on_disk() {
        // What the user trusts is the CA on disk. Re-signing on every load
        // would send out a different chain, and verification would have
        // nothing to stand on.
        let dir = tempfile::tempdir().expect("tempdir");
        let created = LocalCa::load_or_create(dir.path()).expect("creates");
        drop(created);

        let loaded = LocalCa::load_or_create(dir.path()).expect("loads");
        let on_disk = std::fs::read_to_string(dir.path().join(CA_CERT_FILE)).expect("reads");

        assert_eq!(loaded.certificate_pem(), on_disk);

        let chain_der =
            pem_to_der(&on_disk, dir.path().join(CA_CERT_FILE)).expect("converts to DER");
        let issued = loaded.issue("web.myapp.localhost").expect("issues");
        assert_eq!(
            issued.cert[1], chain_der,
            "the CA in the chain must be the one on disk"
        );
    }

    #[test]
    fn a_new_ca_is_narrowed_to_what_kobune_serves() {
        // The whole point. This certificate goes into the system trust
        // store, and without this the key behind it signs `google.com`
        // as readily as anything of Kobune's.
        let (_dir, ca) = temp_ca();

        assert_eq!(ca.permitted_suffixes(), ["localhost"]);
        assert!(
            !ca.permitted_suffixes()[0].starts_with('.'),
            "a leading dot is the non-standard form, and it excludes the apex"
        );
    }

    #[test]
    fn the_constraint_survives_being_written_and_read_back() {
        // **The test that proves it is really in the certificate.**
        // Loading parses the PEM from disk, so a constraint that rcgen
        // had quietly dropped on the way out would come back empty here
        // — and every other assertion in this file would still pass.
        let dir = tempfile::tempdir().expect("tempdir");
        let created = LocalCa::load_or_create(dir.path()).expect("creates");
        let written = created.permitted_suffixes().to_vec();
        drop(created);

        let loaded = LocalCa::load_or_create(dir.path()).expect("loads");

        assert_eq!(loaded.permitted_suffixes(), written.as_slice());
        assert!(!written.is_empty(), "an empty list would pass vacuously");
    }

    #[test]
    fn a_constraint_covers_every_depth_a_worktree_invents() {
        // Including the apex. `kobune-dns` answers for `localhost`
        // itself, so `https://localhost` is a URL somebody really opens
        // — and the leading-dot form this started with refused it.
        //
        // Agreement with a real verifier is
        // `the_constrained_ca_verifies_for_every_name_it_covers`, in
        // tests/proxy_e2e.rs. This one alone would pass either way.
        let (_dir, ca) = temp_ca();

        for host in [
            "localhost",
            "myapp.localhost",
            "web.myapp.localhost",
            "web.feature-user-auth.myapp.localhost",
        ] {
            assert!(ca.permits(host), "{host} should be covered");
        }

        for host in [
            "google.com",
            "myapp.localhost.evil.com",
            "localhostx",
            "notlocalhost",
        ] {
            assert!(!ca.permits(host), "{host} must not be");
        }
    }

    #[test]
    fn a_leading_dot_is_read_the_way_its_verifier_reads_it() {
        // Not a form Kobune writes, but one a CA on disk could carry.
        // X.509 treats it as strictly-below, so reporting the apex as
        // covered would put `doctor` at odds with the browser.
        assert!(!permits(&[".localhost".to_string()], "localhost"));
        assert!(permits(&[".localhost".to_string()], "web.localhost"));

        assert!(permits(&["localhost".to_string()], "localhost"));
        assert!(permits(&["localhost".to_string()], "web.localhost"));
    }

    #[test]
    fn an_unconstrained_ca_says_so_rather_than_looking_fine() {
        // What every installation made before this rule has on disk.
        // Reporting it as constrained would be the one answer worse than
        // not asking.
        assert!(permits(&[], "google.com"), "nothing is out of bounds");

        // A real certificate with no constraint, which is what every
        // installation made before the rule has on disk.
        let key_pair = KeyPair::generate().expect("generates");
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        let certificate = params.self_signed(&key_pair).expect("self-signs");

        assert!(
            permitted_from(&CertificateDer::from(certificate.der().to_vec())).is_empty(),
            "no constraint in the certificate means no permitted suffixes"
        );
    }

    #[test]
    fn matching_is_case_and_trailing_dot_insensitive() {
        // Both arrive from the wire: SNI casing is the client's choice,
        // and a name from DNS can be fully qualified.
        let permitted = vec![".localhost".to_string()];

        assert!(permits(&permitted, "WEB.MyApp.localhost"));
        assert!(permits(&permitted, "web.myapp.localhost."));
        assert!(permits(&permitted, "web.myapp.LOCALHOST."));
    }

    #[test]
    fn regenerates_when_the_ca_is_corrupt() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(CA_CERT_FILE), "garbage").expect("writes");
        std::fs::write(dir.path().join(CA_KEY_FILE), "garbage").expect("writes");

        let ca = LocalCa::load_or_create(dir.path()).expect("starts even when corrupt");
        assert!(ca.certificate_pem().contains("BEGIN CERTIFICATE"));
    }

    #[test]
    fn issues_certificates_for_nested_names() {
        // Depths a wildcard cannot cover. This is why issuance is dynamic.
        let (_dir, ca) = temp_ca();

        for host in [
            "web.feat-1.myapp.localhost",
            "api.feature-user-auth.myapp.localhost",
            "web.myapp.localhost",
        ] {
            let certified = ca.issue(host).expect("issues");
            assert_eq!(
                certified.cert.len(),
                2,
                "the leaf and the CA have to go out together: {host}"
            );
        }
    }

    #[test]
    fn caches_issued_certificates() {
        let (_dir, ca) = temp_ca();
        let resolver = DynamicCertResolver::new(Arc::new(ca));

        let first = resolver
            .certificate_for("web.feat-1.myapp.localhost")
            .expect("issues");
        let second = resolver
            .certificate_for("web.feat-1.myapp.localhost")
            .expect("comes back from the cache");

        assert!(
            Arc::ptr_eq(&first, &second),
            "issuing per request would slow every handshake down"
        );
    }

    #[test]
    fn cache_is_case_insensitive() {
        let (_dir, ca) = temp_ca();
        let resolver = DynamicCertResolver::new(Arc::new(ca));

        let lower = resolver
            .certificate_for("web.myapp.localhost")
            .expect("issues");
        let upper = resolver
            .certificate_for("WEB.MyApp.localhost")
            .expect("issues");

        assert!(Arc::ptr_eq(&lower, &upper));
    }

    #[test]
    fn advertises_only_http1() {
        let (_dir, ca) = temp_ca();
        let config = server_config(Arc::new(ca));

        assert_eq!(
            config.alpn_protocols,
            vec![b"http/1.1".to_vec()],
            "advertising h2 would rule out WebSocket upgrades"
        );
    }
}

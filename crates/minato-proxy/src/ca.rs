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
use std::sync::{Arc, RwLock};

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;

/// The CA certificate. This is the file the user tells the system to trust.
pub const CA_CERT_FILE: &str = "minato-ca.crt";

/// The CA's private key.
pub const CA_KEY_FILE: &str = "minato-ca.key";

/// How long the CA certificate is valid, in years.
const CA_VALIDITY_YEARS: i64 = 10;

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
    /// The issuer used to sign leaves.
    ///
    /// On load this is rebuilt from what is on disk, so its signature bytes
    /// differ from the ones on disk — an ECDSA signature is different every
    /// time. What goes out as the chain is always [`Self::der`].
    certificate: rcgen::Certificate,
    key_pair: KeyPair,
    /// The CA certificate exactly as it is on disk. Sent as the chain
    /// alongside each leaf.
    der: CertificateDer<'static>,
    /// The PEM on disk, guaranteed identical to what the user trusted.
    pem: String,
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

        let params =
            CertificateParams::from_ca_cert_pem(&cert_pem).map_err(|source| CaError::Parse {
                path: cert_path,
                source,
            })?;

        // Only ever used as the signing issuer. Its signature bytes do not
        // match the ones on disk, so it must not go out as the chain.
        let certificate = params.self_signed(&key_pair).map_err(CaError::Generate)?;

        // What the user trusted is the certificate on disk. Send that.
        let der = pem_to_der(&cert_pem, dir.join(CA_CERT_FILE))?;

        Ok(Self {
            certificate,
            key_pair,
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

        let mut name = DistinguishedName::new();
        // A name the user can find in Keychain.
        name.push(DnType::CommonName, "Minato Local CA");
        name.push(DnType::OrganizationName, "Minato");
        params.distinguished_name = name;

        params.not_before = now();
        params.not_after = now() + time::Duration::days(365 * CA_VALIDITY_YEARS);

        let key_pair = KeyPair::generate().map_err(CaError::Generate)?;
        let certificate = params.self_signed(&key_pair).map_err(CaError::Generate)?;

        let cert_path = dir.join(CA_CERT_FILE);
        let key_path = dir.join(CA_KEY_FILE);
        let pem = certificate.pem();

        std::fs::write(&cert_path, &pem).map_err(|source| CaError::Write {
            path: cert_path,
            source,
        })?;

        // Only the owner may read the private key.
        write_private(&key_path, key_pair.serialize_pem().as_bytes())?;

        let der = CertificateDer::from(certificate.der().to_vec());

        Ok(Self {
            certificate,
            key_pair,
            der,
            pem,
            dir: dir.to_path_buf(),
        })
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
            .signed_by(&key_pair, &self.certificate, &self.key_pair)
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
}

impl DynamicCertResolver {
    pub fn new(ca: Arc<LocalCa>) -> Self {
        Self {
            ca,
            cache: RwLock::new(HashMap::new()),
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

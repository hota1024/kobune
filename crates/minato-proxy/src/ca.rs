//! ローカル CA と、SNI ごとの証明書の動的発行。
//!
//! ワイルドカード証明書は 1 ラベルしかカバーしない。`*.localhost` では
//! `web.feat-1.myapp.localhost` を賄えず、worktree が増えるたびに
//! 深さの違う名前が生まれる。証明書を事前に用意する方法では追いつかない。
//!
//! そこで CA を 1 つ持ち、**SNI で要求された名前の証明書をその場で発行して
//! キャッシュする**。利用者が信頼するのは CA 1 枚だけで済む。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;

/// CA 証明書のファイル名。利用者がこれをシステムに信頼させる。
pub const CA_CERT_FILE: &str = "minato-ca.crt";

/// CA の秘密鍵。
pub const CA_KEY_FILE: &str = "minato-ca.key";

/// CA 証明書の有効期間（年）。
const CA_VALIDITY_YEARS: i64 = 10;

/// 発行する leaf 証明書の有効期間（日）。
///
/// ブラウザは長すぎる有効期間の証明書を拒否する。Safari と Chrome は
/// 398 日を超えるものを受け付けないため、余裕を持たせて短くする。
const LEAF_VALIDITY_DAYS: i64 = 300;

#[derive(Debug, thiserror::Error)]
pub enum CaError {
    #[error("CA の生成に失敗しました: {0}")]
    Generate(#[source] rcgen::Error),

    #[error("CA の読み込みに失敗しました ({path}): {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("CA の書き出しに失敗しました ({path}): {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("CA の内容を解釈できません ({path}): {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: rcgen::Error,
    },
}

/// ローカル CA。
pub struct LocalCa {
    /// leaf に署名するときの発行者として使う。
    ///
    /// 読み込み時にはディスクの内容から作り直すため、署名バイト列は
    /// ディスク上のものと一致しない（ECDSA の署名は毎回変わる）。
    /// チェーンとして送るのは必ず [`Self::der`] の方。
    certificate: rcgen::Certificate,
    key_pair: KeyPair,
    /// ディスク上の CA 証明書そのもの。leaf と一緒にチェーンとして送る。
    der: CertificateDer<'static>,
    /// ディスク上の PEM。利用者が信頼したものと同一であることを保証する。
    pem: String,
    dir: PathBuf,
}

impl LocalCa {
    /// `dir` の CA を読み込む。無ければ作る。
    pub fn load_or_create(dir: &Path) -> Result<Self, CaError> {
        let cert_path = dir.join(CA_CERT_FILE);
        let key_path = dir.join(CA_KEY_FILE);

        if cert_path.is_file() && key_path.is_file() {
            match Self::load(dir) {
                Ok(ca) => return Ok(ca),
                Err(err) => {
                    // 壊れた CA を抱えたまま起動しても TLS が通らない。
                    // 作り直して先に進む（利用者は再度信頼させる必要がある）。
                    tracing::warn!("既存の CA を読めないため作り直します: {err}");
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

        // 署名の発行者としてだけ使う。ここで得られる証明書の署名バイト列は
        // ディスク上のものと一致しないので、チェーンには使わない。
        let certificate = params.self_signed(&key_pair).map_err(CaError::Generate)?;

        // 利用者が信頼したのはディスク上の証明書。それをそのまま送る。
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
        // 利用者がキーチェーンで見つけられる名前にする。
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

        // 秘密鍵は本人だけが読めるようにする。
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

    /// CA 証明書のパス。利用者がこれをシステムに信頼させる。
    pub fn certificate_path(&self) -> PathBuf {
        self.dir.join(CA_CERT_FILE)
    }

    /// ディスク上の CA 証明書。利用者が信頼したものと同一。
    pub fn certificate_pem(&self) -> &str {
        &self.pem
    }

    /// `host` 向けの証明書を発行する。
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

        // leaf と CA を並べて送る。CA を信頼していれば検証が通る。
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

/// PEM から最初の証明書を DER として取り出す。
fn pem_to_der(pem: &str, path: PathBuf) -> Result<CertificateDer<'static>, CaError> {
    let mut reader = std::io::BufReader::new(pem.as_bytes());

    rustls_pemfile::certs(&mut reader)
        .next()
        .and_then(|entry| entry.ok())
        .map(|der| der.into_owned())
        .ok_or_else(|| CaError::Read {
            path,
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "証明書が含まれていません",
            ),
        })
}

fn now() -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc()
}

/// SNI を見て証明書を返す。未知の名前はその場で発行する。
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

    /// `host` の証明書を取得する。無ければ発行してキャッシュする。
    pub fn certificate_for(&self, host: &str) -> Option<Arc<CertifiedKey>> {
        let key = host.to_ascii_lowercase();

        if let Some(existing) = self
            .cache
            .read()
            .expect("証明書キャッシュのロックが壊れている")
            .get(&key)
        {
            return Some(existing.clone());
        }

        let issued = match self.ca.issue(&key) {
            Ok(certified) => Arc::new(certified),
            Err(err) => {
                tracing::warn!("{key} の証明書を発行できませんでした: {err}");
                return None;
            }
        };

        self.cache
            .write()
            .expect("証明書キャッシュのロックが壊れている")
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
        // SNI を送らないクライアントには証明書を出せない。
        // どの名前で検証されるべきか決められないため。
        let name = hello.server_name()?;
        self.certificate_for(name)
    }
}

/// SNI ごとに証明書を発行する TLS 設定を作る。
pub fn server_config(ca: Arc<LocalCa>) -> Arc<rustls::ServerConfig> {
    let resolver = Arc::new(DynamicCertResolver::new(ca));

    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(resolver);

    // HTTP/2 はまだ通していない。WebSocket の upgrade は HTTP/1.1 で行うため、
    // ここで h2 を広告すると HMR が繋がらなくなる。
    config.alpn_protocols = vec![b"http/1.1".to_vec()];

    Arc::new(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_ca() -> (tempfile::TempDir, LocalCa) {
        let dir = tempfile::tempdir().expect("tempdir");
        let ca = LocalCa::load_or_create(dir.path()).expect("CA を作れる");
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
            assert_eq!(mode & 0o777, 0o600, "秘密鍵は本人だけが読める必要がある");
        }
    }

    #[test]
    fn reuses_an_existing_ca() {
        let dir = tempfile::tempdir().expect("tempdir");

        let first = LocalCa::load_or_create(dir.path()).expect("作れる");
        let first_pem = first.certificate_pem().to_string();
        drop(first);

        let second = LocalCa::load_or_create(dir.path()).expect("読める");

        assert_eq!(
            first_pem,
            second.certificate_pem(),
            "作り直すと利用者が再び信頼させる羽目になる"
        );
    }

    #[test]
    fn presents_the_certificate_that_is_on_disk() {
        // 利用者が信頼するのはディスク上の CA。読み込みのたびに署名し直すと
        // 送出するチェーンが別物になり、検証の前提が崩れる。
        let dir = tempfile::tempdir().expect("tempdir");
        let created = LocalCa::load_or_create(dir.path()).expect("作れる");
        drop(created);

        let loaded = LocalCa::load_or_create(dir.path()).expect("読める");
        let on_disk = std::fs::read_to_string(dir.path().join(CA_CERT_FILE)).expect("読める");

        assert_eq!(loaded.certificate_pem(), on_disk);

        let chain_der = pem_to_der(&on_disk, dir.path().join(CA_CERT_FILE)).expect("DER にできる");
        let issued = loaded.issue("web.myapp.localhost").expect("発行できる");
        assert_eq!(
            issued.cert[1], chain_der,
            "チェーンに載せる CA はディスク上のものと同一でなければならない"
        );
    }

    #[test]
    fn regenerates_when_the_ca_is_corrupt() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(CA_CERT_FILE), "garbage").expect("書ける");
        std::fs::write(dir.path().join(CA_KEY_FILE), "garbage").expect("書ける");

        let ca = LocalCa::load_or_create(dir.path()).expect("壊れていても起動できる");
        assert!(ca.certificate_pem().contains("BEGIN CERTIFICATE"));
    }

    #[test]
    fn issues_certificates_for_nested_names() {
        // ワイルドカードでは賄えない深さ。これが動的発行の理由。
        let (_dir, ca) = temp_ca();

        for host in [
            "web.feat-1.myapp.localhost",
            "api.feature-user-auth.myapp.localhost",
            "web.myapp.localhost",
        ] {
            let certified = ca.issue(host).expect("発行できる");
            assert_eq!(
                certified.cert.len(),
                2,
                "leaf と CA を並べて送る必要がある: {host}"
            );
        }
    }

    #[test]
    fn caches_issued_certificates() {
        let (_dir, ca) = temp_ca();
        let resolver = DynamicCertResolver::new(Arc::new(ca));

        let first = resolver
            .certificate_for("web.feat-1.myapp.localhost")
            .expect("発行できる");
        let second = resolver
            .certificate_for("web.feat-1.myapp.localhost")
            .expect("キャッシュから返る");

        assert!(
            Arc::ptr_eq(&first, &second),
            "リクエストごとに発行すると handshake が遅くなる"
        );
    }

    #[test]
    fn cache_is_case_insensitive() {
        let (_dir, ca) = temp_ca();
        let resolver = DynamicCertResolver::new(Arc::new(ca));

        let lower = resolver
            .certificate_for("web.myapp.localhost")
            .expect("発行");
        let upper = resolver
            .certificate_for("WEB.MyApp.localhost")
            .expect("発行");

        assert!(Arc::ptr_eq(&lower, &upper));
    }

    #[test]
    fn advertises_only_http1() {
        let (_dir, ca) = temp_ca();
        let config = server_config(Arc::new(ca));

        assert_eq!(
            config.alpn_protocols,
            vec![b"http/1.1".to_vec()],
            "h2 を広告すると WebSocket の upgrade が使えなくなる"
        );
    }
}

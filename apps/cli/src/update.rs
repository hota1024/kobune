//! Finding out whether a newer build exists, and replacing this one with it.
//!
//! Every nightly reports version 0.1.0, so there is no version to compare.
//! What distinguishes two builds is the commit they came from, which
//! [`minato_core::BUILD_COMMIT`] carries and the release records as its
//! target. Comparing those two is the whole of the check.
//!
//! **Neither the background check nor the one `--version` makes writes to
//! stdout, and neither runs under `--json`.** An agent parses that stream,
//! and a line about a new build appearing in it would be a bug in Minato,
//! not a nuisance.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// Where the release lives.
const REPO: &str = "hota1024/minato";

/// The rolling build every merge to `main` replaces.
const CHANNEL: &str = "nightly";

/// How long a check is trusted before asking again.
///
/// The background check runs on ordinary commands, so this is the difference
/// between one request a day and one per invocation.
const CACHE_FOR: Duration = Duration::from_secs(24 * 60 * 60);

/// How long to wait on GitHub.
///
/// Short on purpose: the background check delays a command that has already
/// done its work, so a slow network has to cost almost nothing.
const CHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// A download can take longer, because the user asked for it.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// Set to anything non-empty to stop the checks — both the background one
/// and the one `--version` makes.
pub const NO_CHECK_ENV: &str = "MINATO_NO_UPDATE_CHECK";

/// What [`minato_core::BUILD_COMMIT`] says when there was no commit to
/// record: a source tarball, or a build from before it was recorded.
const NO_COMMIT: &str = "unknown";

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("cannot reach GitHub: {0}")]
    Unreachable(String),

    #[error("the release does not have a build for {0}")]
    NoArchive(String),

    #[error("the download does not match its checksum")]
    ChecksumMismatch,

    #[error("cannot replace {path}: {source}")]
    Replace {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{0}")]
    Other(String),
}

type Result<T> = std::result::Result<T, UpdateError>;

/// What the release API says, of the parts that matter.
#[derive(Debug, Deserialize)]
struct Release {
    /// The commit the release was built from.
    #[serde(default)]
    target_commitish: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

impl Release {
    fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.name == name)
    }
}

/// Whether a newer build is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Running the same commit the release was built from.
    Current,
    /// A different commit is published.
    Available { commit: String },
    /// This build predates the commit being recorded, or came from a source
    /// tarball. Nothing to compare, so nothing is claimed.
    Unknown,
}

impl Status {
    pub fn short(&self) -> Option<String> {
        match self {
            Self::Available { commit } => Some(commit.chars().take(7).collect()),
            _ => None,
        }
    }
}

/// Asks GitHub what the current build is.
pub async fn check() -> Result<Status> {
    let release = fetch_release().await?;
    Ok(compare(
        &release.target_commitish,
        minato_core::BUILD_COMMIT,
    ))
}

/// Compares a published commit with the running one.
///
/// Split out so the interesting part is testable without a network.
fn compare(published: &str, running: &str) -> Status {
    // A release edited by hand can carry a branch name here instead of a
    // commit, and comparing "main" against a commit would report an update
    // on every single run. Anything that is not a commit means unknown.
    if running == NO_COMMIT || !is_commit(published) {
        return Status::Unknown;
    }

    if published == running {
        Status::Current
    } else {
        Status::Available {
            commit: published.to_string(),
        }
    }
}

/// Whether this looks like a full commit hash.
fn is_commit(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|c| c.is_ascii_hexdigit())
}

async fn fetch_release() -> Result<Release> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/tags/{CHANNEL}");

    let response = client(CHECK_TIMEOUT)?
        .get(&url)
        .header("accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|err| UpdateError::Unreachable(err.to_string()))?;

    if !response.status().is_success() {
        return Err(UpdateError::Unreachable(format!(
            "GitHub answered {}",
            response.status()
        )));
    }

    response
        .json()
        .await
        .map_err(|err| UpdateError::Unreachable(err.to_string()))
}

fn client(timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        // GitHub rejects requests with no user agent.
        .user_agent(concat!("minato/", env!("CARGO_PKG_VERSION")))
        .timeout(timeout)
        .build()
        .map_err(|err| UpdateError::Other(err.to_string()))
}

/// What [`install`] is busy with.
///
/// An update replaces the binary someone is running, over a network, and
/// none of the four steps says anything on its own. This is what lets the
/// caller show which one is in hand — and how far the long one has got.
///
/// It says *what is happening*, not what to print: the wording and the
/// bar belong to [`crate::ui`], and `--json` shows none of it at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Pulling the archive down. `total` is `None` when the server did not
    /// say how big it is.
    Downloading {
        done: u64,
        total: Option<u64>,
    },
    Verifying,
    Unpacking,
    Installing,
}

/// Replaces this installation with the published build.
///
/// Returns the commit installed. Both binaries are replaced together: the
/// CLI starts the daemon by looking next to itself, so a pair from
/// different builds would speak whatever protocol each happened to have.
///
/// `report` is called as the work moves along, often during the download.
/// It must be cheap: it runs once per chunk off the socket.
pub async fn install(report: impl Fn(Stage)) -> Result<String> {
    let release = fetch_release().await?;

    let archive_name = format!("minato-{}.tar.gz", minato_core::BUILD_TARGET);
    let archive = release
        .asset(&archive_name)
        .ok_or_else(|| UpdateError::NoArchive(minato_core::BUILD_TARGET.to_string()))?;
    let checksum = release
        .asset(&format!("{archive_name}.sha256"))
        .ok_or_else(|| UpdateError::NoArchive(minato_core::BUILD_TARGET.to_string()))?;

    let bytes = download_watched(&archive.browser_download_url, |done, total| {
        report(Stage::Downloading { done, total })
    })
    .await?;

    // The checksum file is one line, so it is not worth a stage of its own.
    let expected = download(&checksum.browser_download_url).await?;

    report(Stage::Verifying);
    verify(&bytes, &expected)?;

    report(Stage::Unpacking);
    let binaries = unpack(&bytes)?;

    report(Stage::Installing);
    replace(&binaries)?;

    Ok(release.target_commitish)
}

async fn download(url: &str) -> Result<Vec<u8>> {
    download_watched(url, |_, _| {}).await
}

/// Reads the body as it arrives, saying how much of it there is so far.
///
/// `Response::bytes` would be shorter and would also hand back nothing at
/// all until the last byte lands — which for a release archive is the
/// thirty seconds someone spends wondering whether it has hung.
async fn download_watched(url: &str, report: impl Fn(u64, Option<u64>)) -> Result<Vec<u8>> {
    let mut response = client(DOWNLOAD_TIMEOUT)?
        .get(url)
        .send()
        .await
        .map_err(|err| UpdateError::Unreachable(err.to_string()))?;

    if !response.status().is_success() {
        return Err(UpdateError::Unreachable(format!(
            "{url} answered {}",
            response.status()
        )));
    }

    let total = response.content_length();

    // Reserved from what the server claims, but only up to a point: the
    // length is not a fact until the bytes turn up, and a header saying
    // 40 GB should not be an allocation.
    let mut bytes = Vec::with_capacity(total.unwrap_or(0).min(RESERVE_LIMIT) as usize);

    // So the bar appears with the step rather than at the first chunk.
    report(0, total);

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| UpdateError::Unreachable(err.to_string()))?
    {
        bytes.extend_from_slice(&chunk);
        report(bytes.len() as u64, total);
    }

    Ok(bytes)
}

/// The most a claimed Content-Length may reserve up front.
const RESERVE_LIMIT: u64 = 128 * 1024 * 1024;

/// Checks the archive against the published `.sha256`.
///
/// The file is in `sha256sum` format — the digest, whitespace, the filename —
/// so only the first field is compared.
fn verify(bytes: &[u8], checksum_file: &[u8]) -> Result<()> {
    use sha2::{Digest, Sha256};

    let text = String::from_utf8_lossy(checksum_file);
    let expected = text
        .split_whitespace()
        .next()
        .ok_or(UpdateError::ChecksumMismatch)?;

    let actual = format!("{:x}", Sha256::digest(bytes));

    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(UpdateError::ChecksumMismatch)
    }
}

/// Pulls the two binaries out of the archive.
fn unpack(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    use std::io::Read;

    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let mut found = Vec::new();

    let entries = archive
        .entries()
        .map_err(|err| UpdateError::Other(format!("cannot read the archive: {err}")))?;

    for entry in entries {
        let mut entry =
            entry.map_err(|err| UpdateError::Other(format!("cannot read the archive: {err}")))?;

        let path = entry
            .path()
            .map_err(|err| UpdateError::Other(err.to_string()))?
            .to_path_buf();

        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        if !matches!(name, "minato" | "minatod") {
            continue;
        }

        let name = name.to_string();
        let mut contents = Vec::new();
        entry
            .read_to_end(&mut contents)
            .map_err(|err| UpdateError::Other(err.to_string()))?;

        found.push((name, contents));
    }

    if found.len() != 2 {
        return Err(UpdateError::Other(format!(
            "the archive holds {} of the two binaries",
            found.len()
        )));
    }

    Ok(found)
}

/// Writes the new binaries over the installed ones.
///
/// Each goes to a temporary file beside its target and is then renamed into
/// place. A running executable cannot be written to, but it can be replaced:
/// `rename` swaps the directory entry and leaves the running process on the
/// old inode until it exits. Writing in place would fail with ETXTBSY, and
/// deleting first would leave nothing behind if the write then failed.
fn replace(binaries: &[(String, Vec<u8>)]) -> Result<()> {
    let dir = install_dir()?;

    for (name, contents) in binaries {
        let target = dir.join(name);
        let temporary = dir.join(format!(".{name}.new"));

        write_executable(&temporary, contents).map_err(|source| UpdateError::Replace {
            path: temporary.clone(),
            source,
        })?;

        std::fs::rename(&temporary, &target).map_err(|source| {
            // Leaving the temporary behind on failure would have the next
            // run trip over it.
            let _ = std::fs::remove_file(&temporary);
            UpdateError::Replace {
                path: target.clone(),
                source,
            }
        })?;
    }

    Ok(())
}

fn write_executable(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(contents)?;
    file.sync_all()?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }

    Ok(())
}

/// Where the running `minato` lives.
///
/// The update goes beside the binary that asked for it, not to a configured
/// directory: updating an installation other than the one being run is
/// almost never what was meant.
pub fn install_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe()
        .map_err(|err| UpdateError::Other(format!("cannot find the running binary: {err}")))?;

    // Resolve symlinks so an update through a link lands on the real file
    // rather than replacing the link with a binary.
    let exe = exe.canonicalize().unwrap_or(exe);

    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| UpdateError::Other("the running binary has no directory".to_string()))
}

// ---------------------------------------------------------------------------
// The background check
// ---------------------------------------------------------------------------

/// What the last check found, and when.
#[derive(Debug, Serialize, Deserialize)]
struct Cache {
    /// Seconds since the epoch.
    checked_at: u64,
    /// The commit the release carried at that point.
    #[serde(default)]
    published: String,
}

/// Runs the once-a-day check, and returns the commit worth mentioning if
/// there is one.
///
/// Every failure is silent. A check that cannot reach GitHub has found
/// nothing to say, and saying so would interrupt a command that worked.
pub async fn background_notice(paths: &minato_core::Paths) -> Option<String> {
    if refused() {
        return None;
    }

    let cache_path = cache_path(paths);

    if let Some(cache) = read_cache(&cache_path)
        && !is_stale(&cache)
    {
        // Report from the cache rather than going quiet between checks,
        // otherwise the notice appears once a day and is missed.
        return notice(compare(&cache.published, minato_core::BUILD_COMMIT));
    }

    fresh_notice(&cache_path).await
}

/// The check `--version` makes.
///
/// Fresh every time, where the background one asks at most once a day:
/// `--version` is someone asking what they are running, and answering that
/// out of a cache up to a day old would be answering a different question.
///
/// It leaves what it found in the cache all the same — what has just been
/// fetched is newer than whatever the background check has.
pub async fn version_notice(paths: &minato_core::Paths) -> Option<String> {
    if refused() {
        return None;
    }

    fresh_notice(&cache_path(paths)).await
}

/// Asks GitHub, records the answer, and says what is worth mentioning.
///
/// Silent on every failure: both callers print the notice beside output that
/// is already correct, and a network that is down has nothing to add to it.
async fn fresh_notice(cache_path: &Path) -> Option<String> {
    // A build with no commit of its own comes to `Unknown` whatever the
    // release turns out to say, so the request is not worth making.
    if minato_core::BUILD_COMMIT == NO_COMMIT {
        return None;
    }

    let release = fetch_release().await.ok()?;
    let status = compare(&release.target_commitish, minato_core::BUILD_COMMIT);

    write_cache(
        cache_path,
        &Cache {
            checked_at: now(),
            published: release.target_commitish,
        },
    );

    notice(status)
}

/// Whether the check has been turned off.
fn refused() -> bool {
    std::env::var_os(NO_CHECK_ENV).is_some_and(|value| !value.is_empty())
}

fn cache_path(paths: &minato_core::Paths) -> PathBuf {
    paths.root().join("update-check.json")
}

/// The commit to mention, shortened. `None` when there is nothing to say.
///
/// The wording is the UI's — this module decides *whether* there is a
/// notice, not how it reads.
fn notice(status: Status) -> Option<String> {
    status.short()
}

fn is_stale(cache: &Cache) -> bool {
    now().saturating_sub(cache.checked_at) >= CACHE_FOR.as_secs()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

fn read_cache(path: &Path) -> Option<Cache> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Best effort. A cache that cannot be written costs one request per run,
/// which is not worth a message about.
fn write_cache(path: &Path, cache: &Cache) {
    if let Ok(text) = serde_json::to_string(cache) {
        let _ = std::fs::write(path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branch_name_in_place_of_a_commit_is_unknown() {
        // Reporting an update forever is worse than reporting nothing.
        assert_eq!(
            compare("main", "c7282b8530f6408ba5048b2721e24d7cb33425b0"),
            Status::Unknown
        );
    }

    /// Two commits that differ, at the length GitHub reports.
    const ONE: &str = "c7282b8530f6408ba5048b2721e24d7cb33425b0";
    const TWO: &str = "56a3859f1d0a4e4b9c7f2e6d8a1b3c5d7e9f0a12";

    #[test]
    fn the_same_commit_is_current() {
        assert_eq!(compare(ONE, ONE), Status::Current);
    }

    #[test]
    fn a_different_commit_is_available() {
        assert_eq!(compare(TWO, ONE), Status::Available { commit: TWO.into() });
    }

    #[test]
    fn a_build_with_no_commit_claims_nothing() {
        // A source tarball, or a build from before the commit was recorded.
        // "You are out of date" would be a guess, and acting on it would
        // replace a build the user may have made deliberately.
        assert_eq!(compare(TWO, "unknown"), Status::Unknown);
    }

    #[test]
    fn an_empty_published_commit_claims_nothing() {
        // A release with no target recorded. Treating that as "different"
        // would tell everyone to update, every time.
        assert_eq!(compare("", ONE), Status::Unknown);
    }

    #[test]
    fn only_an_available_update_produces_a_notice() {
        assert!(notice(Status::Current).is_none());
        assert!(notice(Status::Unknown).is_none());

        let commit = notice(Status::Available {
            commit: "0123456789abcdef".into(),
        })
        .expect("has a notice");

        // Shortened: the full hash tells a reader nothing the first seven
        // do not.
        assert_eq!(commit, "0123456");
    }

    #[test]
    fn a_fresh_cache_is_not_stale() {
        assert!(!is_stale(&Cache {
            checked_at: now(),
            published: "abc".into(),
        }));
    }

    #[test]
    fn a_day_old_cache_is_stale() {
        assert!(is_stale(&Cache {
            checked_at: now() - CACHE_FOR.as_secs(),
            published: "abc".into(),
        }));
    }

    #[test]
    fn a_cache_from_the_future_is_not_stale() {
        // A clock that moved backwards should not mean a request per run.
        assert!(!is_stale(&Cache {
            checked_at: now() + 1_000,
            published: "abc".into(),
        }));
    }

    #[test]
    fn checksums_are_compared_against_the_first_field() {
        use sha2::Digest;

        // The file is in `sha256sum` format: digest, whitespace, filename.
        let contents = b"hello";
        let digest = format!("{:x}", sha2::Sha256::digest(contents));

        let file = format!("{digest}  minato-x86_64-unknown-linux-gnu.tar.gz\n");
        verify(contents, file.as_bytes()).expect("matches");

        // Case is not significant in hex.
        let upper = format!("{}  file\n", digest.to_uppercase());
        verify(contents, upper.as_bytes()).expect("matches regardless of case");
    }

    #[test]
    fn a_wrong_checksum_is_refused() {
        let file = format!("{}  file\n", "0".repeat(64));
        let err = verify(b"hello", file.as_bytes()).unwrap_err();

        assert!(matches!(err, UpdateError::ChecksumMismatch), "got: {err}");
    }

    #[test]
    fn an_empty_checksum_file_is_refused() {
        // Rather than treated as a match, which is what comparing against
        // nothing would amount to.
        assert!(verify(b"hello", b"").is_err());
        assert!(verify(b"hello", b"\n").is_err());
    }

    #[test]
    fn unpacking_needs_both_binaries() {
        // A half-applied update leaves a CLI and a daemon from different
        // builds in one directory, speaking whatever protocol each had.
        let mut builder = tar::Builder::new(Vec::new());
        let contents = b"binary";

        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "minato-x/minato", contents.as_slice())
            .expect("appends");

        let tarball = builder.into_inner().expect("builds");
        let mut gz = Vec::new();
        {
            use std::io::Write as _;
            let mut encoder = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::fast());
            encoder.write_all(&tarball).expect("writes");
            encoder.finish().expect("finishes");
        }

        let err = unpack(&gz).unwrap_err();
        assert!(err.to_string().contains("1 of the two"), "got: {err}");
    }
}

//! Resolving secret references.
//!
//! **A resolved value never touches disk.** It goes to the container in
//! memory and no further, which is what lets the repository hold nothing
//! but references.
//!
//! A failure here does not take the daemon down. Usually it just means
//! nobody is signed in to 1Password, and letting that keep the whole
//! environment from starting is the worse outcome. Failures come back as
//! warnings, and only that key is dropped.

use std::collections::HashMap;

use minato_core::SecretRef;
use tokio::process::Command;

/// How long to wait for an external command.
///
/// 1Password can stop and ask to be signed in. The daemon has no way to
/// answer, so it gives up rather than wait forever.
const RESOLVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// What resolution produced.
pub struct Resolved {
    /// The values that resolved.
    pub values: HashMap<String, String>,
    /// The keys that did not, and why.
    pub failures: Vec<(String, String)>,
}

/// Resolves secret references.
///
/// `entries` is a list of (key, reference). Plain values do not belong
/// here.
pub async fn resolve(entries: &[(String, SecretRef)]) -> Resolved {
    let mut values = HashMap::new();
    let mut failures = Vec::new();

    // Several keys can share one reference. Fetch it once.
    let mut cache: HashMap<SecretRef, Result<String, String>> = HashMap::new();

    for (key, reference) in entries {
        let outcome = match cache.get(reference) {
            Some(cached) => cached.clone(),
            None => {
                let fetched = fetch(reference).await;
                cache.insert(reference.clone(), fetched.clone());
                fetched
            }
        };

        match outcome {
            Ok(value) => {
                values.insert(key.clone(), value);
            }
            Err(reason) => failures.push((key.clone(), reason)),
        }
    }

    Resolved { values, failures }
}

async fn fetch(reference: &SecretRef) -> Result<String, String> {
    match reference {
        SecretRef::Env(name) => {
            std::env::var(name).map_err(|_| format!("the daemon's environment has no `{name}`"))
        }
        SecretRef::OnePassword(uri) => run("op", &["read", "--no-newline", uri]).await,
        SecretRef::Keychain { service, account } => {
            run(
                "security",
                &["find-generic-password", "-s", service, "-a", account, "-w"],
            )
            .await
        }
    }
}

async fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let output = tokio::time::timeout(RESOLVE_TIMEOUT, Command::new(program).args(args).output())
        .await
        .map_err(|_| {
            format!(
                "{program} did not answer within {} seconds (it may be asking to be signed in)",
                RESOLVE_TIMEOUT.as_secs()
            )
        })?;

    let output = output.map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            format!("no `{program}` found")
        } else {
            format!("cannot run `{program}`: {err}")
        }
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!(
                "`{program}` failed with exit code {}",
                output.status.code().unwrap_or(-1)
            )
        } else {
            stderr
        });
    }

    // `security -w` adds a trailing newline. Left in the value, it makes
    // authentication fail.
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end_matches('\n')
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolves_from_the_daemon_environment() {
        // SAFETY: tests run concurrently in one process, but this
        // variable name is used by this test alone.
        unsafe { std::env::set_var("MINATO_TEST_SECRET", "s3cret") };

        let resolved = resolve(&[(
            "API_KEY".to_string(),
            SecretRef::Env("MINATO_TEST_SECRET".into()),
        )])
        .await;

        assert_eq!(
            resolved.values.get("API_KEY").map(String::as_str),
            Some("s3cret")
        );
        assert!(resolved.failures.is_empty());
    }

    #[tokio::test]
    async fn missing_environment_variable_is_reported_not_fatal() {
        let resolved = resolve(&[(
            "API_KEY".to_string(),
            SecretRef::Env("MINATO_TEST_DEFINITELY_UNSET".into()),
        )])
        .await;

        assert!(resolved.values.is_empty());
        assert_eq!(resolved.failures.len(), 1);
        assert_eq!(resolved.failures[0].0, "API_KEY");
        assert!(resolved.failures[0].1.contains("no `"));
    }

    #[tokio::test]
    async fn other_keys_survive_one_failure() {
        // One unresolvable key must not keep the whole environment down.
        unsafe { std::env::set_var("MINATO_TEST_OK", "fine") };

        let resolved = resolve(&[
            (
                "BAD".to_string(),
                SecretRef::Env("MINATO_TEST_MISSING".into()),
            ),
            ("GOOD".to_string(), SecretRef::Env("MINATO_TEST_OK".into())),
        ])
        .await;

        assert_eq!(
            resolved.values.get("GOOD").map(String::as_str),
            Some("fine")
        );
        assert_eq!(resolved.failures.len(), 1);
    }

    #[tokio::test]
    async fn missing_program_is_reported_clearly() {
        let err = run("minato-definitely-not-a-program", &[])
            .await
            .unwrap_err();

        assert!(err.contains("no `"), "got: {err}");
    }

    #[tokio::test]
    async fn trailing_newline_is_stripped() {
        // Both `security -w` and `op read` can add a newline. Left in
        // the value it fails authentication, and the cause is very hard to
        // see.
        let value = run("printf", &["token\n"]).await.expect("runs");
        assert_eq!(value, "token");
    }

    #[tokio::test]
    async fn nonzero_exit_carries_stderr() {
        let err = run("sh", &["-c", "echo details >&2; exit 1"])
            .await
            .unwrap_err();

        assert!(err.contains("details"), "got: {err}");
    }

    #[tokio::test]
    async fn identical_references_are_fetched_once() {
        unsafe { std::env::set_var("MINATO_TEST_SHARED", "shared") };

        let reference = SecretRef::Env("MINATO_TEST_SHARED".into());
        let resolved = resolve(&[
            ("A".to_string(), reference.clone()),
            ("B".to_string(), reference),
        ])
        .await;

        assert_eq!(resolved.values.len(), 2);
        assert_eq!(resolved.values.get("A"), resolved.values.get("B"));
    }
}

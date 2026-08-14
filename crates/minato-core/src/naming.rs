//! Deriving DNS labels from branch and project names.
//!
//! The names produced here appear in URLs and cannot change once issued.
//! The sanitised result is therefore persisted in the state store, so that
//! changing these rules does not change existing URLs (see `state`).

use sha2::{Digest, Sha256};

/// The maximum length of a single DNS label (RFC 1035).
pub const MAX_LABEL_LEN: usize = 63;

/// How far to truncate an over-long label. The rest becomes a hash suffix.
const TRUNCATED_STEM_LEN: usize = 55;

/// The length of the hash suffix.
const HASH_SUFFIX_LEN: usize = 7;

/// Used when sanitisation leaves nothing behind.
const FALLBACK: &str = "unnamed";

/// Normalises an arbitrary string into a usable DNS label.
///
/// - Lowercases, and replaces anything outside `[a-z0-9-]` with `-`
/// - Collapses runs of `-` and trims them from both ends
/// - Truncates past [`MAX_LABEL_LEN`], appending a hash of the original
///
/// If characters other than separators (`/` `_` `-` `.` space) were
/// dropped, a hash is appended to show that information was lost.
///
/// ```
/// # use minato_core::naming::sanitize_label;
/// assert_eq!(sanitize_label("feature/user-auth"), "feature-user-auth");
///
/// // Dropped characters are replaced by a hash so branches stay distinct.
/// let label = sanitize_label("feature/デモ環境");
/// assert!(label.starts_with("feature-"));
/// assert_ne!(label, sanitize_label("feature/検証環境"));
/// ```
pub fn sanitize_label(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = true; // start true so a leading `-` is dropped
    // Whether information was lost. Non-ASCII branch names are used in
    // practice, and without a hash two different non-ASCII branches
    // would collapse to the same plain label.
    let mut lost_information = false;

    for ch in input.chars() {
        let mapped = match ch {
            'a'..='z' | '0'..='9' => ch,
            'A'..='Z' => ch.to_ascii_lowercase(),
            // Characters used as separators do not count as information.
            '/' | '_' | '-' | '.' | ' ' => '-',
            _ => {
                lost_information = true;
                '-'
            }
        };

        if mapped == '-' {
            if !prev_dash {
                out.push('-');
                prev_dash = true;
            }
        } else {
            out.push(mapped);
            prev_dash = false;
        }
    }

    while out.ends_with('-') {
        out.pop();
    }

    if out.is_empty() {
        return format!("{FALLBACK}-{}", short_hash(input));
    }

    if lost_information {
        // Derived from the original, so the same input always maps to
        // the same label.
        return truncate_with_hash(&out, input);
    }

    if out.len() > MAX_LABEL_LEN {
        return truncate_with_hash(&out, input);
    }

    out
}

/// Fits a label within the limit and appends a hash of the input.
fn truncate_with_hash(label: &str, seed: &str) -> String {
    let mut stem = if label.len() > TRUNCATED_STEM_LEN {
        label[..TRUNCATED_STEM_LEN].to_string()
    } else {
        label.to_string()
    };

    // Make sure the stem does not end in `-` before appending.
    while stem.ends_with('-') {
        stem.pop();
    }

    if stem.is_empty() {
        return format!("{FALLBACK}-{}", short_hash(seed));
    }

    format!("{stem}-{}", short_hash(seed))
}

/// Produces a distinguishable alternative when a sanitised label clashes.
///
/// Pass something that uniquely identifies the clashing item as `seed` —
/// the original branch name, for instance.
pub fn disambiguate(label: &str, seed: &str) -> String {
    let suffix = short_hash(seed);
    let budget = MAX_LABEL_LEN - HASH_SUFFIX_LEN - 1;

    let mut stem = if label.len() > budget {
        label[..budget].to_string()
    } else {
        label.to_string()
    };
    while stem.ends_with('-') {
        stem.pop();
    }

    format!("{stem}-{suffix}")
}

/// Whether a label is already normalised.
pub fn is_valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= MAX_LABEL_LEN
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !label.contains("--")
}

/// Builds the hostname assigned to a service.
///
/// For the main worktree (`workspace` is `None`) the workspace label is
/// omitted, giving `{service}.{project}.{suffix}`.
pub fn service_host(service: &str, workspace: Option<&str>, project: &str, suffix: &str) -> String {
    match workspace {
        Some(ws) => format!("{service}.{ws}.{project}.{suffix}"),
        None => format!("{service}.{project}.{suffix}"),
    }
}

/// Builds a service hostname from an explicit domain.
///
/// With `[project] domain` set, the project name does not appear in the
/// suffix, so use this rather than [`service_host`].
pub fn service_host_in(service: &str, workspace: Option<&str>, domain: &str) -> String {
    match workspace {
        Some(ws) => format!("{service}.{ws}.{domain}"),
        None => format!("{service}.{domain}"),
    }
}

/// The hostname used for Cloudflare Tunnel.
///
/// **One label under the zone**, so the project joins service and workspace
/// with `-` rather than sitting in a label of its own. Cloudflare's
/// Universal SSL covers the apex and first-level subdomains only, and
/// anything deeper needs a certificate nobody has by default — a two-level
/// hostname reaches the edge and is refused there, with no local symptom
/// (see `docs/DESIGN.md` §9).
///
/// The joined parts are already sanitised labels, so this is
/// [`sanitize_label`] only for the length: three names concatenated can
/// pass [`MAX_LABEL_LEN`], and an over-long label is not a hostname at all.
///
/// **Two projects can join into the same label.** Service `web` of
/// project `myapp-x`'s main worktree and service `web` of workspace
/// `myapp` in project `x` both give `web-myapp-x`. The two-level form
/// could not do this — `web.myapp-x` and `web-myapp.x` are different
/// names — so flattening is what introduces it, and the loser is not
/// obvious from the outside: the routing table is keyed by hostname, so
/// whichever project refreshed last serves both URLs.
///
/// Left as it is, with the proxy logging the clash when it registers the
/// route (`Routes::replace_project`), because the alternative is an escape
/// that shows up in every URL for a collision that needs one project to be
/// named the tail of another's label.
pub fn tunnel_host(service: &str, workspace: Option<&str>, project: &str, domain: &str) -> String {
    let label = match workspace {
        Some(ws) => format!("{service}-{ws}-{project}"),
        None => format!("{service}-{project}"),
    };

    format!("{}.{domain}", sanitize_label(&label))
}

fn short_hash(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let hex = format!("{digest:x}");
    hex[..HASH_SUFFIX_LEN].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_disallowed_characters() {
        assert_eq!(sanitize_label("feature/user-auth"), "feature-user-auth");
        assert_eq!(sanitize_label("release/v1.2.3"), "release-v1-2-3");

        // `#` is not a separator, so its loss is marked with a hash.
        let hashed = sanitize_label("FIX_Bug#123");
        assert!(hashed.starts_with("fix-bug-123-"), "got: {hashed}");
    }

    #[test]
    fn collapses_and_trims_dashes() {
        assert_eq!(sanitize_label("--a//b--"), "a-b");
        assert_eq!(sanitize_label("a___b"), "a-b");
    }

    #[test]
    fn keeps_japanese_branches_distinguishable() {
        // Non-ASCII branch names are used in practice. Without a hash for
        // the dropped characters they all collapse to the same label and
        // the URLs collide.
        let a = sanitize_label("feature/デモ環境");
        let b = sanitize_label("feature/検証環境");

        assert_ne!(a, b, "two branches must not share a label");
        assert!(a.starts_with("feature-"), "keep the readable part: {a}");
        assert!(is_valid_label(&a), "{a}");
        assert!(is_valid_label(&b), "{b}");

        // Deterministic. A URL never changes once issued.
        assert_eq!(a, sanitize_label("feature/デモ環境"));
    }

    #[test]
    fn ascii_only_branches_stay_clean() {
        // Separator-only substitution gets no hash; it would hurt readability.
        assert_eq!(sanitize_label("feature/user-auth"), "feature-user-auth");
        assert_eq!(sanitize_label("fix_bug.123"), "fix-bug-123");
        assert_eq!(sanitize_label("release/v1.2.3"), "release-v1-2-3");
    }

    #[test]
    fn falls_back_when_nothing_remains() {
        let out = sanitize_label("///");
        assert!(out.starts_with("unnamed-"));
        assert!(is_valid_label(&out));
    }

    #[test]
    fn truncates_long_input_with_stable_hash() {
        let long = "feature/".to_string() + &"x".repeat(120);
        let a = sanitize_label(&long);
        let b = sanitize_label(&long);

        assert_eq!(a, b, "the same input must yield the same label");
        assert!(a.len() <= MAX_LABEL_LEN);
        assert!(is_valid_label(&a));
    }

    #[test]
    fn different_long_inputs_do_not_collide() {
        let a = sanitize_label(&("feature/".to_string() + &"x".repeat(120)));
        let b = sanitize_label(&("feature/".to_string() + &"x".repeat(121)));
        assert_ne!(a, b);
    }

    #[test]
    fn sanitized_output_is_idempotent() {
        // The output is ASCII-only, so a second pass adds nothing.
        for input in ["feature/user-auth", "release/v1.2.3", "a___b"] {
            let once = sanitize_label(input);
            assert_eq!(sanitize_label(&once), once, "input = {input}");
        }
    }

    #[test]
    fn disambiguate_stays_within_limits() {
        let long = "a".repeat(MAX_LABEL_LEN);
        let out = disambiguate(&long, "seed");
        assert!(out.len() <= MAX_LABEL_LEN);
        assert!(is_valid_label(&out));
    }

    #[test]
    fn builds_hostnames_from_a_domain() {
        assert_eq!(
            service_host_in("web", Some("feat-1"), "myapp.localhost"),
            "web.feat-1.myapp.localhost"
        );
        assert_eq!(
            service_host_in("web", None, "myapp.localhost"),
            "web.myapp.localhost"
        );
        // With an explicit domain the project name does not appear.
        assert_eq!(
            service_host_in("api", Some("feat-1"), "dev.example.com"),
            "api.feat-1.dev.example.com"
        );
    }

    #[test]
    fn builds_hostnames() {
        assert_eq!(
            service_host("web", Some("feat-1"), "myapp", "localhost"),
            "web.feat-1.myapp.localhost"
        );
        assert_eq!(
            service_host("web", None, "myapp", "localhost"),
            "web.myapp.localhost"
        );
        assert_eq!(
            tunnel_host("web", Some("feat-1"), "myapp", "example.com"),
            "web-feat-1-myapp.example.com"
        );
    }

    #[test]
    fn a_tunnel_hostname_is_one_label_under_the_zone() {
        // Universal SSL stops at the first level. A second label reaches
        // Cloudflare and is refused at the TLS handshake, which looks
        // nothing like a Minato problem from the outside.
        let host = tunnel_host("web", Some("feat-1"), "myapp", "example.com");
        let label = host.strip_suffix(".example.com").expect("under the zone");

        assert!(!label.contains('.'), "got: {host}");
        assert!(is_valid_label(label), "got: {label}");
    }

    #[test]
    fn the_main_worktree_leaves_out_the_workspace() {
        assert_eq!(
            tunnel_host("web", None, "myapp", "example.com"),
            "web-myapp.example.com"
        );
    }

    #[test]
    fn a_long_tunnel_hostname_stays_a_hostname() {
        // Three names that each fit can still join into something past
        // the label limit, and an over-long label is not resolvable.
        let workspace = "a".repeat(MAX_LABEL_LEN);
        let host = tunnel_host("web", Some(&workspace), "myapp", "example.com");
        let label = host.strip_suffix(".example.com").expect("under the zone");

        assert!(is_valid_label(label), "got: {label}");
    }
}

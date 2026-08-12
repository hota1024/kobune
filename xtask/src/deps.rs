//! Checking that dependencies point the way `docs/DESIGN.md` §13 says.
//!
//! ```text
//! apps/cli ────┐
//! apps/desktop ┴──> minato-client ──> minato-api ──> minato-core
//! apps/daemon ─────────────────────>  minato-api ──> minato-core
//!        └──> minato-runtime / minato-proxy / minato-dns / minato-tunnel
//! ```
//!
//! **The rule is that no client-side crate may reach the daemon's.** Not
//! for tidiness: `minato-runtime` is where Docker lives, and a GUI that
//! could reach it would eventually talk to Docker directly rather than
//! through the daemon — which is the one architectural rule the whole
//! design rests on (§3, "the daemon's API is the product").
//!
//! It is held today. This exists so that it stays that way without
//! anybody having to remember, which is what the design already claimed
//! was happening.
//!
//! `cargo tree` rather than `cargo metadata`, so xtask needs no
//! dependencies of its own to parse JSON with.

use std::path::Path;
use std::process::Command;

/// Crates that must never reach the daemon's own.
const CLIENTS: &[&str] = &["minato-client", "minato-desktop"];

/// The daemon's own. Docker, the proxy, DNS and the tunnel process.
const DAEMON_ONLY: &[&str] = &[
    "minato-runtime",
    "minato-proxy",
    "minato-dns",
    "minato-tunnel",
];

pub fn check(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut violations = Vec::new();

    for client in CLIENTS {
        let reached = dependencies_of(root, client)?;

        for forbidden in DAEMON_ONLY {
            if reached.iter().any(|name| name == forbidden) {
                violations.push(format!("{client} reaches {forbidden}"));
            }
        }
    }

    if !violations.is_empty() {
        return Err(format!(
            "the direction of dependencies is broken (docs/DESIGN.md §13):\n  {}\n\n\
             A client-side crate may not reach the daemon's. Everything goes\n\
             through minato-api and the daemon behind it.",
            violations.join("\n  ")
        )
        .into());
    }

    println!(
        "dependency direction holds: {} reach none of {}",
        CLIENTS.join(", "),
        DAEMON_ONLY.join(", ")
    );

    Ok(())
}

/// Every crate `package` pulls in, normal dependencies only.
///
/// Dev-dependencies are excluded on purpose: a test reaching for a
/// runtime says nothing about what ships.
fn dependencies_of(root: &Path, package: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args([
            "tree",
            "-p",
            package,
            "-e",
            "normal",
            "--prefix",
            "none",
            "--no-dedupe",
        ])
        .output()
        .map_err(|err| format!("cannot run cargo tree: {err}"))?;

    if !output.status.success() {
        return Err(format!(
            "cargo tree failed for {package}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        // `name v0.1.0 (/path)` — the name is the first field.
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lists_do_not_overlap() {
        // A crate in both would make the check contradict itself, and it
        // would read as a failure of the rule rather than of the list.
        for client in CLIENTS {
            assert!(
                !DAEMON_ONLY.contains(client),
                "{client} cannot be both a client and the daemon's"
            );
        }
    }

    #[test]
    fn the_rule_holds_in_this_repository() {
        // The check checking itself. If this ever fails, either the rule
        // was broken or `cargo tree`'s output changed shape — and both
        // are worth finding out about here rather than in CI.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask/ has a parent")
            .to_path_buf();

        check(&root).expect("the direction of dependencies holds");
    }
}

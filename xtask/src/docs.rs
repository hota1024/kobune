//! Freezing the documentation for a release.
//!
//! VitePress has no versioning of its own. What it does have is locales
//! keyed by directory, and `docs/.vitepress/config.ts` generates one locale
//! per (version, language) pair from `versions.json`. So a snapshot is a
//! copy of the tree plus a line in that file — no configuration to edit,
//! and the sidebar and version switcher follow.
//!
//! Current docs live at the root and are the ones you edit. Snapshots are
//! read-only history; correcting a released page means editing it in place
//! and knowing you are editing history.

use std::error::Error;
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// What gets copied. Everything else under `docs/` is machinery.
const CONTENT: &[&str] = &["index.md", "guide", "reference", "tutorials", "ja"];

/// Freezes the current documentation as `/v<version>/`.
pub fn snapshot(root: &Path, version: &str) -> Result<()> {
    let version = version.trim_start_matches('v');
    validate_version(version)?;

    let docs = root.join("docs");
    if !docs.join("index.md").is_file() {
        return Err(format!("no documentation at {}", docs.display()).into());
    }

    let target = docs.join(format!("v{version}"));
    if target.exists() {
        return Err(format!(
            "v{version} has already been snapshotted ({}). Delete it first to redo it",
            target.display()
        )
        .into());
    }

    for entry in CONTENT {
        let from = docs.join(entry);
        if !from.exists() {
            return Err(format!("{} is missing", from.display()).into());
        }
        copy(&from, &target.join(entry))?;
    }

    rewrite_absolute_links(&target, version)?;
    register(&docs, version)?;

    println!("snapshotted the docs as v{version} at {}", target.display());
    println!();
    println!("The root still holds the docs you edit. Bump CURRENT in");
    println!("docs/.vitepress/config.ts to the next version you are writing for.");

    Ok(())
}

/// Rejects anything that would not make a usable URL segment.
fn validate_version(version: &str) -> Result<()> {
    let usable = !version.is_empty()
        && version
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
        && version.starts_with(|c: char| c.is_ascii_digit());

    if usable {
        Ok(())
    } else {
        Err(format!("`{version}` is not a usable version. Try something like 0.1").into())
    }
}

fn copy(from: &Path, to: &Path) -> Result<()> {
    if from.is_file() {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(from, to)?;
        return Ok(());
    }

    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        copy(&entry.path(), &to.join(entry.file_name()))?;
    }

    Ok(())
}

/// Points a snapshot's absolute links back inside itself.
///
/// Relative links (`./installation`, `../reference/cli`) survive a copy
/// untouched. Absolute ones do not: the home page's `/guide/getting-started`
/// would send a reader of v0.1 to the current docs, which is the one thing a
/// versioned site exists to prevent.
fn rewrite_absolute_links(target: &Path, version: &str) -> Result<()> {
    for path in markdown_files(target)? {
        let text = std::fs::read_to_string(&path)?;

        // Longest first: `/ja/` has to be rewritten before `/` swallows it.
        let rewritten = text
            .replace("(/ja/", &format!("(/v{version}/ja/"))
            .replace("(/guide/", &format!("(/v{version}/guide/"))
            .replace("(/reference/", &format!("(/v{version}/reference/"))
            .replace("(/tutorials/", &format!("(/v{version}/tutorials/"))
            .replace("link: /ja/", &format!("link: /v{version}/ja/"))
            .replace("link: /guide/", &format!("link: /v{version}/guide/"))
            .replace(
                "link: /reference/",
                &format!("link: /v{version}/reference/"),
            )
            .replace(
                "link: /tutorials/",
                &format!("link: /v{version}/tutorials/"),
            );

        if rewritten != text {
            std::fs::write(&path, rewritten)?;
        }
    }

    Ok(())
}

fn markdown_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();

    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();

        if path.is_dir() {
            found.extend(markdown_files(&path)?);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            found.push(path);
        }
    }

    Ok(found)
}

/// Adds the version to `versions.json`, newest first.
///
/// Hand-rolled rather than pulling in serde_json for one array of strings.
fn register(docs: &Path, version: &str) -> Result<()> {
    let path = docs.join(".vitepress").join("versions.json");
    let existing = std::fs::read_to_string(&path).unwrap_or_else(|_| "[]".to_string());

    let mut versions: Vec<String> = existing
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|entry| entry.trim().trim_matches('"').to_string())
        .filter(|entry| !entry.is_empty())
        .collect();

    if versions.iter().any(|entry| entry == version) {
        return Ok(());
    }

    versions.insert(0, version.to_string());

    let rendered = format!(
        "[{}]\n",
        versions
            .iter()
            .map(|entry| format!("\"{entry}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );

    std::fs::write(&path, rendered)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A documentation tree small enough to assert about.
    fn tree(root: &Path) {
        let docs = root.join("docs");
        std::fs::create_dir_all(docs.join("guide")).expect("creates");
        std::fs::create_dir_all(docs.join("reference")).expect("creates");
        std::fs::create_dir_all(docs.join("tutorials")).expect("creates");
        std::fs::create_dir_all(docs.join("ja/guide")).expect("creates");
        std::fs::create_dir_all(docs.join("ja/reference")).expect("creates");
        std::fs::create_dir_all(docs.join("ja/tutorials")).expect("creates");
        std::fs::create_dir_all(docs.join(".vitepress")).expect("creates");

        std::fs::write(
            docs.join("index.md"),
            "hero:\n  actions:\n    - link: /guide/getting-started\n",
        )
        .expect("writes");
        std::fs::write(
            docs.join("guide/index.md"),
            "see [install](./installation) and [cli](../reference/cli)\n",
        )
        .expect("writes");
        std::fs::write(docs.join("reference/cli.md"), "# cli\n").expect("writes");
        std::fs::write(docs.join("tutorials/first.md"), "# first\n").expect("writes");
        std::fs::write(docs.join("ja/index.md"), "hero:\n    - link: /ja/guide/\n")
            .expect("writes");
        std::fs::write(docs.join("ja/guide/index.md"), "# ガイド\n").expect("writes");
        std::fs::write(docs.join("ja/reference/cli.md"), "# cli\n").expect("writes");
        std::fs::write(docs.join("ja/tutorials/first.md"), "# first\n").expect("writes");
        std::fs::write(docs.join(".vitepress/versions.json"), "[]\n").expect("writes");
    }

    #[test]
    fn copies_both_locales() {
        let dir = tempfile::tempdir().expect("tempdir");
        tree(dir.path());

        snapshot(dir.path(), "0.1").expect("snapshots");

        let v = dir.path().join("docs/v0.1");
        assert!(v.join("guide/index.md").is_file());
        assert!(v.join("reference/cli.md").is_file());
        assert!(v.join("ja/guide/index.md").is_file(), "Japanese too");
    }

    #[test]
    fn leaves_the_current_docs_alone() {
        // The root is what you go on editing. A snapshot that moved things
        // would make the next edit land in history.
        let dir = tempfile::tempdir().expect("tempdir");
        tree(dir.path());

        snapshot(dir.path(), "0.1").expect("snapshots");

        let current = std::fs::read_to_string(dir.path().join("docs/index.md")).expect("reads");
        assert!(
            current.contains("link: /guide/getting-started"),
            "still points at the current docs: {current}"
        );
    }

    #[test]
    fn points_absolute_links_inside_the_snapshot() {
        // Otherwise a reader of v0.1 clicking the home page lands in the
        // current docs, which is what versioning exists to prevent.
        let dir = tempfile::tempdir().expect("tempdir");
        tree(dir.path());

        snapshot(dir.path(), "0.1").expect("snapshots");

        let home = std::fs::read_to_string(dir.path().join("docs/v0.1/index.md")).expect("reads");
        assert!(
            home.contains("link: /v0.1/guide/getting-started"),
            "got: {home}"
        );

        let ja = std::fs::read_to_string(dir.path().join("docs/v0.1/ja/index.md")).expect("reads");
        assert!(ja.contains("link: /v0.1/ja/guide/"), "got: {ja}");
    }

    #[test]
    fn leaves_relative_links_as_they_are() {
        // They already resolve correctly inside the copy, and rewriting
        // them would break them.
        let dir = tempfile::tempdir().expect("tempdir");
        tree(dir.path());

        snapshot(dir.path(), "0.1").expect("snapshots");

        let page =
            std::fs::read_to_string(dir.path().join("docs/v0.1/guide/index.md")).expect("reads");
        assert!(page.contains("(./installation)"), "got: {page}");
        assert!(page.contains("(../reference/cli)"), "got: {page}");
    }

    #[test]
    fn registers_the_version_newest_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        tree(dir.path());

        snapshot(dir.path(), "0.1").expect("snapshots");
        snapshot(dir.path(), "0.2").expect("snapshots");

        let versions = std::fs::read_to_string(dir.path().join("docs/.vitepress/versions.json"))
            .expect("reads");
        assert_eq!(versions.trim(), r#"["0.2", "0.1"]"#);
    }

    #[test]
    fn refuses_to_overwrite_a_snapshot() {
        // Released documentation is history. Silently replacing it would
        // rewrite what someone on that version is reading.
        let dir = tempfile::tempdir().expect("tempdir");
        tree(dir.path());

        snapshot(dir.path(), "0.1").expect("snapshots");
        let err = snapshot(dir.path(), "0.1").unwrap_err();

        assert!(
            err.to_string().contains("already been snapshotted"),
            "got: {err}"
        );
    }

    #[test]
    fn accepts_a_leading_v() {
        let dir = tempfile::tempdir().expect("tempdir");
        tree(dir.path());

        snapshot(dir.path(), "v0.1").expect("snapshots");
        assert!(dir.path().join("docs/v0.1").is_dir(), "not docs/vv0.1");
    }

    #[test]
    fn rejects_a_version_that_would_not_make_a_url() {
        let dir = tempfile::tempdir().expect("tempdir");
        tree(dir.path());

        for bad in ["", "../etc", "next", "0.1 beta"] {
            assert!(snapshot(dir.path(), bad).is_err(), "accepted `{bad}`");
        }
    }
}

//! `minato skill install` — installs the Skill for agents.
//!
//! What it contains is **judgement**, not a CLI reference. `--help` covers
//! what the commands do; promises like "never reach for `docker`" and
//! "never guess a port" only land if they are written down.

use std::path::{Path, PathBuf};

/// The Skill itself, baked into the binary.
///
/// Embedded so no file has to ship alongside. A self-contained `minato`
/// works however it was installed.
const SKILL: &str = include_str!("../../../skills/minato/SKILL.md");

/// Where Claude Code looks for Skills.
const SKILL_DIR: &str = ".claude/skills/minato";

const SKILL_FILE: &str = "SKILL.md";

#[derive(Debug)]
pub struct Installed {
    pub path: PathBuf,
    pub overwritten: bool,
}

/// Writes the Skill into a repository.
pub fn install(root: &Path, force: bool) -> anyhow::Result<Installed> {
    let dir = root.join(SKILL_DIR);
    let path = dir.join(SKILL_FILE);

    let existing = std::fs::read_to_string(&path).ok();

    if let Some(existing) = &existing {
        if existing == SKILL {
            // Identical content is left alone, so git stays clean.
            return Ok(Installed {
                path,
                overwritten: false,
            });
        }

        if !force {
            anyhow::bail!(
                "{} already exists with different content. Pass --force to overwrite it",
                path.display()
            );
        }
    }

    std::fs::create_dir_all(&dir)?;
    std::fs::write(&path, SKILL)?;

    Ok(Installed {
        path,
        overwritten: existing.is_some(),
    })
}

/// The embedded Skill.
pub fn contents() -> &'static str {
    SKILL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_has_the_frontmatter_claude_code_needs() {
        // Without name and description it is not recognised as a Skill.
        assert!(
            SKILL.starts_with("---\n"),
            "it has to open with frontmatter"
        );
        assert!(SKILL.contains("\nname: minato\n"));
        assert!(SKILL.contains("\ndescription: "));

        let end = SKILL[4..].find("\n---\n").expect("the frontmatter closes");
        let frontmatter = &SKILL[4..4 + end];
        assert!(
            frontmatter.lines().count() <= 5,
            "keep the frontmatter short: {frontmatter}"
        );
    }

    #[test]
    fn description_says_when_to_use_it() {
        // The description alone decides whether it gets reached for.
        let description = SKILL
            .lines()
            .find(|line| line.starts_with("description: "))
            .expect("is there");

        assert!(description.contains("worktree"), "got: {description}");
        assert!(
            description.len() > 60,
            "it has to be long enough to say when to use it: {description}"
        );
    }

    #[test]
    fn states_the_rules_that_matter() {
        // Leave these out and an agent falls back to docker.
        for rule in ["docker", "minato url", "minato logs", "minato doctor"] {
            assert!(SKILL.contains(rule), "`{rule}` goes unmentioned");
        }
    }

    #[test]
    fn documents_the_exit_codes() {
        // Branching on the exit code is the whole point for an agent.
        assert!(SKILL.contains("exit code"));
        assert!(SKILL.contains("--json"));
    }

    #[test]
    fn installs_into_the_conventional_location() {
        let dir = tempfile::tempdir().expect("tempdir");

        let installed = install(dir.path(), false).expect("installs");

        assert_eq!(
            installed.path,
            dir.path().join(".claude/skills/minato/SKILL.md")
        );
        assert!(!installed.overwritten);
        assert_eq!(
            std::fs::read_to_string(&installed.path).expect("reads"),
            SKILL
        );
    }

    #[test]
    fn reinstalling_the_same_content_is_a_no_op() {
        // No diff. Rewriting every time would dirty the repository.
        let dir = tempfile::tempdir().expect("tempdir");

        install(dir.path(), false).expect("first time");
        let second = install(dir.path(), false).expect("second time too");

        assert!(!second.overwritten);
    }

    #[test]
    fn refuses_to_clobber_local_edits() {
        let dir = tempfile::tempdir().expect("tempdir");
        install(dir.path(), false).expect("installs");

        let path = dir.path().join(".claude/skills/minato/SKILL.md");
        std::fs::write(&path, "edited by hand").expect("writes");

        let err = install(dir.path(), false).unwrap_err();
        assert!(err.to_string().contains("--force"), "got: {err}");

        let forced = install(dir.path(), true).expect("--force overwrites");
        assert!(forced.overwritten);
    }
}

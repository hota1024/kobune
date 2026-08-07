//! `minato skill install` — エージェント向けの Skill を配置する。
//!
//! 内容は CLI のリファレンスではなく**判断基準**。「何ができるか」は
//! `--help` を読めば分かるが、「`docker` を直接使わない」「ポートを
//! 推測しない」といった約束は書いておかないと伝わらない。

use std::path::{Path, PathBuf};

/// バイナリに埋め込む Skill 本体。
///
/// ファイルを配布物に含めなくて済むよう埋め込む。`minato` 単体で
/// 完結する方が、インストール手段を選ばない。
const SKILL: &str = include_str!("../../../skills/minato/SKILL.md");

/// Claude Code が Skill を探す場所。
const SKILL_DIR: &str = ".claude/skills/minato";

const SKILL_FILE: &str = "SKILL.md";

#[derive(Debug)]
pub struct Installed {
    pub path: PathBuf,
    pub overwritten: bool,
}

/// リポジトリに Skill を書き出す。
pub fn install(root: &Path, force: bool) -> anyhow::Result<Installed> {
    let dir = root.join(SKILL_DIR);
    let path = dir.join(SKILL_FILE);

    let existing = std::fs::read_to_string(&path).ok();

    if let Some(existing) = &existing {
        if existing == SKILL {
            // 同じ内容なら書き直さない。git の差分を汚さない。
            return Ok(Installed {
                path,
                overwritten: false,
            });
        }

        if !force {
            anyhow::bail!(
                "{} は既に存在し、内容が異なります。上書きするには --force を付けてください",
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

/// 埋め込んだ Skill の内容。
pub fn contents() -> &'static str {
    SKILL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_has_the_frontmatter_claude_code_needs() {
        // name と description が無いと Skill として認識されない。
        assert!(SKILL.starts_with("---\n"), "frontmatter で始まる必要がある");
        assert!(SKILL.contains("\nname: minato\n"));
        assert!(SKILL.contains("\ndescription: "));

        let end = SKILL[4..]
            .find("\n---\n")
            .expect("frontmatter が閉じている");
        let frontmatter = &SKILL[4..4 + end];
        assert!(
            frontmatter.lines().count() <= 5,
            "frontmatter は短く保つ: {frontmatter}"
        );
    }

    #[test]
    fn description_says_when_to_use_it() {
        // description だけを見て呼ぶかどうかを判断される。
        let description = SKILL
            .lines()
            .find(|line| line.starts_with("description: "))
            .expect("ある");

        assert!(description.contains("worktree"), "got: {description}");
        assert!(
            description.len() > 60,
            "いつ使うかが分かる長さが要る: {description}"
        );
    }

    #[test]
    fn states_the_rules_that_matter() {
        // これらが抜けると、エージェントは docker に戻ってしまう。
        for rule in ["docker", "minato url", "minato logs", "minato doctor"] {
            assert!(SKILL.contains(rule), "`{rule}` に触れていない");
        }
    }

    #[test]
    fn documents_the_exit_codes() {
        // 終了コードで分岐できることが、エージェント向けの肝。
        assert!(SKILL.contains("終了コード"));
        assert!(SKILL.contains("--json"));
    }

    #[test]
    fn installs_into_the_conventional_location() {
        let dir = tempfile::tempdir().expect("tempdir");

        let installed = install(dir.path(), false).expect("配置できる");

        assert_eq!(
            installed.path,
            dir.path().join(".claude/skills/minato/SKILL.md")
        );
        assert!(!installed.overwritten);
        assert_eq!(
            std::fs::read_to_string(&installed.path).expect("読める"),
            SKILL
        );
    }

    #[test]
    fn reinstalling_the_same_content_is_a_no_op() {
        // 差分が出ないようにする。毎回書き換えると git が汚れる。
        let dir = tempfile::tempdir().expect("tempdir");

        install(dir.path(), false).expect("1 回目");
        let second = install(dir.path(), false).expect("2 回目も通る");

        assert!(!second.overwritten);
    }

    #[test]
    fn refuses_to_clobber_local_edits() {
        let dir = tempfile::tempdir().expect("tempdir");
        install(dir.path(), false).expect("配置できる");

        let path = dir.path().join(".claude/skills/minato/SKILL.md");
        std::fs::write(&path, "手で書き換えた内容").expect("書ける");

        let err = install(dir.path(), false).unwrap_err();
        assert!(err.to_string().contains("--force"), "got: {err}");

        let forced = install(dir.path(), true).expect("force なら上書きできる");
        assert!(forced.overwritten);
    }
}

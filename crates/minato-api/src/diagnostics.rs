//! `minato doctor` の診断結果。
//!
//! 「動いていない」だけでは何もできない。**必ず直し方を添える**。
//! 人間はそれを実行し、エージェントは `fix` を読んで次の行動を決められる。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostics {
    pub checks: Vec<Check>,
}

impl Diagnostics {
    pub fn new(checks: Vec<Check>) -> Self {
        Self { checks }
    }

    /// 使用に支障がある項目があるか。
    pub fn has_failures(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == CheckStatus::Fail)
    }

    pub fn has_warnings(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == CheckStatus::Warn)
    }

    /// 直し方が判明している項目。
    pub fn fixes(&self) -> Vec<&Check> {
        self.checks
            .iter()
            .filter(|check| check.fix.is_some() && check.status != CheckStatus::Ok)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// 安定した識別子。エージェントはこれで分岐する。
    pub id: String,
    /// 人間向けの項目名。
    pub title: String,
    pub status: CheckStatus,
    /// 何が分かったか。
    pub detail: String,
    /// 直すために実行するコマンド。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Check {
    pub fn ok(id: impl Into<String>, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            status: CheckStatus::Ok,
            detail: detail.into(),
            fix: None,
        }
    }

    pub fn warn(
        id: impl Into<String>,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            status: CheckStatus::Warn,
            detail: detail.into(),
            fix: None,
        }
    }

    pub fn fail(
        id: impl Into<String>,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            status: CheckStatus::Fail,
            detail: detail.into(),
            fix: None,
        }
    }

    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// 問題なし。
    Ok,
    /// 動くが、一部の機能が使えない。
    Warn,
    /// このままでは使えない。
    Fail,
}

impl CheckStatus {
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Ok => "✓",
            Self::Warn => "!",
            Self::Fail => "✗",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarises_severity() {
        let diagnostics = Diagnostics::new(vec![
            Check::ok("a", "A", "問題なし"),
            Check::warn("b", "B", "一部使えない"),
        ]);

        assert!(!diagnostics.has_failures());
        assert!(diagnostics.has_warnings());
    }

    #[test]
    fn collects_only_actionable_fixes() {
        let diagnostics = Diagnostics::new(vec![
            // 直っているものの fix は出さない。
            Check::ok("a", "A", "問題なし").with_fix("何もしなくてよい"),
            Check::fail("b", "B", "壊れている").with_fix("sudo minato setup"),
            Check::fail("c", "C", "直し方が分からない"),
        ]);

        let fixes = diagnostics.fixes();
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].id, "b");
    }

    #[test]
    fn roundtrips_on_the_wire() {
        let diagnostics = Diagnostics::new(vec![
            Check::ok("runtime", "Docker", "29.4.0"),
            Check::fail("resolver", "DNS resolver", "未設置").with_fix("sudo minato setup"),
        ]);

        let json = serde_json::to_string(&diagnostics).expect("serializes");
        let back: Diagnostics = serde_json::from_str(&json).expect("deserializes");

        assert_eq!(back, diagnostics);
    }

    #[test]
    fn omits_absent_fixes_on_the_wire() {
        let json = serde_json::to_string(&Check::ok("a", "A", "問題なし")).expect("serializes");
        assert!(!json.contains("fix"));
    }
}

//! The results of `minato doctor`.
//!
//! "It is broken" helps nobody. **Always attach the fix.** A human runs it;
//! an agent reads `fix` and decides what to do next.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostics {
    pub checks: Vec<Check>,
}

impl Diagnostics {
    pub fn new(checks: Vec<Check>) -> Self {
        Self { checks }
    }

    /// Whether anything blocks normal use.
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

    /// The checks with a known fix.
    pub fn fixes(&self) -> Vec<&Check> {
        self.checks
            .iter()
            .filter(|check| check.fix.is_some() && check.status != CheckStatus::Ok)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// A stable identifier. Agents branch on this.
    pub id: String,
    /// The human-facing name of the check.
    pub title: String,
    pub status: CheckStatus,
    /// What was found.
    pub detail: String,
    /// The command that fixes it.
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
    /// Nothing wrong.
    Ok,
    /// Usable, but some features are unavailable.
    Warn,
    /// Unusable as things stand.
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
            Check::ok("a", "A", "fine"),
            Check::warn("b", "B", "partly unavailable"),
        ]);

        assert!(!diagnostics.has_failures());
        assert!(diagnostics.has_warnings());
    }

    #[test]
    fn collects_only_actionable_fixes() {
        let diagnostics = Diagnostics::new(vec![
            // A passing check offers no fix.
            Check::ok("a", "A", "fine").with_fix("nothing to do"),
            Check::fail("b", "B", "broken").with_fix("sudo minato setup"),
            Check::fail("c", "C", "no known fix"),
        ]);

        let fixes = diagnostics.fixes();
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].id, "b");
    }

    #[test]
    fn roundtrips_on_the_wire() {
        let diagnostics = Diagnostics::new(vec![
            Check::ok("runtime", "Docker", "29.4.0"),
            Check::fail("resolver", "DNS resolver", "not installed").with_fix("sudo minato setup"),
        ]);

        let json = serde_json::to_string(&diagnostics).expect("serializes");
        let back: Diagnostics = serde_json::from_str(&json).expect("deserializes");

        assert_eq!(back, diagnostics);
    }

    #[test]
    fn omits_absent_fixes_on_the_wire() {
        let json = serde_json::to_string(&Check::ok("a", "A", "fine")).expect("serializes");
        assert!(!json.contains("fix"));
    }
}

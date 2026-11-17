//! `org/repeated-logic`: Same logic in three or more recipes; suggest a bbclass or `.inc`.
//!
//! See docs/lint-rules.md. Stub (M0): T4.3 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OrgRepeatedLogic;

impl Rule for OrgRepeatedLogic {
    fn id(&self) -> &'static str {
        "org/repeated-logic"
    }

    fn scope(&self) -> Scope {
        Scope::Layer
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

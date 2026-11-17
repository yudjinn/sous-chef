//! `org/append-no-match`: bbappend that matches no recipe.
//!
//! See docs/lint-rules.md. Stub (M0): T4.3 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OrgAppendNoMatch;

impl Rule for OrgAppendNoMatch {
    fn id(&self) -> &'static str {
        "org/append-no-match"
    }

    fn scope(&self) -> Scope {
        Scope::Layer
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

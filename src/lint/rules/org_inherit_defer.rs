//! `org/inherit-defer`: `inherit ${VAR}` where the variable may change later; suggest `inherit_defer`.
//!
//! See docs/lint-rules.md. Stub (M0): T4.3 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OrgInheritDefer;

impl Rule for OrgInheritDefer {
    fn id(&self) -> &'static str {
        "org/inherit-defer"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

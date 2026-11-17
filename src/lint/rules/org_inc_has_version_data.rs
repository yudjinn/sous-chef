//! `org/inc-has-version-data`: Shared `.inc` sets version-specific data.
//!
//! See docs/lint-rules.md. Stub (M0): T4.3 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OrgIncHasVersionData;

impl Rule for OrgIncHasVersionData {
    fn id(&self) -> &'static str {
        "org/inc-has-version-data"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

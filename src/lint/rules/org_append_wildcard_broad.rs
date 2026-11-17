//! `org/append-wildcard-broad`: `<pn>_%.bbappend` pinning version-specific data.
//!
//! See docs/lint-rules.md. Stub (M0): T4.3 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OrgAppendWildcardBroad;

impl Rule for OrgAppendWildcardBroad {
    fn id(&self) -> &'static str {
        "org/append-wildcard-broad"
    }

    fn scope(&self) -> Scope {
        Scope::Layer
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

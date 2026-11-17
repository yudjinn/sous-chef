//! `org/duplicate-versions`: Versions of a recipe with mostly identical bodies; suggest a shared `.inc`.
//!
//! See docs/lint-rules.md. Stub (M0): T4.3 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OrgDuplicateVersions;

impl Rule for OrgDuplicateVersions {
    fn id(&self) -> &'static str {
        "org/duplicate-versions"
    }

    fn scope(&self) -> Scope {
        Scope::Layer
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

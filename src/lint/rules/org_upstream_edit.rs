//! `org/upstream-edit`: Uncommitted modifications in a layer not marked writable.
//!
//! See docs/lint-rules.md. Stub (M0): T4.3 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OrgUpstreamEdit;

impl Rule for OrgUpstreamEdit {
    fn id(&self) -> &'static str {
        "org/upstream-edit"
    }

    fn scope(&self) -> Scope {
        Scope::Layer
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

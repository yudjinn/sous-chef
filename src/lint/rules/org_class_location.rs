//! `org/class-location`: Class in `classes/` in a layer whose compat is 4.1 or later.
//!
//! See docs/lint-rules.md. Stub (M0): T4.3 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OrgClassLocation;

impl Rule for OrgClassLocation {
    fn id(&self) -> &'static str {
        "org/class-location"
    }

    fn scope(&self) -> Scope {
        Scope::Layer
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

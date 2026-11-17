//! `override/append-vs-plus-equals`: `:append` in a `.bb` where `+=` would do.
//!
//! See docs/lint-rules.md. Stub (M0): T4.2 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OverrideAppendVsPlusEquals;

impl Rule for OverrideAppendVsPlusEquals {
    fn id(&self) -> &'static str {
        "override/append-vs-plus-equals"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

//! `override/append-no-space`: `:append` without a leading space or `:prepend` without a trailing space on a list variable.
//!
//! See docs/lint-rules.md. Stub (M0): T4.2 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OverrideAppendNoSpace;

impl Rule for OverrideAppendNoSpace {
    fn id(&self) -> &'static str {
        "override/append-no-space"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

//! `override/weak-after-default`: `??=` after `?=` or `=` for the same variable in the same file.
//!
//! See docs/lint-rules.md. Stub (M0): T4.2 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OverrideWeakAfterDefault;

impl Rule for OverrideWeakAfterDefault {
    fn id(&self) -> &'static str {
        "override/weak-after-default"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

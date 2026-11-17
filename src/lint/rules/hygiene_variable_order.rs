//! `hygiene/variable-order`: Order differs from the OE style guide (disabled by default).
//!
//! See docs/lint-rules.md. Stub (M0): T4.4 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct HygieneVariableOrder;

impl Rule for HygieneVariableOrder {
    fn id(&self) -> &'static str {
        "hygiene/variable-order"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

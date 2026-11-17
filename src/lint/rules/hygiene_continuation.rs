//! `hygiene/continuation`: Comment or blank line inside a continued assignment; whitespace after `\`.
//!
//! See docs/lint-rules.md. Stub (M0): T4.4 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct HygieneContinuation;

impl Rule for HygieneContinuation {
    fn id(&self) -> &'static str {
        "hygiene/continuation"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

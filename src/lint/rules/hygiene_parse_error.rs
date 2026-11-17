//! `hygiene/parse-error`: Any `Unknown` statement or parser diagnostic.
//!
//! See docs/lint-rules.md. Stub (M0): T4.1 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct HygieneParseError;

impl Rule for HygieneParseError {
    fn id(&self) -> &'static str {
        "hygiene/parse-error"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

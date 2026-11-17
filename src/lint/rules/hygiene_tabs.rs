//! `hygiene/tabs`: Mixed tabs and spaces in shell bodies; python bodies not indented with 4 spaces.
//!
//! See docs/lint-rules.md. Stub (M0): T4.4 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct HygieneTabs;

impl Rule for HygieneTabs {
    fn id(&self) -> &'static str {
        "hygiene/tabs"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

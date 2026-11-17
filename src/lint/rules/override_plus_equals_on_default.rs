//! `override/plus-equals-on-default`: `VAR +=` in a conf file on a variable defaulted with `?=` elsewhere.
//!
//! See docs/lint-rules.md. Stub (M0): T4.2 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OverridePlusEqualsOnDefault;

impl Rule for OverridePlusEqualsOnDefault {
    fn id(&self) -> &'static str {
        "override/plus-equals-on-default"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

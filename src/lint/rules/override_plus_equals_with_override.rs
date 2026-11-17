//! `override/plus-equals-with-override`: `VAR:foo += "x"` modifies `VAR:foo`, not `VAR`; suggest `VAR:append:foo`.
//!
//! See docs/lint-rules.md. Stub (M0): T4.2 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OverridePlusEqualsWithOverride;

impl Rule for OverridePlusEqualsWithOverride {
    fn id(&self) -> &'static str {
        "override/plus-equals-with-override"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

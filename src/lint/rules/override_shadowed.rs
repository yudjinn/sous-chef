//! `override/shadowed`: Plain assignment always overwritten by a later unconditional `=`.
//!
//! See docs/lint-rules.md. Stub (M0): T4.2 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OverrideShadowed;

impl Rule for OverrideShadowed {
    fn id(&self) -> &'static str {
        "override/shadowed"
    }

    fn scope(&self) -> Scope {
        Scope::Recipe
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

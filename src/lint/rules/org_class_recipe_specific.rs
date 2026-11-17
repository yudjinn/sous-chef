//! `org/class-recipe-specific`: bbclass sets recipe-specific values or uses `=` where `?=` fits.
//!
//! See docs/lint-rules.md. Stub (M0): T4.3 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OrgClassRecipeSpecific;

impl Rule for OrgClassRecipeSpecific {
    fn id(&self) -> &'static str {
        "org/class-recipe-specific"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

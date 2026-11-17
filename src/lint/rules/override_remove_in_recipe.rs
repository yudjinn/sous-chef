//! `override/remove-in-recipe`: `:remove` in a `.bb`, `.inc` or `.bbclass` cannot be undone downstream.
//!
//! See docs/lint-rules.md. Stub (M0): T4.2 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OverrideRemoveInRecipe;

impl Rule for OverrideRemoveInRecipe {
    fn id(&self) -> &'static str {
        "override/remove-in-recipe"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

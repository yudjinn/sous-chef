//! `override/dead`: Override that matches no known machine, distro, class, package or `OVERRIDES` entry.
//!
//! See docs/lint-rules.md. Stub (M0): T4.2 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OverrideDead;

impl Rule for OverrideDead {
    fn id(&self) -> &'static str {
        "override/dead"
    }

    fn scope(&self) -> Scope {
        Scope::Recipe
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

//! `org/policy-in-recipe`: Recipe or bbappend sets distro or machine policy.
//!
//! See docs/lint-rules.md. Stub (M0): T4.3 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OrgPolicyInRecipe;

impl Rule for OrgPolicyInRecipe {
    fn id(&self) -> &'static str {
        "org/policy-in-recipe"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

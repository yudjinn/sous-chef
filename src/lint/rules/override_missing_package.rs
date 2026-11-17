//! `override/missing-package`: Package-scoped variable assigned without a package override.
//!
//! See docs/lint-rules.md. Stub (M0): T4.2 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OverrideMissingPackage;

impl Rule for OverrideMissingPackage {
    fn id(&self) -> &'static str {
        "override/missing-package"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

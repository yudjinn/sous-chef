//! `override/legacy-syntax`: Underscore override syntax (`VAR_append`, `RDEPENDS_${PN}`).
//!
//! See docs/lint-rules.md. Stub (M0): T4.2 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OverrideLegacySyntax;

impl Rule for OverrideLegacySyntax {
    fn id(&self) -> &'static str {
        "override/legacy-syntax"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

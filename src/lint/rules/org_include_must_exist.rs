//! `org/include-must-exist`: `include` of an essential file (use `require`), or `require` that does not resolve.
//!
//! See docs/lint-rules.md. Stub (M0): T4.3 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OrgIncludeMustExist;

impl Rule for OrgIncludeMustExist {
    fn id(&self) -> &'static str {
        "org/include-must-exist"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

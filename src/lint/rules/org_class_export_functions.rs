//! `org/class-export-functions`: bbclass defines `<class>_do_<task>` without `EXPORT_FUNCTIONS`, or `do_<task>` directly.
//!
//! See docs/lint-rules.md. Stub (M0): T4.3 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OrgClassExportFunctions;

impl Rule for OrgClassExportFunctions {
    fn id(&self) -> &'static str {
        "org/class-export-functions"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

//! `org/append-missing-filesextrapaths`: bbappend adds `file://` to `SRC_URI` without extending `FILESEXTRAPATHS`.
//!
//! See docs/lint-rules.md. Stub (M0): T4.3 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OrgAppendMissingFilesextrapaths;

impl Rule for OrgAppendMissingFilesextrapaths {
    fn id(&self) -> &'static str {
        "org/append-missing-filesextrapaths"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

//! `override/filesextrapaths`: `FILESEXTRAPATHS` modified without `:prepend`/`:append`, `:=` or the separator colon.
//!
//! See docs/lint-rules.md. Stub (M0): T4.2 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OverrideFilesextrapaths;

impl Rule for OverrideFilesextrapaths {
    fn id(&self) -> &'static str {
        "override/filesextrapaths"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

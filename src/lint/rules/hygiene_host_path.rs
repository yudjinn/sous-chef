//! `hygiene/host-path`: Absolute host paths in task bodies; `do_install` without `${D}`.
//!
//! See docs/lint-rules.md. Stub (M0): T4.4 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct HygieneHostPath;

impl Rule for HygieneHostPath {
    fn id(&self) -> &'static str {
        "hygiene/host-path"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

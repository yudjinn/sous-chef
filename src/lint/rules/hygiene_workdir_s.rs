//! `hygiene/workdir-s`: `S = "${WORKDIR}"`.
//!
//! See docs/lint-rules.md. Stub (M0): T4.4 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct HygieneWorkdirS;

impl Rule for HygieneWorkdirS {
    fn id(&self) -> &'static str {
        "hygiene/workdir-s"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

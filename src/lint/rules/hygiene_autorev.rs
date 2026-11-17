//! `hygiene/autorev`: `SRCREV = "${AUTOREV}"` outside `local.conf`.
//!
//! See docs/lint-rules.md. Stub (M0): T4.4 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct HygieneAutorev;

impl Rule for HygieneAutorev {
    fn id(&self) -> &'static str {
        "hygiene/autorev"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

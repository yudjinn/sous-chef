//! `hygiene/missing-metadata`: No `SUMMARY`, `LICENSE` or `LIC_FILES_CHKSUM`.
//!
//! See docs/lint-rules.md. Stub (M0): T4.4 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct HygieneMissingMetadata;

impl Rule for HygieneMissingMetadata {
    fn id(&self) -> &'static str {
        "hygiene/missing-metadata"
    }

    fn scope(&self) -> Scope {
        Scope::Recipe
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

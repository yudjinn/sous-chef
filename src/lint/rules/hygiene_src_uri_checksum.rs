//! `hygiene/src-uri-checksum`: Remote tarball without `sha256sum`; git URL without `SRCREV` or `branch`.
//!
//! See docs/lint-rules.md. Stub (M0): T4.4 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct HygieneSrcUriChecksum;

impl Rule for HygieneSrcUriChecksum {
    fn id(&self) -> &'static str {
        "hygiene/src-uri-checksum"
    }

    fn scope(&self) -> Scope {
        Scope::Recipe
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

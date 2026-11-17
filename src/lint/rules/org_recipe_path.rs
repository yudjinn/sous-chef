//! `org/recipe-path`: Recipe not under `recipes-<category>/<pn>/` or badly named.
//!
//! See docs/lint-rules.md. Stub (M0): T4.3 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OrgRecipePath;

impl Rule for OrgRecipePath {
    fn id(&self) -> &'static str {
        "org/recipe-path"
    }

    fn scope(&self) -> Scope {
        Scope::Layer
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

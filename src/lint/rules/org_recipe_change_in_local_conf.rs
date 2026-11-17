//! `org/recipe-change-in-local-conf`: `local.conf` carries `:pn-<recipe>` changes.
//!
//! See docs/lint-rules.md. Stub (M0): T4.3 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OrgRecipeChangeInLocalConf;

impl Rule for OrgRecipeChangeInLocalConf {
    fn id(&self) -> &'static str {
        "org/recipe-change-in-local-conf"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

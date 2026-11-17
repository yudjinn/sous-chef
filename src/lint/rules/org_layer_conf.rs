//! `org/layer-conf`: Problems in `conf/layer.conf` (severity E or W per check).
//!
//! See docs/lint-rules.md. Stub (M0): T4.3 implements `check`.

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};

pub struct OrgLayerConf;

impl Rule for OrgLayerConf {
    fn id(&self) -> &'static str {
        "org/layer-conf"
    }

    fn scope(&self) -> Scope {
        Scope::Layer
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, _cx: &LintContext<'_>, _out: &mut Vec<Finding>) {}
}

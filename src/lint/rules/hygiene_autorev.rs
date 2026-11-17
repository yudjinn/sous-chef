//! `hygiene/autorev`: `SRCREV = "${AUTOREV}"` outside a `local.conf`.
//!
//! See docs/lint-rules.md. A source revision that floats with the branch head makes the
//! build unreproducible and makes BitBake query the remote on every parse. Developers set
//! it in their own `local.conf` (or `auto.conf`, which is the same kind of per-build file);
//! anywhere else in a layer it is reported.
//!
//! Matched: an assignment (any operator, any override, not a flag) to `SRCREV` or to
//! `SRCREV_<name>` (the per-URL form for `name=` in `SRC_URI`; `SRCREV_FORMAT` is not a
//! revision) whose raw value contains `${AUTOREV}`.

use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::LineSpan;

pub struct HygieneAutorev;

/// Per-build configuration files where a floating revision is the developer's own choice.
const LOCAL_FILES: &[&str] = &["local.conf", "auto.conf"];

fn is_srcrev(base: &str) -> bool {
    base == "SRCREV" || (base.starts_with("SRCREV_") && base != "SRCREV_FORMAT")
}

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

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(file) = cx.file else { return };
        let name = cx
            .loc
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if LOCAL_FILES.contains(&name) {
            return;
        }
        for s in &file.statements {
            let StatementKind::Assign { var, value, .. } = &s.kind else {
                continue;
            };
            if var.flag.is_some() || !is_srcrev(&var.base) || !value.contains("${AUTOREV}") {
                continue;
            }
            out.push(helpers::finding(
                self,
                cx,
                LineSpan::line(s.span.start),
                format!(
                    "`{}` follows the branch head (`${{AUTOREV}}`), so builds are not \
                     reproducible; pin a commit hash here and set AUTOREV only in a local.conf",
                    var.base
                ),
            ));
        }
    }
}

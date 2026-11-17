//! `org/append-no-match`: A bbappend that matches no recipe in the workspace.
//!
//! See docs/lint-rules.md. The layer's files among `query::orphan_appends()` (bbappends
//! collected by `BBFILES` whose `bbappend` edge has no target; an append that `BBFILES` does
//! not collect has no edge and is `org/layer-conf`'s business). BitBake stops on such an
//! append (`cooker.py` `show_appends_with_no_recipes`: `bb.fatal` on master; on 2.0 and 2.8
//! only a warning when `BB_DANGLINGAPPENDS_WARNONLY` is set). When recipes of the same `PN`
//! exist, the message lists their file names.

use crate::index::query;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::types::LineSpan;

pub struct OrgAppendNoMatch;

impl Rule for OrgAppendNoMatch {
    fn id(&self) -> &'static str {
        "org/append-no-match"
    }

    fn scope(&self) -> Scope {
        Scope::Layer
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(layer) = cx.layer else { return };
        let Ok(orphans) = query::orphan_appends(cx.index) else {
            return;
        };
        for f in orphans.iter().filter(|f| f.layer == layer) {
            let name = f
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let hint = match f.pn.as_deref() {
                Some(pn) => {
                    let mut names: Vec<String> = query::find_recipes(cx.index, pn)
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|r| r.file.path.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .collect();
                    names.sort();
                    names.dedup();
                    if names.is_empty() {
                        format!("there is no {pn} recipe; remove the append or add its layer")
                    } else {
                        format!(
                            "{pn} exists as {}; rename the append to match one of them, or to \
                             {pn}_%.bbappend for every version",
                            names.join(", ")
                        )
                    }
                }
                None => "remove it or rename it after an existing recipe".to_string(),
            };
            out.push(Finding {
                rule: self.id(),
                severity: self.default_severity(),
                loc: f.loc(LineSpan::line(1)),
                message: format!(
                    "{name} matches no recipe in the workspace, which BitBake treats as an \
                     error (\"No recipes available for\"); {hint}"
                ),
            });
        }
    }
}

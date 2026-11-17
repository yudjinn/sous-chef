//! `hygiene/workdir-s`: `S = "${WORKDIR}"`, which Styhead and later reject.
//!
//! See docs/lint-rules.md and "Releases" in docs/bitbake-semantics.md
//! (`Feature::WorkdirAsSourceDir`, accepted up to Scarthgap; the 5.1 migration guide: "If a
//! recipe has S set to be WORKDIR, this is no longer supported, and an error will be
//! issued."). Reported, as an error, only for a file checked against a release that lacks
//! the feature, naming those releases; a layer that claims only releases that accept it gets
//! nothing. The rule therefore keeps the default release range (every release) and decides
//! per finding, rather than declaring the feature's own range, which is exactly the releases
//! where there is nothing to report.
//!
//! Matched: an assignment to `S` (no flag; any operator except `+=`-style and
//! `:append` / `:prepend` / `:remove`, with or without other overrides) whose value is
//! `${WORKDIR}` or `${WORKDIR}/`.

use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::release::Feature;
use crate::types::{AssignOp, LineSpan};

pub struct HygieneWorkdirS;

impl Rule for HygieneWorkdirS {
    fn id(&self) -> &'static str {
        "hygiene/workdir-s"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(file) = cx.file else { return };
        let rejecting = helpers::releases_lacking(cx, Feature::WorkdirAsSourceDir);
        if rejecting.is_empty() {
            return;
        }
        for s in &file.statements {
            let StatementKind::Assign { var, op, value, .. } = &s.kind else {
                continue;
            };
            let sets = matches!(
                op,
                AssignOp::Set | AssignOp::Default | AssignOp::WeakDefault | AssignOp::Immediate
            );
            let keyword = var
                .overrides
                .iter()
                .any(|o| matches!(o.as_str(), "append" | "prepend" | "remove"));
            let v = value.trim();
            if var.base != "S"
                || var.flag.is_some()
                || !sets
                || keyword
                || !(v == "${WORKDIR}" || v == "${WORKDIR}/")
            {
                continue;
            }
            out.push(helpers::finding(
                self,
                cx,
                LineSpan::line(s.span.start),
                format!(
                    "`S = \"${{WORKDIR}}\"` is rejected with an error in {}, which this layer \
                     is checked against; point S at the unpacked sources instead, e.g. \
                     `S = \"${{UNPACKDIR}}\"` (the variable exists from {} on)",
                    helpers::release_list(&rejecting),
                    Feature::UnpackDir.first().codename()
                ),
            ));
        }
    }
}

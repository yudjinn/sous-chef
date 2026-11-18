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
//!
//! **Fix** (since x7), safe: the value becomes `${UNPACKDIR}`, the form the 5.1 guide gives
//! ("The recipe should be changed to: `S = "${UNPACKDIR}"`"), the rule's own correction.
//! Offered only when every release the file is checked against has `UNPACKDIR`
//! (`Feature::UnpackDir`); a layer that also claims an older release, which has no
//! `UNPACKDIR`, gets no fix and the message says so.

use crate::lint::fix::{self, Edit, Fix, FixedFinding, Safety};
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
        out.extend(self.scan(cx, false).into_iter().map(|(f, _)| f));
    }

    fn fixes(&self, cx: &LintContext<'_>) -> Vec<FixedFinding> {
        self.scan(cx, true)
            .into_iter()
            .filter_map(|(finding, fix)| fix.map(|fix| FixedFinding { finding, fix }))
            .collect()
    }
}

impl HygieneWorkdirS {
    fn scan(&self, cx: &LintContext<'_>, want_fix: bool) -> Vec<(Finding, Option<Fix>)> {
        let mut out = Vec::new();
        let Some(file) = cx.file else { return out };
        let rejecting = helpers::releases_lacking(cx, Feature::WorkdirAsSourceDir);
        if rejecting.is_empty() {
            return out;
        }
        let no_unpackdir = helpers::releases_lacking(cx, Feature::UnpackDir);
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
            let by_hand = if no_unpackdir.is_empty() {
                String::new()
            } else {
                format!(
                    "; no automatic fix while the layer also claims {}, which {} no UNPACKDIR",
                    helpers::release_list(&no_unpackdir),
                    if no_unpackdir.len() == 1 {
                        "has"
                    } else {
                        "have"
                    }
                )
            };
            let finding = helpers::finding(
                self,
                cx,
                LineSpan::line(s.span.start),
                format!(
                    "`S = \"${{WORKDIR}}\"` is rejected with an error in {}, which this layer \
                     is checked against; point S at the unpacked sources instead, e.g. \
                     `S = \"${{UNPACKDIR}}\"` (the variable exists from {} on){by_hand}",
                    helpers::release_list(&rejecting),
                    Feature::UnpackDir.first().codename()
                ),
            );
            let fix = (want_fix && no_unpackdir.is_empty())
                .then(|| {
                    let p = fix::assign_parts(cx.source?, s)?;
                    Fix::in_file(
                        cx,
                        vec![Edit::replace(p.value(), "${UNPACKDIR}")],
                        "set S to `${UNPACKDIR}`",
                        Safety::Safe,
                    )
                })
                .flatten();
            out.push((finding, fix));
        }
        out
    }
}

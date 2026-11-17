//! `hygiene/missing-metadata`: a recipe without `SUMMARY`, `LICENSE` or `LIC_FILES_CHKSUM`
//! (warning), or without `HOMEPAGE` (info).
//!
//! See docs/lint-rules.md. Recipe scope: the values come from the evaluated recipe (its
//! `.inc` files, bbappends and classes count). Defaults from configuration do not count:
//! OE-core's `bitbake.conf` sets `SUMMARY ?= "${PN} version ${PV}-${PR}"`,
//! `LICENSE ??= "INVALID"` and `HOMEPAGE = ""` (checked against openembedded-core master,
//! 2026-11), so a variable counts as set only when an operation from a recipe, `.inc`,
//! bbappend or class contributes to it (any status but an inactive override), and its value
//! is not empty.
//!
//! `LIC_FILES_CHKSUM` is required only when `LICENSE` is not `CLOSED` and `SRC_URI` is not
//! empty, as OE-core's `populate_lic_qa_checksum` (`insane.bbclass`, master 2026-11) checks
//! it: `if lic == "CLOSED": return` and `if not lic_files and d.getVar('SRC_URI')`.
//!
//! Not reported when the value depends on python the evaluator cannot run: the value is
//! marked `unevaluated_python`, or anonymous python sets the variable.

use crate::eval::{DataStore, OpStatus};
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::types::{FileKind, LineSpan};

pub struct HygieneMissingMetadata;

/// How a variable stands in the evaluated recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Set,
    Missing,
    /// Depends on python; say nothing.
    Unknown,
}

fn state(data: &DataStore, name: &str) -> State {
    if data.replay().python_vars.contains(name) {
        return State::Unknown;
    }
    let value = data.value(name);
    if value.as_ref().is_some_and(|v| v.unevaluated_python) {
        return State::Unknown;
    }
    let from_metadata = data.history(name).iter().any(|(op, status)| {
        *status != OpStatus::InactiveOverride
            && FileKind::from_path(&op.loc.path) != Some(FileKind::Conf)
    });
    let non_empty = value.is_some_and(|v| !v.text.trim().is_empty());
    if from_metadata && non_empty {
        State::Set
    } else {
        State::Missing
    }
}

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

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(data) = cx.data else { return };
        let mut missing: Vec<&str> = Vec::new();
        for name in ["SUMMARY", "LICENSE"] {
            if state(data, name) == State::Missing {
                missing.push(name);
            }
        }
        let license = data.value("LICENSE");
        let closed = license
            .as_ref()
            .is_some_and(|v| !v.unevaluated_python && v.text.trim() == "CLOSED");
        let license_known = license.as_ref().is_none_or(|v| !v.unevaluated_python);
        let src_uri = data.value("SRC_URI");
        let fetches = src_uri
            .as_ref()
            .is_some_and(|v| !v.unevaluated_python && !v.text.trim().is_empty());
        if license_known && !closed && fetches && state(data, "LIC_FILES_CHKSUM") == State::Missing
        {
            missing.push("LIC_FILES_CHKSUM");
        }
        let line = LineSpan::line(1);
        if !missing.is_empty() {
            out.push(helpers::finding(
                self,
                cx,
                line,
                format!(
                    "recipe has no {}; set {} in the recipe (or its .inc){}",
                    missing.join(", "),
                    if missing.len() == 1 { "it" } else { "them" },
                    if missing.contains(&"LIC_FILES_CHKSUM") {
                        ", or LICENSE = \"CLOSED\" for closed source"
                    } else {
                        ""
                    }
                ),
            ));
        }
        if state(data, "HOMEPAGE") == State::Missing {
            let mut f = helpers::finding(
                self,
                cx,
                line,
                "recipe has no HOMEPAGE; set HOMEPAGE to the upstream project's web page",
            );
            f.severity = Severity::Info;
            out.push(f);
        }
    }
}

//! `override/filesextrapaths`: `FILESEXTRAPATHS` modified without `:prepend` / `:append`,
//! without `:=`, or without the trailing / leading colon.
//!
//! See docs/lint-rules.md and bitbake-semantics.md ("Files and patches"): the idiom is
//! `FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"`. `FILESEXTRAPATHS` is a colon list that
//! OE-core starts as `"__default:"`, so
//!
//! - a plain `=`, `+=`, `.=` (no `:prepend` / `:append`) replaces or corrupts the list; a
//!   `?=` / `??=` in a `.conf` file is the definition of the default and is not reported;
//! - `:prepend` needs the value to end in `:` and `:append` to start with `:`, or the path
//!   is glued to its neighbour;
//! - `${THISDIR}` (and `${FILE_DIRNAME}`, `${FILE}`) must be expanded immediately with `:=`,
//!   or it expands at the end of parsing to the directory of the recipe, not of the file
//!   that wrote it. Without such a reference, or in the `.bb` itself (where the late value
//!   is the same directory), `=` is harmless and not reported (the catalogue row says
//!   "without `:=`" unconditionally).
//!
//! Legacy names (`FILESEXTRAPATHS_prepend`) are left to `override/legacy-syntax`.

use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::{AssignOp, FileKind, LineSpan};

pub struct OverrideFilesextrapaths;

impl Rule for OverrideFilesextrapaths {
    fn id(&self) -> &'static str {
        "override/filesextrapaths"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(file) = cx.file else { return };
        for s in &file.statements {
            let StatementKind::Assign { var, op, value, .. } = &s.kind else {
                continue;
            };
            if var.base != "FILESEXTRAPATHS" || var.legacy_syntax || var.flag.is_some() {
                continue;
            }
            let keyword = var
                .overrides
                .iter()
                .find(|o| matches!(o.as_str(), "append" | "prepend" | "remove"))
                .map(String::as_str);
            let mut problems: Vec<&str> = Vec::new();
            match keyword {
                Some("remove") => continue,
                None => {
                    let default_in_conf = cx.kind == Some(FileKind::Conf)
                        && matches!(op, AssignOp::Default | AssignOp::WeakDefault);
                    if default_in_conf {
                        continue;
                    }
                    problems.push("without `:prepend` (it replaces the search path)");
                }
                Some(_) => {}
            }
            let refers_to_file = ["${THISDIR}", "${FILE_DIRNAME}", "${FILE}"]
                .iter()
                .any(|r| value.contains(r));
            // In the `.bb` itself a late `${THISDIR}` is still the recipe's directory (FILE
            // is the recipe again after the bbappends), so only other files need `:=`.
            if refers_to_file && *op != AssignOp::Immediate && cx.kind != Some(FileKind::Recipe) {
                problems.push("without `:=` (`${THISDIR}` would expand to the recipe's directory)");
            }
            let v = value.trim();
            match keyword {
                Some("prepend") | None if !v.is_empty() && !v.ends_with(':') => {
                    problems.push("without the trailing `:`");
                }
                Some("append") if !v.is_empty() && !v.starts_with(':') => {
                    problems.push("without the leading `:`");
                }
                _ => {}
            }
            if problems.is_empty() {
                continue;
            }
            let name = {
                let mut n = var.base.clone();
                for o in &var.overrides {
                    n.push(':');
                    n.push_str(o);
                }
                n
            };
            let fix = match keyword {
                Some("append") => "FILESEXTRAPATHS:append := \":${THISDIR}/${PN}\"".to_string(),
                _ => {
                    let path = v.trim_end_matches(':');
                    let path = if path.is_empty() || path.chars().count() > 40 {
                        "${THISDIR}/${PN}"
                    } else {
                        path
                    };
                    format!("FILESEXTRAPATHS:prepend := \"{path}:\"")
                }
            };
            out.push(helpers::finding(
                self,
                cx,
                LineSpan::line(s.span.start),
                format!(
                    "`{name} {}` {}; write `{fix}`",
                    op.as_str(),
                    problems.join(", ")
                ),
            ));
        }
    }
}

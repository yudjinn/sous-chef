//! `org/inc-has-version-data`: A `.inc` shared by several versions sets version data.
//!
//! See docs/lint-rules.md. Applies to an `.inc` that `.bb` files of at least two different
//! `PV`s `require` or `include` directly (index edges; a file that is not indexed is
//! skipped). Reported assignments (no overrides):
//!
//! - `PV` set with `=`, `?=`, `??=` or `:=` (appending, e.g. `PV .= "+git"`, is generic);
//! - `SRCREV` or `SRCREV_<name>` (not `SRCREV_FORMAT`), unless the value uses `AUTOREV`;
//! - a checksum flag of `SRC_URI` (`[sha256sum]`, `[<name>.sha256sum]`, md5, sha1, sha384,
//!   sha512);
//! - `LIC_FILES_CHKSUM` set with `=`, `?=`, `??=` or `:=`: as **info**, since sharing it is
//!   fine while the license text is the same in every version.

use crate::index::query;
use crate::index::scan::written_name;
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::{AssignOp, FileKind};

pub struct OrgIncHasVersionData;

const CHECKSUMS: &[&str] = &["sha256sum", "md5sum", "sha1sum", "sha384sum", "sha512sum"];

fn sets(op: AssignOp) -> bool {
    matches!(
        op,
        AssignOp::Set | AssignOp::Default | AssignOp::WeakDefault | AssignOp::Immediate
    )
}

/// The recipe name and the sorted distinct versions of the `.bb` files that pull `file` in.
fn users(cx: &LintContext<'_>) -> Option<(String, Vec<String>)> {
    let row = query::file_by_path(cx.index, &cx.loc.layer, &cx.loc.path).ok()??;
    let mut pns: Vec<String> = Vec::new();
    let mut pvs: Vec<String> = Vec::new();
    for e in query::edges_to(cx.index, row.id).ok()? {
        if !matches!(e.kind.as_str(), "require" | "include" | "include_all")
            || e.src.kind != FileKind::Recipe
        {
            continue;
        }
        if let (Some(pn), Some(pv)) = (e.src.pn.clone(), e.src.pv.clone()) {
            if !pns.contains(&pn) {
                pns.push(pn);
            }
            if !pvs.contains(&pv) {
                pvs.push(pv);
            }
        }
    }
    if pvs.len() < 2 {
        return None;
    }
    pvs.sort_by(|a, b| query::vercmp(a, b));
    pns.sort();
    Some((pns.join(", "), pvs))
}

impl Rule for OrgIncHasVersionData {
    fn id(&self) -> &'static str {
        "org/inc-has-version-data"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        if cx.kind != Some(FileKind::Include) {
            return;
        }
        let Some(file) = cx.file else { return };
        let mut shared: Option<Option<(String, Vec<String>)>> = None;
        for st in &file.statements {
            let StatementKind::Assign { var, op, value, .. } = &st.kind else {
                continue;
            };
            if !var.overrides.is_empty() || var.legacy_syntax {
                continue;
            }
            let base = var.base.as_str();
            let flag = var.flag.as_deref();
            let (what, severity) = match (base, flag) {
                ("PV", None) if sets(*op) => ("the version", Severity::Warning),
                ("LIC_FILES_CHKSUM", None) if sets(*op) => ("license checksums", Severity::Info),
                (b, None)
                    if (b == "SRCREV" || (b.starts_with("SRCREV_") && b != "SRCREV_FORMAT"))
                        && !value.contains("AUTOREV") =>
                {
                    ("a source revision", Severity::Warning)
                }
                ("SRC_URI", Some(f)) if CHECKSUMS.iter().any(|c| f.ends_with(c)) => {
                    ("a source checksum", Severity::Warning)
                }
                _ => continue,
            };
            let Some((pn, versions)) = shared.get_or_insert_with(|| users(cx)).clone() else {
                return;
            };
            let mut name = written_name(var, helpers::statement_text(cx, st));
            if let Some(f) = flag {
                name = format!("{name}[{f}]");
            }
            let list = and_list(&versions);
            let message = if severity == Severity::Info {
                format!(
                    "`{name}` is in an .inc shared by {pn} {list}: fine only while the license \
                     text is identical in every version; otherwise move it to each \
                     {pn}_<version>.bb"
                )
            } else {
                format!(
                    "`{name}` sets {what} in an .inc shared by {pn} {list}, so every version \
                     gets the same value; move it to each {pn}_<version>.bb"
                )
            };
            let mut f = helpers::finding(self, cx, st.span, message);
            f.severity = severity;
            out.push(f);
        }
    }
}

/// `1.0`, `1.0 and 1.1`, `1.0, 1.1 and 1.2`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

//! `override/filesextrapaths`: `FILESEXTRAPATHS` replaced instead of extended, a path not
//! separated by its colon, or a file-relative path not expanded at once.
//!
//! See docs/lint-rules.md and bitbake-semantics.md ("Files and patches"): the idiom is
//! `FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"`. `FILESEXTRAPATHS` is a colon list that
//! OE-core starts as `"__default:"` (`bitbake.conf`, `?=`); `base_set_filespath`
//! (`utils.bbclass`) drops `__default:`, splits the rest on `:` and skips empty entries, and
//! insane.bbclass warns when `__default` is no longer one of the entries. So
//!
//! - `:prepend` (any operator) and `:append` keep the list; so do `=.` (prepend, no space)
//!   and `.=` (append, no space) on the plain name outside a `.conf`, because a recipe, an
//!   `.inc`, a bbappend or a class is parsed after `bitbake.conf` gave the default
//!   (BitBake `ast.py` `DataNode.eval`: `predot` stores `value + old`, `postdot`
//!   `old + value`). A plain `=`, `:=`, `+=`, `=+` replaces or corrupts the list; a
//!   `?=` / `??=` in a `.conf` file is the definition of the default and is not reported.
//!   In a `.conf` (parsed before `bitbake.conf`'s `?=` when it is a `layer.conf`), `=.` and
//!   `.=` lose the default and stay reported; so do `=.` / `.=` on a conditional name
//!   (`FILESEXTRAPATHS:foo =.` builds `FILESEXTRAPATHS:foo`, which replaces the list).
//! - a prepend (`:prepend`, `=.`) needs the value to end in `:` and an append (`:append`,
//!   `.=`) to start with `:`, or the path is glued to its neighbour (`/a__default:`). A value
//!   that ends (prepend) or starts (append) with a `${NAME}` reference is judged only when
//!   the file itself gives `NAME` a literal value; otherwise it is not reported.
//! - `${THISDIR}` (and `${FILE_DIRNAME}`, `${FILE}`) must be expanded immediately with `:=`,
//!   or it expands at the end of parsing to the directory of the recipe, not of the file
//!   that wrote it. Not reported in the `.bb` itself (the late value is the same directory)
//!   nor in an `.inc` that only `.bb` files of its own directory reach (same reason).
//!
//! Legacy names (`FILESEXTRAPATHS_prepend`) are left to `override/legacy-syntax`.

use crate::index::query;
use crate::lint::helpers::{self, ValueEdge};
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::{ParsedFile, StatementKind};
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
        // Computed once, only when needed.
        let mut beside: Option<bool> = None;
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
                    let dot_extends = matches!(op, AssignOp::PrependDot | AssignOp::AppendDot)
                        && var.overrides.is_empty()
                        && cx.kind != Some(FileKind::Conf);
                    if !dot_extends {
                        problems.push("without `:prepend` (it replaces the search path)");
                    }
                }
                Some(_) => {}
            }
            let refers_to_file = ["${THISDIR}", "${FILE_DIRNAME}", "${FILE}"]
                .iter()
                .any(|r| value.contains(r));
            // In the `.bb` itself a late `${THISDIR}` is still the recipe's directory (FILE
            // is the recipe again after the bbappends), so only other files need `:=`; so
            // is an `.inc` that only recipes of its own directory include.
            if refers_to_file
                && *op != AssignOp::Immediate
                && cx.kind != Some(FileKind::Recipe)
                && !*beside.get_or_insert_with(|| inc_beside_its_recipes(cx))
            {
                problems.push("without `:=` (`${THISDIR}` would expand to the recipe's directory)");
            }
            let v = value.trim();
            let appends =
                keyword == Some("append") || (keyword.is_none() && *op == AssignOp::AppendDot);
            if !v.is_empty() && colon_missing(file, v, appends) {
                problems.push(if appends {
                    "without the leading `:`"
                } else {
                    "without the trailing `:`"
                });
            }
            if problems.is_empty() {
                continue;
            }
            let name = helpers::colon_name(var);
            let fix = if appends {
                "FILESEXTRAPATHS:append := \":${THISDIR}/${PN}\"".to_string()
            } else {
                let path = v.trim_end_matches(':');
                let path = if path.is_empty() || path.chars().count() > 40 {
                    "${THISDIR}/${PN}"
                } else {
                    path
                };
                format!("FILESEXTRAPATHS:prepend := \"{path}:\"")
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

/// Whether the (trimmed, not empty) value lacks its separating colon: at the start for an
/// append, at the end for a prepend. A `${NAME}` at that end counts as missing the colon only
/// when the file gives `NAME` a literal value without it; python is never judged.
fn colon_missing(file: &ParsedFile, v: &str, appends: bool) -> bool {
    fn edge(s: &str, appends: bool) -> ValueEdge<'_> {
        if appends {
            helpers::leading_edge(s)
        } else {
            helpers::trailing_edge(s)
        }
    }
    let has_colon = |s: &str| {
        if appends {
            s.starts_with(':')
        } else {
            s.ends_with(':')
        }
    };
    match edge(v, appends) {
        ValueEdge::Literal => !has_colon(v),
        // A path: `THISDIR` (base.bbclass) and `FILE_DIRNAME` (bitbake.conf) are
        // `os.path.dirname` of `FILE`, `LAYERDIR` a layer's path; none starts or ends with
        // `:`.
        ValueEdge::Ref("THISDIR" | "FILE_DIRNAME" | "FILE" | "LAYERDIR") => true,
        ValueEdge::Unknown => false,
        ValueEdge::Ref(name) => helpers::plain_values(file, name).iter().any(|(_, x)| {
            let x = x.trim();
            !x.is_empty() && edge(x, appends) == ValueEdge::Literal && !has_colon(x)
        }),
    }
}

/// Whether the context's file is an `.inc` that only `.bb` files in its own directory reach
/// (through include edges), so that a late `${THISDIR}` / `${FILE_DIRNAME}` is its own
/// directory too.
fn inc_beside_its_recipes(cx: &LintContext<'_>) -> bool {
    if cx.kind != Some(FileKind::Include) {
        return false;
    }
    let Ok(Some(row)) = query::file_for_loc(cx.index, &cx.loc) else {
        return false;
    };
    let Some(inc) = helpers::including_recipes(cx.index, &row) else {
        return false;
    };
    let dir = row.path.parent();
    !inc.other
        && !inc.recipes.is_empty()
        && inc
            .recipes
            .iter()
            .all(|r| r.layer_id == row.layer_id && r.path.parent() == dir)
}

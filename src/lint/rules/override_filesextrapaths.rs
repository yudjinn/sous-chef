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
use crate::lint::fix::{self, Edit, Fix, FixedFinding, Safety};
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
        out.extend(self.scan(cx, false).into_iter().map(|(f, _)| f));
    }

    /// One fix per finding, covering every problem it names, or none when one of them has no
    /// mechanical fix ([`Problems::fix`]).
    fn fixes(&self, cx: &LintContext<'_>) -> Vec<FixedFinding> {
        self.scan(cx, true)
            .into_iter()
            .filter_map(|(finding, fix)| fix.map(|fix| FixedFinding { finding, fix }))
            .collect()
    }
}

/// The problems of one `FILESEXTRAPATHS` statement, for its fix.
struct Problems {
    /// Replaced instead of extended.
    replaced: bool,
    /// `${THISDIR}` (or `${FILE_DIRNAME}`, `${FILE}`) without `:=`.
    needs_immediate: bool,
    /// The separating colon is missing (at the start for an append, else at the end).
    colon: bool,
    appends: bool,
}

impl Problems {
    /// The fix, or `None` when a problem has no mechanical one:
    ///
    /// - missing colon: `:` inserted right after the opening quote (append) or right before
    ///   the closing one (prepend). Safe: it is the separator the rule asks for, and an empty
    ///   entry (`::`) is skipped by `base_set_filespath`.
    /// - no `:=`: the operator `=` of a `:prepend` / `:append` becomes `:=`. Safe when the
    ///   value refers to nothing but `${THISDIR}`, `${FILE_DIRNAME}`, `${FILE}` (what the rule
    ///   says must be expanded now); suggested when it refers to anything else, which `:=`
    ///   expands now too (`${PN}`: in a bbappend from the append's file name, in an `.inc`
    ///   BitBake's `defaultpkgname`, and never a `BBCLASSEXTEND` variant's name). Other
    ///   operators (`+=` on a `:prepend` adds a space): no fix.
    /// - replaced: a plain unconditional `FILESEXTRAPATHS =` / `:=` becomes
    ///   `FILESEXTRAPATHS:prepend :=` (with the colon as above). Suggested: the line now
    ///   extends the list instead of replacing it, which is the rule's point but changes
    ///   meaning. Other operators, conditional names and `.conf` forms: no fix (the intent
    ///   is not clear).
    fn fix(
        &self,
        cx: &LintContext<'_>,
        s: &crate::parse::Statement,
        var: &crate::types::VarRef,
        op: AssignOp,
        value: &str,
    ) -> Option<Fix> {
        let source = cx.source?;
        let p = fix::assign_parts(source, s)?;
        let mut edits = Vec::new();
        let mut safety = Safety::Safe;
        let mut what = Vec::new();
        if self.replaced {
            if !(matches!(op, AssignOp::Set | AssignOp::Immediate)
                && var.overrides.is_empty()
                && cx.kind != Some(FileKind::Conf))
            {
                return None;
            }
            edits.push(Edit::replace(p.name.clone(), "FILESEXTRAPATHS:prepend"));
            if op != AssignOp::Immediate {
                edits.push(Edit::replace(p.op.clone(), ":="));
            }
            safety = Safety::Suggested;
            what.push("extend the list with `FILESEXTRAPATHS:prepend :=` instead of replacing it");
        } else if self.needs_immediate {
            if op != AssignOp::Set {
                return None;
            }
            edits.push(Edit::replace(p.op.clone(), ":="));
            let refs_only_file = !value.contains("${@")
                && value.match_indices("${").all(|(at, _)| {
                    ["${THISDIR}", "${FILE_DIRNAME}", "${FILE}"]
                        .iter()
                        .any(|r| value[at..].starts_with(r))
                });
            if refs_only_file {
                what.push("expand it at once with `:=`");
            } else {
                safety = Safety::Suggested;
                what.push(
                    "expand it at once with `:=`, which also expands its other references \
                     (`${PN}` and the like) now",
                );
            }
        }
        if self.colon {
            let appends = self.appends && !self.replaced;
            // A blank between the quote and the path would stay part of the entry.
            let blank_edge = if appends {
                value.starts_with(char::is_whitespace)
            } else {
                value.ends_with(char::is_whitespace)
            };
            if blank_edge {
                return None;
            }
            edits.push(Edit::insert(
                if appends { p.open + 1 } else { p.close },
                ":",
            ));
            what.push(if appends {
                "add the leading `:`"
            } else {
                "add the trailing `:`"
            });
        }
        Fix::in_file(cx, edits, helpers::and_list(&what), safety)
    }
}

impl OverrideFilesextrapaths {
    fn scan(&self, cx: &LintContext<'_>, want_fix: bool) -> Vec<(Finding, Option<Fix>)> {
        let mut out = Vec::new();
        let Some(file) = cx.file else { return out };
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
            let found = Problems {
                replaced: problems.iter().any(|p| p.contains(":prepend")),
                needs_immediate: problems.iter().any(|p| p.contains(":=")),
                colon: problems
                    .iter()
                    .any(|p| p.contains("the leading") || p.contains("the trailing")),
                appends,
            };
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
            let finding = helpers::finding(
                self,
                cx,
                LineSpan::line(s.span.start),
                format!(
                    "`{name} {}` {}; write `{fix}`",
                    op.as_str(),
                    problems.join(", ")
                ),
            );
            let fix = want_fix
                .then(|| found.fix(cx, s, var, *op, value))
                .flatten();
            out.push((finding, fix));
        }
        out
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

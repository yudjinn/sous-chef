//! `override/shadowed`: A plain assignment that is always overwritten by a later
//! unconditional `=` in the same tree.
//!
//! See docs/lint-rules.md. Recipe scope, on the evaluated [`DataStore`]. Reported: an
//! assignment `NAME = ...` / `NAME := ...` (no `:append` / `:prepend` / `:remove`, no flag,
//! no `${...}` in the name) in one of the recipe's own files (the `.bb`, its `.inc` files and
//! bbappends; never classes or configuration, whose values recipes are meant to replace)
//! that the datastore marks [`OpStatus::Shadowed`], when a later `NAME = ...` /
//! `NAME := ...` of exactly the same name (so under the same conditions) follows in the
//! replay. Not reported:
//!
//! - when the later assignment is in a bbappend and the earlier one is not in that same
//!   bbappend (replacing a recipe's value is what bbappends are for);
//! - when anything in the recipe's tree (classes included) could read the earlier value
//!   before it is replaced: a `:=` whose value mentions `NAME` (`${NAME}` or python), or an
//!   `inherit` / `require` / `include` whose argument mentions `${NAME}`. Their order
//!   relative to the two assignments is not checked, so any such statement is enough;
//! - functions (a later definition replacing an earlier one is how tasks are overridden).

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use crate::eval::OpStatus;
use crate::index::query::{self, StatementFilter, StatementRow};
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::types::{AssignOp, FileKind, LineSpan, Loc, VarRef};

pub struct OverrideShadowed;

impl Rule for OverrideShadowed {
    fn id(&self) -> &'static str {
        "override/shadowed"
    }

    fn scope(&self) -> Scope {
        Scope::Recipe
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(data) = cx.data else { return };
        let (tree, all) = helpers::recipe_pass_files(cx, data);
        if tree.is_empty() {
            return;
        }
        let tree_rows = statements(cx, &tree);
        let bases: BTreeSet<String> = tree_rows
            .iter()
            .filter(|s| {
                s.kind == "assign"
                    && matches!(s.op.as_deref(), Some("=" | ":="))
                    && s.flag.is_none()
                    && !s.legacy
            })
            .filter_map(|s| s.base.clone())
            .collect();
        if bases.is_empty() {
            return;
        }
        let readers = statements(cx, &all)
            .into_iter()
            .filter(|s| match s.kind.as_str() {
                "assign" => s.op.as_deref() == Some(":="),
                "inherit" | "include" => true,
                _ => false,
            })
            .collect::<Vec<_>>();
        for base in &bases {
            if data.is_function(base) {
                continue;
            }
            let history = data.history(base);
            for (i, (op, status)) in history.iter().enumerate() {
                if *status != OpStatus::Shadowed || !plain_set(&op.var, op.op) {
                    continue;
                }
                let key = (op.loc.layer.clone(), op.loc.path.clone());
                if !tree.contains(&key) {
                    continue;
                }
                let name = helpers::colon_name(&op.var);
                let Some(later) = history[i + 1..].iter().map(|(o, _)| o).find(|o| {
                    plain_set(&o.var, o.op)
                        && helpers::colon_name(&o.var) == name
                        && (FileKind::from_path(&o.loc.path) != Some(FileKind::Append)
                            || (o.loc.layer == op.loc.layer && o.loc.path == op.loc.path))
                }) else {
                    continue;
                };
                if read_somewhere(&name, &readers) {
                    continue;
                }
                out.push(Finding {
                    loc: Loc {
                        lines: LineSpan::line(op.loc.lines.start),
                        ..op.loc.clone()
                    },
                    ..helpers::finding(
                        self,
                        cx,
                        op.loc.lines,
                        format!(
                            "`{name} {}` has no effect: the `{}` at {} always replaces it; \
                             remove it, or make it a default with `?=`",
                            op.op.as_str(),
                            later.op.as_str(),
                            Loc {
                                lines: LineSpan::line(later.loc.lines.start),
                                ..later.loc.clone()
                            }
                        ),
                    )
                });
            }
        }
    }
}

/// `NAME =` / `NAME :=`, no keyword, no flag, no `${` in the name, not legacy.
fn plain_set(var: &VarRef, op: AssignOp) -> bool {
    matches!(op, AssignOp::Set | AssignOp::Immediate)
        && !var.legacy_syntax
        && var.flag.is_none()
        && !var.base.contains("${")
        && var
            .overrides
            .iter()
            .all(|o| !o.contains("${") && !matches!(o.as_str(), "append" | "prepend" | "remove"))
}

/// Whether something may read `name`'s value at parse time: a `:=` whose value mentions it
/// (`${name}`, or python that names it), or an inherit / include argument with `${name}`.
/// Conservative: the position of the statement is not checked.
fn read_somewhere(name: &str, readers: &[StatementRow]) -> bool {
    let reference = format!("${{{name}}}");
    readers.iter().any(|s| {
        let v = s.value.as_deref().unwrap_or("");
        let n = s.name.as_deref().unwrap_or("");
        match s.kind.as_str() {
            "assign" => v.contains(&reference) || (v.contains("${@") && v.contains(name)),
            _ => n.contains(&reference) || v.contains(&reference),
        }
    })
}

fn statements(cx: &LintContext<'_>, files: &HashSet<(String, PathBuf)>) -> Vec<StatementRow> {
    let mut out = Vec::new();
    for (layer, path) in files {
        let Ok(Some(f)) = query::file_by_path(cx.index, layer, path) else {
            continue;
        };
        out.extend(
            query::statements_in(cx.index, f.id, &StatementFilter::default()).unwrap_or_default(),
        );
    }
    out
}

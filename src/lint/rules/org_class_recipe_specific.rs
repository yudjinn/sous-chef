//! `org/class-recipe-specific`: A bbclass sets per-recipe values, or uses `=` where `?=`
//! would let recipes override.
//!
//! See docs/lint-rules.md. In a `.bbclass`, for assignments without overrides or flag:
//!
//! - `SRC_URI`, `PV` or `LICENSE` set with `=` or `:=`: per-recipe data that overwrites what
//!   a recipe set before `inherit`. `?=` / `??=` (a default) and `+=` / `:append` (an
//!   addition) are not reported.
//! - any other variable set with `=` or `:=` that a file inheriting the class directly
//!   (index `inherit` edges) sets with `=`, `?=`, `??=` or `:=` (no overrides, no flag)
//!   **before** its `inherit` line: the class silently replaces the recipe's value. The rule
//!   reports only this proven case, not every `=` in a class, since most of a class's
//!   variables are its own. A recipe value written exactly as the class's (whitespace
//!   aside) is not lost and does not count. Needs the index; a file that is not indexed gets only the first
//!   check.

use crate::index::query;
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::{AssignOp, FileKind, LineSpan, Loc};

pub struct OrgClassRecipeSpecific;

const PER_RECIPE: &[&str] = &["SRC_URI", "PV", "LICENSE"];

/// Words of a value with whitespace collapsed, to compare two values as written.
fn normalised(value: &str) -> Vec<&str> {
    value.split_whitespace().collect()
}

/// A user that sets `base` before inheriting the class to something other than the class's
/// own `value` (as written, whitespace aside): its pointer. A recipe that sets the same text
/// loses nothing when the class sets it again (OE-core's `python-async.inc` and
/// `pypi.bbclass` both write `SECTION = "devel/python"`: lint-precision).
fn set_before_inherit(
    users: &[query::UserRow],
    cx: &LintContext<'_>,
    base: &str,
    value: &str,
) -> Option<String> {
    let scope = query::StatementFilter {
        kinds: vec!["assign".into()],
        ..query::StatementFilter::default()
    };
    for u in users {
        if !matches!(
            u.file.kind,
            FileKind::Recipe | FileKind::Append | FileKind::Include
        ) {
            continue;
        }
        let Ok(stmts) = query::statements_in(cx.index, u.file.id, &scope) else {
            continue;
        };
        let hit = stmts.iter().find(|s| {
            s.base.as_deref() == Some(base)
                && s.overrides.is_empty()
                && s.flag.is_none()
                && matches!(s.op.as_deref(), Some("=" | "?=" | "??=" | ":="))
                && s.loc.lines.start < u.via.lines.start
                && s.value.as_deref().map(normalised) != Some(normalised(value))
        });
        if let Some(s) = hit {
            let at = Loc {
                lines: LineSpan::line(s.loc.lines.start),
                ..s.loc.clone()
            };
            return Some(at.to_string());
        }
    }
    None
}

impl Rule for OrgClassRecipeSpecific {
    fn id(&self) -> &'static str {
        "org/class-recipe-specific"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        if cx.kind != Some(FileKind::Class) {
            return;
        }
        let Some(file) = cx.file else { return };
        let Some(class) = cx.loc.path.file_stem().and_then(|s| s.to_str()) else {
            return;
        };
        let mut users: Option<Vec<query::UserRow>> = None;
        for st in &file.statements {
            let StatementKind::Assign { var, op, value, .. } = &st.kind else {
                continue;
            };
            if !var.overrides.is_empty() || var.flag.is_some() || var.legacy_syntax {
                continue;
            }
            if !matches!(op, AssignOp::Set | AssignOp::Immediate) {
                continue;
            }
            let base = var.base.as_str();
            if PER_RECIPE.contains(&base) {
                out.push(helpers::finding(
                    self,
                    cx,
                    st.span,
                    format!(
                        "class {class} sets {base} with `{op}`: that is per-recipe data and it \
                         replaces what a recipe sets before `inherit {class}`; set {base} in \
                         the recipe, or write `{base} ?=` for a default"
                    ),
                ));
                continue;
            }
            let users = users.get_or_insert_with(|| {
                query::inheritors(cx.index, class, false).unwrap_or_default()
            });
            if let Some(at) = set_before_inherit(users, cx, base, value) {
                out.push(helpers::finding(
                    self,
                    cx,
                    st.span,
                    format!(
                        "`{base} {op}` in class {class} replaces the value {at} sets before \
                         `inherit {class}`; write `{base} ?=` so recipes can set it"
                    ),
                ));
            }
        }
    }
}

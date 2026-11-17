//! `org/class-export-functions`: class task functions without `EXPORT_FUNCTIONS`, or
//! `do_<task>` defined directly in a class.
//!
//! See docs/lint-rules.md. In `foo.bbclass`:
//!
//! - a function `foo_do_<task>` (no overrides) with no `EXPORT_FUNCTIONS do_<task>` in the
//!   class: recipes never get it as `do_<task>`. Skipped for a class name with `-`, which
//!   BitBake refuses with `EXPORT_FUNCTIONS` for shell functions.
//! - a function `do_<task>` (no overrides) for a task the class does not add itself
//!   (`addtask`): it replaces a `do_<task>` the recipe defined before `inherit foo`. With
//!   `EXPORT_FUNCTIONS` BitBake keeps the recipe's own definition (`ast.py`
//!   `ExportFuncsNode.eval` leaves a function alone unless it is unset or its own export
//!   stub), which is why the exported form is the one to use. A class that adds the task
//!   (`addtask foo`) and defines `do_foo` is the usual way to introduce a new task and is
//!   not reported. Skipped for a class name with `-` too, since the suggested
//!   `foo-bar_do_x` cannot be exported.

use std::collections::HashSet;

use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::types::FileKind;

pub struct OrgClassExportFunctions;

/// `do_x` for `x` or `do_x`.
fn task_name(t: &str) -> String {
    if t.starts_with("do_") {
        t.to_string()
    } else {
        format!("do_{t}")
    }
}

impl Rule for OrgClassExportFunctions {
    fn id(&self) -> &'static str {
        "org/class-export-functions"
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
        let mut exported: HashSet<String> = HashSet::new();
        let mut added: HashSet<String> = HashSet::new();
        for st in &file.statements {
            match &st.kind {
                StatementKind::ExportFunctions { names } => {
                    exported.extend(names.iter().cloned());
                }
                StatementKind::AddTask { name, .. } => {
                    added.extend(name.split_whitespace().map(task_name));
                }
                _ => {}
            }
        }
        let prefix = format!("{class}_");
        for st in &file.statements {
            let StatementKind::Function {
                name: Some(var), ..
            } = &st.kind
            else {
                continue;
            };
            if !var.overrides.is_empty() || var.flag.is_some() || var.legacy_syntax {
                continue;
            }
            let fname = var.base.as_str();
            if let Some(task) = fname.strip_prefix(&prefix) {
                if task.starts_with("do_") && !class.contains('-') && !exported.contains(task) {
                    out.push(helpers::finding(
                        self,
                        cx,
                        st.span,
                        format!(
                            "{fname} is defined but the class has no `EXPORT_FUNCTIONS {task}`, \
                             so recipes that inherit {class} never run it as {task}; add \
                             `EXPORT_FUNCTIONS {task}`"
                        ),
                    ));
                }
                continue;
            }
            if fname.starts_with("do_")
                && fname.len() > 3
                && !added.contains(fname)
                && !class.contains('-')
            {
                out.push(helpers::finding(
                    self,
                    cx,
                    st.span,
                    format!(
                        "{fname} is defined directly in a class, so it replaces the {fname} a \
                         recipe defines before `inherit {class}`; name it {class}_{fname} and \
                         add `EXPORT_FUNCTIONS {fname}`"
                    ),
                ));
            }
        }
    }
}

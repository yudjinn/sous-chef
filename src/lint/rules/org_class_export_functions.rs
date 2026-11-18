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

use crate::index::query;
use crate::lint::fix::{self, Edit, Fix, FixedFinding, Safety};
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::StatementKind;
use crate::parse::words;
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

/// The tasks a python `bb.build.addtask('do_x', ...)` in `text` adds.
fn python_addtasks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, _) in text.match_indices("bb.build.addtask(") {
        let rest = text[at + "bb.build.addtask(".len()..].trim_start();
        let Some(q) = rest.chars().next().filter(|c| *c == '\'' || *c == '"') else {
            continue;
        };
        if let Some(end) = rest[1..].find(q) {
            out.push(task_name(&rest[1..1 + end]));
        }
    }
    out
}

/// Whether `file` uses function `name` as a helper: a word of a `[prefuncs]` /
/// `[postfuncs]` flag or of a variable whose name ends in `FUNCS` or `_COMMAND` (function
/// lists: `PACKAGEFUNCS`, `ROOTFS_POSTPROCESS_COMMAND`), the first word of a line of
/// another function's body (a shell call), or `exec_func("name"` in one (a python call).
/// A name merely mentioned (`bb.build.addtask('do_x', 'do_build', 'do_install', d)`, a
/// `depends` flag `x:do_install`) does not count.
fn referenced_elsewhere(
    cx: &LintContext<'_>,
    file: &crate::parse::ParsedFile,
    own: &crate::parse::Statement,
    name: &str,
) -> bool {
    let has_word = |text: &str| {
        text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .any(|w| w == name)
    };
    let calls = [
        format!("exec_func(\"{name}\""),
        format!("exec_func('{name}'"),
    ];
    file.statements.iter().any(|s| {
        if std::ptr::eq(s, own) {
            return false;
        }
        match &s.kind {
            StatementKind::Assign { var, value, .. } => {
                let list = matches!(var.flag.as_deref(), Some("prefuncs" | "postfuncs"))
                    || (var.flag.is_none()
                        && (var.base.ends_with("FUNCS") || var.base.ends_with("_COMMAND")));
                list && has_word(value)
            }
            StatementKind::Function { .. } | StatementKind::PyDef { .. } => {
                // The body only: the name line of the function is the statement's first line.
                let text = helpers::statement_text(cx, s);
                let body = text.split_once('\n').map_or("", |(_, body)| body);
                calls.iter().any(|c| body.contains(c.as_str()))
                    || body.lines().any(|l| {
                        l.split(|c: char| c.is_whitespace() || c == ';')
                            .find(|w| !w.is_empty())
                            == Some(name)
                    })
            }
            _ => false,
        }
    })
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

    /// Suggested, both cases: `EXPORT_FUNCTIONS do_<task>` as a line of its own right after
    /// the function (after, because `ExportFuncsNode.eval` copies the `python` flag from
    /// `<class>_do_<task>` when the line is parsed, so the function must already be
    /// defined), and for a `do_<task>` defined directly, the function renamed to
    /// `<class>_do_<task>`. Recipes then get the class's task only when they do not define
    /// their own, which is what the rule recommends and changes what an inheriting recipe
    /// runs.
    fn fixes(&self, cx: &LintContext<'_>) -> Vec<FixedFinding> {
        let (Some(file), Some(source)) = (cx.file, cx.source) else {
            return Vec::new();
        };
        let Some(class) = cx.loc.path.file_stem().and_then(|s| s.to_str()) else {
            return Vec::new();
        };
        let exported: HashSet<&str> = file
            .statements
            .iter()
            .filter_map(|s| match &s.kind {
                StatementKind::ExportFunctions { names } => Some(names),
                _ => None,
            })
            .flatten()
            .map(String::as_str)
            .collect();
        let mut found = Vec::new();
        self.check(cx, &mut found);
        let prefix = format!("{class}_");
        found
            .into_iter()
            .filter_map(|finding| {
                let s = file.statements.iter().find(|s| {
                    s.span == finding.loc.lines
                        && matches!(&s.kind, StatementKind::Function { name: Some(_), .. })
                })?;
                let StatementKind::Function {
                    name: Some(var), ..
                } = &s.kind
                else {
                    return None;
                };
                let fname = var.base.as_str();
                let (task, rename) = match fname.strip_prefix(&prefix) {
                    Some(task) => (task.to_string(), None),
                    None => (fname.to_string(), Some(format!("{class}_{fname}"))),
                };
                let mut edits = Vec::new();
                if let Some(new) = &rename {
                    edits.push(Edit::replace(fix::function_name(source, s)?, new.clone()));
                }
                if !exported.contains(task.as_str()) {
                    edits.push(fix::insert_line_after(
                        source,
                        s,
                        &format!("EXPORT_FUNCTIONS {task}"),
                    )?);
                }
                let what = match &rename {
                    Some(new) => format!(
                        "rename {fname} to {new} and add `EXPORT_FUNCTIONS {task}` after it \
                         (a recipe's own {task} is then kept)"
                    ),
                    None => format!(
                        "add `EXPORT_FUNCTIONS {task}` after {fname} (recipes inheriting \
                         {class} then run it as {task})"
                    ),
                };
                let fix = Fix::in_file(cx, edits, what, Safety::Suggested)?;
                Some(FixedFinding { finding, fix })
            })
            .collect()
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
                    added.extend(words::words(name).into_iter().map(task_name));
                }
                _ => {}
            }
        }
        // Tasks the class adds from python (`bb.build.addtask('do_x', ...)`, kernel.bbclass's
        // `do_bundle_initramfs`).
        added.extend(python_addtasks(cx.source.unwrap_or("")));
        // Every task some file of the workspace adds (base.bbclass's standard ones, a class
        // that inherits this one): a `do_x` that is none of them is no task. An index error,
        // or an index with no `addtask` at all (no core layer to say what the tasks are),
        // counts as "a task" (the behaviour before x7).
        let tasks: Option<HashSet<String>> = query::addtask_names(cx.index)
            .ok()
            .map(|names| {
                names
                    .iter()
                    .flat_map(|n| words::words(n).into_iter().map(task_name))
                    .collect::<HashSet<String>>()
            })
            .filter(|s| !s.is_empty());
        let is_task = |t: &str| added.contains(t) || tasks.as_ref().is_none_or(|s| s.contains(t));
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
            // A helper, not a task: named in a value of the class (a `[postfuncs]` /
            // `[prefuncs]` flag, `PACKAGEFUNCS`) or in the body of another function
            // (`bb.build.exec_func("do_package_deb", d)`, ptest.bbclass's
            // `do_configure_ptest` called from `do_configure_ptest_base`).
            if referenced_elsewhere(cx, file, st, fname) {
                continue;
            }
            if let Some(task) = fname.strip_prefix(&prefix) {
                if task.starts_with("do_")
                    && !class.contains('-')
                    && !exported.contains(task)
                    && is_task(task)
                {
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
                && is_task(fname)
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

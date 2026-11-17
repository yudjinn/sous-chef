//! `hygiene/parse-error`: Any `Unknown` statement or parser diagnostic.
//!
//! See docs/lint-rules.md. Implemented by T4.1 as the sample rule.
//!
//! The parser has no severity on its diagnostics, so this rule classifies them by message
//! (the fixture test pins every message it relies on):
//!
//! - **Not reported:** whitespace after a trailing `\` (BitBake right-strips the line and
//!   still continues it; `hygiene/continuation` owns that finding); `addfragments`, which
//!   BitBake accepts in every file kind; `addpylib` in a `.conf` file (configuration only).
//! - **Warning:** the `addtask` quirk that glues two `before` / `after` groups together.
//!   BitBake accepts the line, but the resulting task name is almost certainly not meant.
//! - **Error:** everything else, including `Unknown` statements without a diagnostic.
//!
//! In a `.conf` file it also reports what only BitBake's recipe parser accepts: functions,
//! `def`, `inherit`, `inherit_defer`, `addtask`, `deltask`, `addhandler` and
//! `EXPORT_FUNCTIONS`. `ConfHandler.feeder` (BitBake master 2026-11) accepts assignments,
//! `include`, `require`, `include_all`, `export`, `unset`, `addpylib` (base configuration)
//! and `addfragments`, and raises "unparsed line" for anything else.

use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::{ParseDiagnostic, StatementKind};
use crate::types::FileKind;

pub struct HygieneParseError;

/// How a parser diagnostic is reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Skip,
    Report(Severity),
}

/// Classifies a parser diagnostic for a file of `kind` (see the module docs).
pub fn classify(d: &ParseDiagnostic, kind: Option<FileKind>) -> Class {
    let m = d.message.as_str();
    if m.starts_with("whitespace after `\\`") || m.starts_with("`addfragments` ") {
        return Class::Skip;
    }
    if m.starts_with("`addpylib` ") {
        return if kind == Some(FileKind::Conf) {
            Class::Skip
        } else {
            Class::Report(Severity::Error)
        };
    }
    if m.starts_with("addtask: `before`/`after` groups") {
        return Class::Report(Severity::Warning);
    }
    Class::Report(Severity::Error)
}

/// What to write instead, by diagnostic.
fn advice(m: &str) -> &'static str {
    if m.starts_with("addtask:") {
        "write all `after` tasks in one group and all `before` tasks in another"
    } else if m.starts_with("`addpylib`") {
        "move `addpylib` to conf/layer.conf"
    } else if m.contains("byte order mark") {
        "save the file as UTF-8 without a BOM"
    } else if m.starts_with("indented comment") {
        "start the comment in column 0"
    } else if m.contains("never closed") {
        "close the function with `}` in column 0"
    } else if m.contains("continuation") || m.contains("continued statement") {
        "remove the `\\` or the comment so each statement is complete"
    } else {
        "fix the line so BitBake can parse it, or remove it"
    }
}

impl Rule for HygieneParseError {
    fn id(&self) -> &'static str {
        "hygiene/parse-error"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(file) = cx.file else { return };
        for d in &file.diagnostics {
            let Class::Report(severity) = classify(d, cx.kind) else {
                continue;
            };
            let mut f = helpers::finding(
                self,
                cx,
                d.span,
                format!("{}; {}", d.message, advice(&d.message)),
            );
            f.severity = severity;
            out.push(f);
        }
        for s in &file.statements {
            let covered = |span: crate::types::LineSpan| {
                file.diagnostics
                    .iter()
                    .any(|d| d.span.start <= span.end && span.start <= d.span.end)
            };
            match &s.kind {
                StatementKind::Unknown { .. } if !covered(s.span) => {
                    out.push(helpers::finding(
                        self,
                        cx,
                        s.span,
                        "unparsed line; fix the line so BitBake can parse it, or remove it",
                    ));
                }
                k if cx.kind == Some(FileKind::Conf) => {
                    if let Some(what) = recipe_only(k) {
                        out.push(helpers::finding(
                            self,
                            cx,
                            crate::types::LineSpan::line(s.span.start),
                            format!(
                                "{what} in a .conf file: BitBake's configuration parser \
                                 rejects it as an unparsed line; move it to a class or recipe \
                                 (use INHERIT += for a global class)"
                            ),
                        ));
                    }
                }
                _ => {}
            }
        }
    }
}

/// The statements only BitBake's recipe parser accepts.
fn recipe_only(k: &StatementKind) -> Option<&'static str> {
    Some(match k {
        StatementKind::Function { .. } => "a function",
        StatementKind::PyDef { .. } => "a python `def`",
        StatementKind::Inherit {
            deferred: false, ..
        } => "`inherit`",
        StatementKind::Inherit { deferred: true, .. } => "`inherit_defer`",
        StatementKind::AddTask { .. } => "`addtask`",
        StatementKind::DelTask { .. } => "`deltask`",
        StatementKind::AddHandler { .. } => "`addhandler`",
        StatementKind::ExportFunctions { .. } => "`EXPORT_FUNCTIONS`",
        _ => return None,
    })
}

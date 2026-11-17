//! `hygiene/parse-error`: Any `Unknown` statement or parser diagnostic.
//!
//! See docs/lint-rules.md. Implemented by T4.1 as the sample rule.
//!
//! The parser has no severity on its diagnostics, so this rule classifies them by message
//! (the fixture test pins every message it relies on):
//!
//! - **Not reported:** whitespace after a trailing `\` (BitBake right-strips the line and
//!   still continues it; `hygiene/continuation` owns that finding). (`addfragments` and
//!   `addpylib` are statement kinds of their own and have no diagnostic; with too few
//!   arguments each is an "unparsed line" error.)
//! - **Warning:** the `addtask` quirk that glues two `before` / `after` groups together.
//!   BitBake accepts the line, but the resulting task name is almost certainly not meant.
//! - **Error:** everything else, including `Unknown` statements without a diagnostic.
//!
//! In a `.conf` file it also reports what only BitBake's recipe parser accepts: functions,
//! `def`, `inherit`, `inherit_defer`, `addtask`, `deltask`, `addhandler` and
//! `EXPORT_FUNCTIONS`. `ConfHandler.feeder` (BitBake master 2026-11) accepts assignments,
//! `include`, `require`, `include_all`, `export`, `unset`, `addpylib` (base configuration)
//! and `addfragments`, and raises "unparsed line" for anything else.
//!
//! `addpylib` (`StatementKind::AddPyLib`) is judged by where it is and by release
//! (bitbake-semantics.md, "Directives"): `ConfHandler.feeder` accepts it only when
//! `baseconfig and conffile`, that is in a file `cookerdata` parses directly with
//! `parse_config_file` (`conf/bblayers.conf`, each layer's `conf/layer.conf`,
//! `conf/bitbake.conf`), never in a file those include and never through `BBHandler`
//! (recipes, classes, `.inc`). Anywhere else it is an error on every release. In the base
//! configuration it is an error on a release without `Feature::AddPyLib` (BitBake 2.0 and
//! 2.2 have no such pattern, and the line is "unparsed").

use std::path::Path;

use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::{ParseDiagnostic, StatementKind};
use crate::release::Feature;
use crate::types::{FileKind, LineSpan};

pub struct HygieneParseError;

/// How a parser diagnostic is reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Skip,
    Report(Severity),
}

/// Classifies a parser diagnostic for a file of `kind` (see the module docs). No message
/// depends on the kind at present; the argument is kept for callers.
pub fn classify(d: &ParseDiagnostic, _kind: Option<FileKind>) -> Class {
    let m = d.message.as_str();
    if m.starts_with("whitespace after `\\`") {
        return Class::Skip;
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
            let covered = |span: LineSpan| {
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
                StatementKind::AddPyLib { .. } => {
                    if let Some(f) = self.addpylib(cx, s.span.start) {
                        out.push(f);
                    }
                }
                k if cx.kind == Some(FileKind::Conf) => {
                    if let Some(what) = recipe_only(k) {
                        out.push(helpers::finding(
                            self,
                            cx,
                            LineSpan::line(s.span.start),
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

impl HygieneParseError {
    /// The finding for an `addpylib` statement on `line`, if BitBake rejects it there.
    fn addpylib(&self, cx: &LintContext<'_>, line: u32) -> Option<Finding> {
        let at = LineSpan::line(line);
        if !is_base_config(cx) {
            return Some(helpers::finding(
                self,
                cx,
                at,
                "unparsed line: BitBake accepts `addpylib` only in the base configuration \
                 (conf/bblayers.conf, a layer's conf/layer.conf, conf/bitbake.conf), not in a \
                 recipe, a class or a file they include; move it to conf/layer.conf",
            ));
        }
        let lacking = helpers::releases_lacking(cx, Feature::AddPyLib);
        (!lacking.is_empty()).then(|| {
            helpers::finding(
                self,
                cx,
                at,
                format!(
                    "unparsed line: `addpylib` is not available in {}, which this layer is \
                     checked against (BitBake there has no such directive); remove it while \
                     the layer claims that release",
                    helpers::release_list(&lacking)
                ),
            )
        })
    }
}

/// Whether the file is one BitBake parses as base configuration (`cookerdata`
/// `parse_config_file`): `conf/layer.conf`, `conf/bitbake.conf` or `conf/bblayers.conf` at
/// the root of its layer (or of the build directory).
fn is_base_config(cx: &LintContext<'_>) -> bool {
    cx.kind == Some(FileKind::Conf)
        && ["conf/layer.conf", "conf/bitbake.conf", "conf/bblayers.conf"]
            .iter()
            .any(|p| cx.loc.path == Path::new(p))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(m: &str) -> ParseDiagnostic {
        ParseDiagnostic {
            span: LineSpan::line(1),
            message: m.into(),
        }
    }

    #[test]
    fn no_addfragments_or_addpylib_special_case() {
        // The parser makes `addfragments` and `addpylib` statement kinds; no message about
        // them is skipped.
        assert_eq!(
            classify(&diag("`addfragments` x"), Some(FileKind::Conf)),
            Class::Report(Severity::Error)
        );
        assert_eq!(
            classify(
                &diag("unparsed line: `addfragments` needs at least three arguments"),
                Some(FileKind::Conf)
            ),
            Class::Report(Severity::Error)
        );
        assert_eq!(
            classify(
                &diag("unparsed line: `addpylib` needs two arguments"),
                Some(FileKind::Conf)
            ),
            Class::Report(Severity::Error)
        );
    }
}

//! `org/include-must-exist`: `include` of a file the recipe cannot work without, or
//! `require` of a file that does not resolve.
//!
//! See docs/lint-rules.md. Uses the index edges of the file (a file that is not indexed is
//! skipped), so resolution is the scanner's: the including file's directory, then `BBPATH`,
//! with only static variables (`PN`, `BPN`, `PV`, `THISDIR`...) expanded.
//!
//! - `include X` in a `.bb`, `.bbappend`, `.inc` or `.bbclass`, where `X` resolves to a file
//!   that sets `SRC_URI` or `LICENSE` (`=`, `?=`, `??=`, `:=`, no overrides) or defines a task
//!   (a `do_*` function without overrides, or `addtask`): `include` silently skips a missing
//!   file (`ConfHandler.include_single_file`), so the recipe would build without it.
//! - `require X` (any file kind) whose edge is unresolved, when the target text needs no
//!   evaluation (no `${` left after static expansion) and is not an absolute path that
//!   exists outside the workspace: BitBake stops with "Could not include required file".
//!   A single-target edge keeps the text as written, so a `require ${BPN}.inc` that expands
//!   statically but finds nothing is not reported either (a known miss).

use std::path::Path;

use crate::index::query;
use crate::lint::fix::{self, Edit, Fix, FixedFinding, Safety};
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::{IncludeMode, StatementKind};
use crate::types::{FileKind, LineSpan, Loc};

pub struct OrgIncludeMustExist;

/// What makes `file` essential: a description and its pointer.
fn essential(cx: &LintContext<'_>, file: &query::FileRow) -> Option<(String, Loc)> {
    let stmts = query::statements_in(cx.index, file.id, &query::StatementFilter::default()).ok()?;
    for s in stmts {
        let at = Loc {
            lines: LineSpan::line(s.loc.lines.start),
            ..s.loc.clone()
        };
        let plain = s.overrides.is_empty() && s.flag.is_none() && !s.legacy;
        match (s.kind.as_str(), s.base.as_deref()) {
            ("assign", Some(b @ ("SRC_URI" | "LICENSE")))
                if plain && matches!(s.op.as_deref(), Some("=" | "?=" | "??=" | ":=")) =>
            {
                return Some((format!("it sets {b}"), at));
            }
            ("function", Some(b)) if plain && b.starts_with("do_") => {
                return Some((format!("it defines {b}"), at));
            }
            ("addtask", _) => return Some(("it adds a task".to_string(), at)),
            _ => {}
        }
    }
    None
}

impl Rule for OrgIncludeMustExist {
    fn id(&self) -> &'static str {
        "org/include-must-exist"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    /// Safe, for `include` of an essential file only: the keyword becomes `require`. The
    /// rule reports it only when the index resolved the file, and BitBake's `include` and
    /// `require` read a file they find the same way (`IncludeNode.eval` passes `error_out`
    /// to `ConfHandler.include`, which matters only when the file is not found), so the
    /// change is exactly the rule's correction: a missing file now stops the parse. A
    /// `require` that does not resolve has no fix (the right path is not known).
    fn fixes(&self, cx: &LintContext<'_>) -> Vec<FixedFinding> {
        let (Some(file), Some(source)) = (cx.file, cx.source) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        self.check(cx, &mut found);
        found
            .into_iter()
            .filter(|f| f.message.starts_with("`include "))
            .filter_map(|finding| {
                let st = file.statements.iter().find(|s| {
                    s.span == finding.loc.lines
                        && matches!(
                            s.kind,
                            StatementKind::Include {
                                mode: IncludeMode::Include,
                                ..
                            }
                        )
                })?;
                let text = source.get(st.bytes.clone())?;
                let rest = text.strip_prefix("include")?;
                if !rest.starts_with(fix::py_space) {
                    return None;
                }
                let at = st.bytes.start;
                let fix = Fix::in_file(
                    cx,
                    vec![Edit::replace(at..at + "include".len(), "require")],
                    "`include` becomes `require` (the file is found, so it is read the same way; \
                     a missing one now stops the parse)",
                    Safety::Safe,
                )?;
                Some(FixedFinding { finding, fix })
            })
            .collect()
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(file) = cx.file else { return };
        let Ok(Some(row)) = query::file_for_loc(cx.index, &cx.loc) else {
            return;
        };
        let Ok(edges) = query::edges_from(cx.index, row.id) else {
            return;
        };
        for st in &file.statements {
            let StatementKind::Include { path, mode } = &st.kind else {
                continue;
            };
            let kind = match mode {
                IncludeMode::Include => "include",
                IncludeMode::Require => "require",
                IncludeMode::IncludeAll => continue,
            };
            let here: Vec<&query::EdgeRow> = edges
                .iter()
                .filter(|e| e.kind == kind && e.line == Some(st.span.start))
                .collect();
            if here.is_empty() {
                continue;
            }
            match mode {
                IncludeMode::Require => {
                    for e in here.iter().filter(|e| e.target.is_none()) {
                        let t = e.target_text.as_str();
                        if t.contains("${") || (Path::new(t).is_absolute() && Path::new(t).exists())
                        {
                            continue;
                        }
                        out.push(helpers::finding(
                            self,
                            cx,
                            st.span,
                            format!(
                                "`require {path}` does not resolve: no {t} next to this file or \
                                 on BBPATH in the workspace, and BitBake stops with \
                                 \"Could not include required file\"; fix the path, or use \
                                 `include` if the file is optional"
                            ),
                        ));
                    }
                }
                IncludeMode::Include => {
                    if cx.kind == Some(FileKind::Conf) {
                        continue;
                    }
                    for e in &here {
                        let Some(target) = &e.target else { continue };
                        if let Some((why, at)) = essential(cx, target) {
                            out.push(helpers::finding(
                                self,
                                cx,
                                st.span,
                                format!(
                                    "`include {path}` pulls in a file this recipe needs ({why} \
                                     at {at}), but `include` silently skips a missing file; \
                                     write `require {path}`"
                                ),
                            ));
                            break;
                        }
                    }
                }
                IncludeMode::IncludeAll => {}
            }
        }
    }
}

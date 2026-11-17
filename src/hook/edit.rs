//! Hook handler run after Edit / Write / MultiEdit on a Yocto file (T7.4).
//!
//! For `tool_input.file_path` (always absolute) naming a BitBake file inside the
//! workspace:
//!
//! 1. **Reindex that file** with [`Index::refresh_file`] (no walk). The hook otherwise opens
//!    the index read-only; this handler opens the same existing database read-write with
//!    [`Index::open_existing`] and a [`WRITE_BUSY`] busy timeout, so a database locked by
//!    the MCP server's refresh makes the write fail quickly and the hook fail open (no
//!    output) instead of blowing the budget. A file the index does not hold (new layer,
//!    skipped directory) or that is gone gets no output.
//! 2. **Lint it** through the engine (`lint::run`, `Target::Files`) with the file-scope
//!    rules only: those enabled by default and not disabled in `[lint] disable`, selected by
//!    id so that no recipe- or layer-scope rule (evaluation, `git status`) runs. Findings
//!    at warning level or above, after `[lint.severity]` and suppression comments, are
//!    added as context: at most [`MAX_FINDINGS`], then `+N more`.
//! 3. If the file is in a layer that is not writable, one line says so and suggests a
//!    bbappend, with the placement call to run: `scaffold kind=where name=<pn>` when the
//!    file belongs to one recipe ([`recipe_of`]), else `scaffold kind=where` in general.
//!
//! No findings and a writable layer: no output. The context is an addition; the tool output
//! is never replaced.

use std::time::Duration;

use super::{Ctx, Event, Handler, POST_TOOL_USE, Response, is_yocto_path};
use crate::error::{Error, Result};
use crate::index::query;
use crate::index::{FileRefresh, Index};
use crate::lint::{self, Options, Scope, Severity, Target};
use crate::types::FileKind;
use crate::types::LineSpan;
use crate::workspace::BUILD_LAYER;
use crate::workspace::Workspace;

/// Findings listed before `+N more`.
pub const MAX_FINDINGS: usize = 5;

/// How long the hook waits for the write lock on the index database.
pub const WRITE_BUSY: Duration = Duration::from_millis(50);

/// `PostToolUse` on `Edit`, `Write` and `MultiEdit`.
pub struct EditFollowUp;

impl Handler for EditFollowUp {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn events(&self) -> &'static [&'static str] {
        &[POST_TOOL_USE]
    }

    fn tools(&self) -> &'static [&'static str] {
        &["Edit", "Write", "MultiEdit"]
    }

    fn handle(&self, cx: &mut Ctx<'_>, ev: &Event) -> Result<Option<Response>> {
        let Some(abs) = ev.input_str("file_path").and_then(|p| ev.abs_path(p)) else {
            return Ok(None);
        };
        if !is_yocto_path(&abs) {
            return Ok(None);
        }
        cx.env()?;
        let config = cx.config().cloned().ok_or_else(|| Error::Invalid {
            msg: "no configuration".into(),
        })?;
        let env = cx.env()?;
        let ws = env.ws;
        let Some(loc) = ws.loc_for(&abs, LineSpan::line(1)) else {
            return Ok(None);
        };
        if ws.confine(&abs).is_err() {
            return Ok(None);
        }

        let mut index = Index::open_existing(env.index.path(), WRITE_BUSY)?;
        match index.refresh_file(ws, &abs)? {
            FileRefresh::Reindexed(_) | FileRefresh::Unchanged => {}
            FileRefresh::Removed(_) | FileRefresh::NotIndexed => return Ok(None),
        }
        env.check_budget()?;

        let rules: Vec<String> = lint::all_rules()
            .iter()
            .filter(|r| {
                r.scope() == Scope::File
                    && r.default_enabled()
                    && !lint::is_disabled(&config, r.id())
            })
            .map(|r| r.id().to_string())
            .collect();
        let mut lines: Vec<String> = Vec::new();
        if !rules.is_empty() {
            let report = lint::run(
                ws,
                &index,
                &config,
                &Target::Files {
                    files: vec![abs.clone()],
                    lines: None,
                },
                &Options {
                    rules,
                    min_severity: Some(Severity::Warning),
                    all: false,
                },
            )?;
            let n = report.findings.len();
            if n > 0 {
                lines.push(format!(
                    "sous-chef lint of {}:{} after this change ({n} at warning or above):",
                    loc.layer,
                    loc.path.display()
                ));
                for f in report.findings.iter().take(MAX_FINDINGS) {
                    lines.push(format!(
                        "{} {} {} {}",
                        f.severity.as_str(),
                        f.rule,
                        f.loc,
                        f.message
                    ));
                }
                if n > MAX_FINDINGS {
                    lines.push(format!(
                        "+{} more (`lint {}:{}` lists them)",
                        n - MAX_FINDINGS,
                        loc.layer,
                        loc.path.display()
                    ));
                }
            }
        }
        let upstream =
            loc.layer != BUILD_LAYER && ws.layer(&loc.layer).is_some_and(|l| !l.writable);
        if upstream {
            let hint = match recipe_of(&index, ws, &abs)? {
                Some(pn) => format!("`scaffold kind=where name={pn}` says where"),
                None => "`scaffold kind=where` says where".to_string(),
            };
            lines.push(format!(
                "{} is not a writable layer: this edits upstream metadata, which an update of \
                 the layer overwrites; put the change in a bbappend in a writable layer \
                 ({hint}).",
                loc.layer
            ));
        }
        if lines.is_empty() {
            return Ok(None);
        }
        Ok(Some(Response::context(lines.join("\n"))))
    }
}

/// The one recipe (`PN`) the file at `abs` belongs to: a `.bb` itself, the recipes a
/// `.bbappend` applies to, the recipes that `require` / `include` / `include_all` an
/// `.inc` directly. `None` when there is no such recipe or there are several names, or a
/// recipe has no `PN` in the index.
pub fn recipe_of(index: &Index, ws: &Workspace, abs: &std::path::Path) -> Result<Option<String>> {
    let Some(file) = query::file_by_abs(index, ws, abs)? else {
        return Ok(None);
    };
    let recipes: Vec<query::FileRow> = match file.kind {
        FileKind::Recipe => vec![file],
        FileKind::Append => query::edges_from(index, file.id)?
            .into_iter()
            .filter(|e| e.kind == "bbappend")
            .filter_map(|e| e.target)
            .collect(),
        FileKind::Include => query::edges_to(index, file.id)?
            .into_iter()
            .filter(|e| matches!(e.kind.as_str(), "require" | "include" | "include_all"))
            .map(|e| e.src)
            .filter(|f| f.kind == FileKind::Recipe)
            .collect(),
        _ => Vec::new(),
    };
    let mut names: Vec<String> = Vec::new();
    for r in recipes {
        let Some(pn) = r.pn else { return Ok(None) };
        if !names.contains(&pn) {
            names.push(pn);
        }
    }
    Ok(match names.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    })
}

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
//!    bbappend (`scaffold kind=where`).
//!
//! No findings and a writable layer: no output. The context is an addition; the tool output
//! is never replaced.

use std::time::Duration;

use super::{Ctx, Event, Handler, POST_TOOL_USE, Response, is_yocto_path};
use crate::error::{Error, Result};
use crate::index::{FileRefresh, Index};
use crate::lint::{self, Options, Scope, Severity, Target};
use crate::types::LineSpan;
use crate::workspace::BUILD_LAYER;

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
            lines.push(format!(
                "{} is not a writable layer: this edits upstream metadata, which an update of \
                 the layer overwrites; put the change in a bbappend in a writable layer \
                 (`scaffold kind=where` says where).",
                loc.layer
            ));
        }
        if lines.is_empty() {
            return Ok(None);
        }
        Ok(Some(Response::context(lines.join("\n"))))
    }
}

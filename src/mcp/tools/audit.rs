//! Tool `audit`: the whole-repository audit report (T8.6). See docs/mcp-tools.md.
//!
//! Builds the report with [`crate::audit::build`] (each of its stage lines is an MCP
//! progress step when the client sent a progress token: [`stage_progress`]), stores the full Markdown under a `text`
//! handle and returns a digest within the budget: the summary line and the workspace rows,
//! one row per section with its summary, then the items most worth attention, and the
//! handle with the `slice` hint. The report itself is not subject to the budget.
//!
//! `root` may not widen what the server reads: it must be the server's working directory,
//! or a directory inside the span of the workspace ([`workspace_span`]: the deepest
//! directory holding the build directory, every enabled layer and the kas project root),
//! never the filesystem root. The default is the working directory when that holds the
//! build directory or an enabled layer, else the span.

use std::fs;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Deserialize;

use crate::audit::{self, Audit};
use crate::error::{Error, Result};
use crate::handles::HandleKind;
use crate::mcp::Ctx;
use crate::render::{Format, Table};
use crate::workspace::Workspace;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// The repository root to audit: the working directory (default) or a directory inside
    /// the workspace's span (relative to the working directory).
    pub root: Option<String>,
    /// Digest budget in tokens (default 600). The stored report is not cut.
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<String> {
    let root = resolve_root(ctx, args.root.as_deref())?;
    let opts = audit::Options {
        progress: Some(stage_progress),
        lint_progress: Some(lint_file_progress),
        ..audit::Options::new(&root)
    };
    let build = audit::build(ctx, &opts);
    // The lint run inside is synchronous and takes seconds; on the server's multi-threaded
    // runtime, `block_in_place` hands this worker's queue to another thread meanwhile, so
    // the progress forwarder and rmcp's service loop keep running and notifications go out
    // while the audit runs, not after it. (A current-thread runtime, as in tests, cannot do
    // that and just awaits.)
    let report = match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| h.block_on(build))?
        }
        _ => build.await?,
    };
    let md = audit::render_markdown(&report);
    let label = format!(
        "audit {}",
        root.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".".into())
    );
    let handle = ctx.handles.put(HandleKind::Text, &label, md.as_bytes())?;
    let budget = match args
        .budget_tokens
        .unwrap_or(ctx.config.output.budget_tokens)
    {
        0 => crate::render::DEFAULT_BUDGET_TOKENS,
        n => n,
    };
    let lines = md.lines().count();
    let t = digest(&report, &handle.0, lines, md.len());
    Ok(t.render_as(args.format.unwrap_or_default(), budget))
}

/// `audit::Options::progress`: each stage line of the audit (refreshing the index, layer
/// discovery and git facts, lint, lint done, parse problems) is one MCP progress step.
fn stage_progress(msg: &str) {
    crate::mcp::progress::step(&format!("audit: {msg}"));
}

/// `audit::Options::lint_progress`: the lint stage's files as `done` of `total` progress
/// (values at or below the stage steps already sent are dropped, so progress still only
/// increases; the step after lint continues from the file count).
fn lint_file_progress(done: usize, total: usize) {
    if crate::mcp::progress::active() {
        crate::mcp::progress::of(
            done as u64,
            total as u64,
            &format!("audit: linting, {done} of {total} files"),
        );
    }
}

/// The digest table: `field value` rows, most important first, so a small budget keeps
/// the summary and the sections and drops attention items from the end.
pub fn digest(a: &Audit, handle: &str, lines: usize, bytes: usize) -> Table {
    let mut t = Table::new(&["field", "value"]);
    let s = &a.summary;
    t.row(&["summary", &s.line]);
    let mut ws = format!("root .; release {} ({})", s.release, s.release_source);
    if let Some(b) = &s.build_dir {
        ws.push_str(&format!("; build {b}"));
    }
    if !s.kas_files.is_empty() {
        ws.push_str(&format!("; kas {}", s.kas_files.join(":")));
    }
    t.row(&["workspace", &ws]);
    t.row(&["1 inventory", &a.inventory.summary]);
    t.row(&["2 releases", &a.releases.summary]);
    t.row(&["3 structure", &a.structure.summary]);
    t.row(&["4 lint", &a.lint.summary]);
    t.row(&["5 parse", &a.parse.summary]);
    t.row(&["6 not checked", &a.not_checked.summary]);
    for (i, x) in s.attention.iter().enumerate() {
        t.row(&[
            &format!("attention {}", i + 1),
            &format!(
                "{} {} x{} ({}): {} {}",
                x.severity,
                x.what,
                x.count,
                x.layers.join(", "),
                x.pointer,
                x.message
            ),
        ]);
    }
    t.note(&format!(
        "report {handle}: {lines} lines, {bytes} bytes of Markdown; attention items are chosen by: {}",
        "errors in own layers, then layer structure errors, then warnings in own layers"
    ));
    t.note(&format!(
        "slice handle={handle} page=1 | grep=RE | lines=A-B"
    ));
    t
}

/// The deepest directory containing the build directory, every enabled layer root and the
/// kas project root; `None` when that is the filesystem root.
pub fn workspace_span(ws: &Workspace) -> Option<PathBuf> {
    let mut dirs: Vec<&Path> = ws.layers.iter().map(|l| l.root.as_path()).collect();
    if let Some(b) = &ws.build_dir {
        dirs.push(b);
    }
    if let Some(k) = &ws.kas {
        dirs.push(&k.root);
    }
    let mut span = dirs.first()?.to_path_buf();
    for d in &dirs[1..] {
        while !d.starts_with(&span) {
            if !span.pop() {
                return None;
            }
        }
    }
    let span = fs::canonicalize(&span).unwrap_or(span);
    (span.parent().is_some()).then_some(span)
}

/// The audit root for `arg` (see the module documentation).
pub fn resolve_root(ctx: &Ctx, arg: Option<&str>) -> Result<PathBuf> {
    let work = fs::canonicalize(&ctx.config.work_dir)?;
    let span = workspace_span(&ctx.workspace);
    let holds_workspace = |d: &Path| {
        ctx.workspace.layers.iter().any(|l| l.root.starts_with(d))
            || ctx
                .workspace
                .build_dir
                .as_ref()
                .is_some_and(|b| b.starts_with(d))
    };
    let Some(arg) = arg.filter(|a| !a.trim().is_empty()) else {
        return Ok(match &span {
            Some(s) if !holds_workspace(&work) => s.clone(),
            _ => work,
        });
    };
    let candidate = work.join(arg);
    let resolved = fs::canonicalize(&candidate).map_err(|e| Error::Invalid {
        msg: format!("root {arg:?}: {e}; give an existing directory"),
    })?;
    let inside_span = span.as_ref().is_some_and(|s| resolved.starts_with(s));
    if !resolved.is_dir() || resolved.parent().is_none() || !(resolved == work || inside_span) {
        return Err(Error::Invalid {
            msg: format!(
                "root {arg:?} is outside what the audit may read: give the working directory or a directory inside {}",
                span.as_ref()
                    .map_or_else(|| work.display().to_string(), |s| s.display().to_string())
            ),
        });
    }
    Ok(resolved)
}

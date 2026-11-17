//! Tool `scaffold`: create recipes, bbappends, layers, classes and `.inc` splits, or advise where a change belongs. See docs/mcp-tools.md.
//!
//! The engine is `crate::scaffold`; this file turns its [`Outcome`] into output.
//!
//! Dry run (text): a table `action path lines` (one row per planned file, `path` as
//! `layer:path`), the plan notes, then each file's full content after a
//! `--- <action> <path>` line. When the whole output would pass [`CONTENT_CAP_BYTES`], the
//! contents go to a `text` handle instead and a note names it.
//!
//! Write (text): the same table with a `result` column, notes on the write, the index
//! refresh and the lint run, then a findings table `severity rule loc message` if lint found
//! anything. `format: "json"` returns the same data as one JSON object ([`JsonOutput`]).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::handles::HandleKind;
use crate::lint::Finding;
use crate::mcp::Ctx;
use crate::render::{self, Format, JsonTable, Table};
use crate::scaffold::{self, Generator, Outcome, PostWriteLint, Request, RuleLint};

pub use crate::scaffold::Kind;

/// Cap on the whole dry-run output, header and contents included.
pub const CONTENT_CAP_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// `recipe`, `bbappend`, `layer`, `class`, `inc_split` or `where`.
    pub kind: Kind,
    /// Target layer (must be writable).
    pub layer: Option<String>,
    /// Name, as relevant to `kind`.
    pub name: Option<String>,
    /// Version, as relevant to `kind`.
    pub version: Option<String>,
    /// Kind-specific options (source URL, build system, variables, files...).
    #[serde(default)]
    pub options: serde_json::Map<String, serde_json::Value>,
    /// Write the files (default false: dry run returning paths and content).
    #[serde(default)]
    pub write: bool,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<String> {
    run_with(ctx, args, None, &RuleLint).await
}

/// [`run`] with the generator and the post-write lint injectable (tests). `generator: None`
/// dispatches on `args.kind`.
pub async fn run_with(
    ctx: &Ctx,
    args: Args,
    generator: Option<&dyn Generator>,
    lint: &dyn PostWriteLint,
) -> Result<String> {
    let request = Request {
        kind: args.kind,
        layer: args.layer.clone(),
        name: args.name.clone(),
        version: args.version.clone(),
        options: args.options.clone(),
    };
    let outcome = {
        let mut index = ctx.index.lock().await;
        scaffold::execute(
            &ctx.workspace,
            &ctx.config,
            &mut index,
            &request,
            args.write,
            generator,
            lint,
        )?
    };
    let budget = args
        .budget_tokens
        .unwrap_or(ctx.config.output.budget_tokens);
    Ok(render_outcome(
        ctx,
        &request,
        &outcome,
        args.format.unwrap_or_default(),
        budget,
    ))
}

/// One file's content in JSON output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonContent {
    pub path: String,
    pub action: String,
    pub content: String,
}

/// `format: "json"` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonOutput {
    pub kind: String,
    /// True when the files were written.
    pub written: bool,
    /// The planned files, with the notes.
    pub files: JsonTable,
    /// Dry run: each file's content, unless over the cap (then see `content_handle`).
    pub contents: Vec<JsonContent>,
    pub content_handle: Option<String>,
    /// After a write: the lint findings on the written files.
    pub lint: Option<JsonTable>,
}

fn render_outcome(
    ctx: &Ctx,
    request: &Request,
    o: &Outcome,
    format: Format,
    budget: usize,
) -> String {
    let mut table = if o.written {
        Table::new(&["action", "path", "lines", "result"])
    } else {
        Table::new(&["action", "path", "lines"])
    };
    for c in &o.checked {
        let Some(f) = o.plan.files.get(c.index) else {
            continue;
        };
        let lines = scaffold::line_count(&f.content).to_string();
        if o.written {
            table.row(&[f.action.as_str(), &c.display, &lines, "written"]);
        } else {
            table.row(&[f.action.as_str(), &c.display, &lines]);
        }
    }

    let n = o.checked.len();
    if o.written {
        table.note(&format!("wrote {n} {}", plural(n, "file")));
    } else if n > 0 {
        table.note(&format!(
            "dry run: nothing written; call again with write=true to create {n} {}",
            plural(n, "file")
        ));
    }
    for note in &o.plan.notes {
        table.note(&format!("note: {note}"));
    }
    match &o.refresh {
        Some(Ok(r)) => {
            if let Some(line) = render::freshness_line(r) {
                table.note(&line);
            }
        }
        Some(Err(e)) => table.note(&format!(
            "note: index refresh failed ({e}); the files are written, the index catches up on a later refresh"
        )),
        None => {}
    }
    let findings = match &o.lint {
        Some(Ok(f)) => {
            table.note(&format!(
                "lint: {} {} on the {n} written {}",
                f.len(),
                plural(f.len(), "finding"),
                plural(n, "file")
            ));
            Some(f.as_slice())
        }
        Some(Err(e)) => {
            table.note(&format!("note: lint failed on the written files ({e})"));
            None
        }
        None => None,
    };
    let lint_table = findings.filter(|f| !f.is_empty()).map(findings_table);

    // Dry-run contents, capped.
    let mut contents: Vec<JsonContent> = Vec::new();
    let mut content_handle = None;
    if !o.written {
        for c in &o.checked {
            if let Some(f) = o.plan.files.get(c.index) {
                contents.push(JsonContent {
                    path: c.display.clone(),
                    action: f.action.as_str().to_string(),
                    content: f.content.clone(),
                });
            }
        }
        let text = contents_text(&contents);
        if !contents.is_empty() && table.render(budget).len() + text.len() > CONTENT_CAP_BYTES {
            let command = format!(
                "scaffold {} {}",
                request.kind.as_str(),
                request.name.as_deref().unwrap_or("")
            );
            let full = format!("{}{text}", table.render(usize::MAX / 8));
            match ctx
                .handles
                .put(HandleKind::Text, command.trim_end(), full.as_bytes())
            {
                Ok(h) => {
                    table.note(&format!(
                        "content ({} bytes) is over the 8 KB cap: stored as {h}; read it with slice handle={h} page=1",
                        text.len(),
                        h = h.0
                    ));
                    content_handle = Some(h.0);
                }
                Err(e) => table.note(&format!(
                    "content ({} bytes) is over the 8 KB cap and could not be stored ({e}); re-run with fewer files",
                    text.len()
                )),
            }
            contents.clear();
        }
    }

    match format {
        Format::Json => {
            let out = JsonOutput {
                kind: o.kind.as_str().to_string(),
                written: o.written,
                files: table.to_json(budget),
                contents,
                content_handle,
                lint: lint_table.map(|t| t.to_json(budget)),
            };
            serde_json::to_string(&out).unwrap_or_else(|_| "{}".into())
        }
        Format::Text => {
            let mut out = table.render(budget);
            if let Some(t) = lint_table {
                out.push('\n');
                out.push_str(&t.render(budget));
            }
            out.push_str(&contents_text(&contents));
            out
        }
    }
}

fn findings_table(findings: &[Finding]) -> Table {
    let mut t = Table::new(&["severity", "rule", "loc", "message"]);
    for f in findings {
        let sev = match f.severity {
            crate::lint::Severity::Info => "info",
            crate::lint::Severity::Warning => "warning",
            crate::lint::Severity::Error => "error",
        };
        t.row(&[sev, f.rule, &f.loc.to_string(), &f.message]);
    }
    t
}

/// `\n--- <action> <path>\n<content>` per file, each content ending in a newline.
fn contents_text(contents: &[JsonContent]) -> String {
    let mut out = String::new();
    for c in contents {
        out.push_str(&format!("\n--- {} {}\n", c.action, c.path));
        out.push_str(&c.content);
        if !c.content.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}

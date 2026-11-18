//! Tool `lint`: lint a recipe, file or layer against Yocto conventions. See docs/mcp-tools.md.
//!
//! A thin wrapper over [`lint::run`]: refresh the index, resolve the target, run, render with
//! [`lint::render_report`] (summary line first: counts by severity and by rule, so a table
//! cut by the budget still tells the whole story). Also generates the `sous-chef://rules`
//! resource ([`rules_resource`]).

use schemars::JsonSchema;
use serde::Deserialize;

use crate::config::Config;
use crate::error::Result;
use crate::lint::{self, Options, Severity};
use crate::mcp::{Ctx, progress};
use crate::release::Release;
use crate::render::{self, Format};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// A recipe, a file pointer or a layer name; default: writable layers.
    pub target: Option<String>,
    /// Rule ids or groups to run; default: all enabled rules.
    pub rules: Option<Vec<String>>,
    /// Minimum severity: `info`, `warning` or `error`.
    pub severity: Option<Severity>,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
    /// Fix what can be fixed safely; a dry run (a diff per file) unless `write`.
    pub fix: Option<bool>,
    /// Also apply suggested fixes (they change meaning as the rule recommends); implies `fix`.
    pub fix_suggested: Option<bool>,
    /// With `fix`: write the fixes into the writable layers, then lint the changed files.
    pub write: Option<bool>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<String> {
    if args.fix.unwrap_or(false) || args.fix_suggested.unwrap_or(false) || args.write.is_some() {
        return run_fix(ctx, args).await;
    }
    let mut index = ctx.index.lock().await;
    let refresh = index.refresh(&ctx.workspace)?;
    let target = lint::resolve_target(
        &ctx.workspace,
        &index,
        args.target.as_deref(),
        &ctx.config.work_dir,
    )?;
    let options = Options {
        rules: args.rules.clone().unwrap_or_default(),
        min_severity: args.severity,
        all: false,
    };
    let mut report = progress::blocking(|| {
        lint::run_progress(
            &ctx.workspace,
            &index,
            &ctx.config,
            &target,
            &options,
            &file_progress,
        )
    })?;
    drop(index);
    if let Some(line) = render::freshness_line(&refresh) {
        report.notes.insert(0, line);
    }
    let budget = args
        .budget_tokens
        .unwrap_or(ctx.config.output.budget_tokens);
    Ok(lint::render_report(
        &report,
        args.format.unwrap_or_default(),
        budget,
    ))
}

/// `fix: true` (or `fix_suggested`): [`lint::fix::run`] on the target, rendered as text
/// ([`lint::fix::render_text`], cut at the budget on a line boundary with a note) or JSON
/// (the whole [`lint::fix::FixReport`]). `write` without `fix` is an error.
async fn run_fix(ctx: &Ctx, args: Args) -> Result<String> {
    let suggested = args.fix_suggested.unwrap_or(false);
    let write = args.write.unwrap_or(false);
    if !(args.fix.unwrap_or(false) || suggested) {
        return Err(crate::error::Error::Invalid {
            msg: "`write` applies fixes: pass it with `fix: true` (or `fix_suggested: true`)"
                .into(),
        });
    }
    let mut index = ctx.index.lock().await;
    let refresh = index.refresh(&ctx.workspace)?;
    let target = lint::resolve_target(
        &ctx.workspace,
        &index,
        args.target.as_deref(),
        &ctx.config.work_dir,
    )?;
    let options = Options {
        rules: args.rules.clone().unwrap_or_default(),
        min_severity: args.severity,
        all: false,
    };
    let fix_options = lint::fix::FixOptions { suggested, write };
    let mut report = progress::blocking(|| {
        lint::fix::run(
            &ctx.workspace,
            &mut index,
            &ctx.config,
            &target,
            &options,
            &fix_options,
        )
    })?;
    drop(index);
    if let Some(line) = render::freshness_line(&refresh) {
        report.report.notes.insert(0, line);
    }
    if args.format.unwrap_or_default() == Format::Json {
        return Ok(serde_json::to_string(&report).unwrap_or_else(|_| "{}".into()));
    }
    let budget = args
        .budget_tokens
        .unwrap_or(ctx.config.output.budget_tokens);
    Ok(cut_to_budget(&lint::fix::render_text(&report), budget))
}

/// `text` cut at the last line that fits `budget_tokens`, with a note saying how much was
/// left out.
fn cut_to_budget(text: &str, budget_tokens: usize) -> String {
    if render::estimate_tokens(text) <= budget_tokens {
        return text.to_string();
    }
    let mut out = String::new();
    let mut lines = text.lines();
    for line in lines.by_ref() {
        if render::estimate_tokens(&out) + render::estimate_tokens(line) + 40 > budget_tokens
            && !out.is_empty()
        {
            let rest = 1 + lines.count();
            out.push_str(&format!(
                "note: output cut at the budget ({budget_tokens} tokens): {rest} more lines; \
                 raise budget_tokens, narrow the target, or run `sous-chef lint --fix`\n"
            ));
            return out;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The engine's per-file hook ([`lint::Progress`]): one MCP progress update `done` of
/// `total` files (a no-op without a progress token; the forwarder rate-limits).
pub fn file_progress(done: usize, total: usize) {
    if progress::active() {
        progress::of(
            done as u64,
            total as u64,
            &format!("lint: {done} of {total} files"),
        );
    }
}

/// `sous-chef://rules`: one row per rule of the registry, `id severity scope flags`, in
/// catalogue order. `severity` is the effective one (`[lint.severity]` applied when a
/// configuration is given); `flags` is the description from docs/lint-rules.md, prefixed
/// with `[releases A to B]` for a rule limited to some releases and `[off by default]` for a
/// rule that runs only when selected. The last line lists `[lint] disable`, if any.
pub fn rules_resource(config: Option<&Config>) -> String {
    const DOC: &str = include_str!("../../../docs/lint-rules.md");
    let mut descriptions = std::collections::HashMap::new();
    for line in DOC.lines() {
        let Some(rest) = line.strip_prefix("| `") else {
            continue;
        };
        let cells: Vec<&str> = rest.trim_end_matches('|').split(" | ").collect();
        if let [id, _sev, _scope, flags] = cells.as_slice() {
            descriptions.insert(id.trim_end_matches('`').to_string(), flags.trim());
        }
    }
    let mut out = String::from("id\tseverity\tscope\tflags\n");
    for r in lint::all_rules() {
        let severity = match config {
            Some(c) => lint::effective_severity(c, *r),
            None => r.default_severity(),
        };
        let mut flags = String::new();
        let range = r.releases();
        if *range.start() != Release::FLOOR || *range.end() != Release::NEWEST {
            flags.push_str(&format!(
                "[releases {} to {}] ",
                range.start().codename(),
                range.end().codename()
            ));
        }
        if !r.default_enabled() {
            flags.push_str("[off by default] ");
        }
        flags.push_str(descriptions.get(r.id()).copied().unwrap_or(""));
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            r.id(),
            severity.as_str(),
            r.scope().as_str(),
            flags.replace('\t', " ")
        ));
    }
    if let Some(cfg) = config
        && !cfg.lint.disable.is_empty()
    {
        out.push_str(&format!(
            "disabled in sous-chef.toml: {}\n",
            cfg.lint.disable.join(", ")
        ));
    }
    out
}

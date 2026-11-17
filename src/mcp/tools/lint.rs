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
use crate::mcp::Ctx;
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
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<String> {
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
    let mut report = lint::run(&ctx.workspace, &index, &ctx.config, &target, &options)?;
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

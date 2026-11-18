//! Tool `diff`: the release diff (T8.4). See docs/mcp-tools.md.
//!
//! Builds the diff with [`crate::diff::build`] and renders its table within the budget.
//! When the whole table is over the budget, the whole diff is stored under a `text` handle
//! in the format asked for (the table as tab-separated text for `text`; the
//! `sous-chef-diff/1` document, pretty-printed one field per line, for `json`), and the
//! answer is the table cut to the budget (summary rows and own bbappends first) with the
//! handle and the `slice` hint.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::diff::{self, Options};
use crate::error::Result;
use crate::handles::HandleKind;
use crate::mcp::Ctx;
use crate::render::{self, Format};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Layer name; its git repository is compared.
    pub layer: String,
    /// Old revision (branch, tag or commit).
    pub from: String,
    /// New revision (default: the working tree).
    pub to: Option<String>,
    /// One recipe (default: those your writable layers append or use).
    pub recipe: Option<String>,
    /// Every recipe of the layer.
    pub all: Option<bool>,
    /// Output budget in tokens (default 600; more is stored under a handle).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<String> {
    let opts = Options {
        layer: args.layer.clone(),
        from: args.from.clone(),
        to: args.to.clone(),
        recipe: args.recipe.clone(),
        all: args.all.unwrap_or(false),
    };
    let d = diff::build(ctx, &opts).await?;
    let budget = match args
        .budget_tokens
        .unwrap_or(ctx.config.output.budget_tokens)
    {
        0 => render::DEFAULT_BUDGET_TOKENS,
        n => n,
    };
    let format = args.format.unwrap_or_default();
    let mut t = diff::table(&d);
    let full = t.render(usize::MAX / 8);
    if render::estimate_tokens(&full) <= budget {
        return Ok(t.render_as(format, budget));
    }
    let label = format!(
        "diff {} {}..{}",
        d.layer,
        d.from.given,
        if d.to.uncommitted.is_some() {
            "worktree"
        } else {
            d.to.given.as_str()
        }
    );
    let (stored, what) = match format {
        Format::Text => (full, "tab-separated, as above"),
        Format::Json => (
            diff::render_json(&d),
            "JSON, schema sous-chef-diff/1, one field per line",
        ),
    };
    let handle = ctx
        .handles
        .put(HandleKind::Text, &label, stored.as_bytes())?;
    t.note(&format!(
        "full diff {}: {} rows, {} bytes ({what})",
        handle.0,
        t.len(),
        stored.len()
    ));
    t.note(&format!(
        "slice handle={} page=1 | grep=RE | lines=A-B",
        handle.0
    ));
    Ok(t.render_as(format, budget))
}

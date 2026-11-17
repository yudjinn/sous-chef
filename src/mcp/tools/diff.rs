//! Tool `diff`: the release diff (T8.4). See docs/mcp-tools.md.
//!
//! Builds the diff with [`crate::diff::build`] and renders its table within the budget.
//! When the whole table is over the budget, it is stored under a `text` handle and the
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
    /// The layer to compare (a layer name of the workspace); its git repository is the one
    /// holding the layer's directory.
    pub layer: String,
    /// The old revision: a branch, tag or commit of that repository.
    pub from: String,
    /// The new revision (default: the working tree, HEAD plus uncommitted changes).
    pub to: Option<String>,
    /// One recipe (PN). Default: the recipes your writable layers append or use.
    pub recipe: Option<String>,
    /// Every recipe of the layer (unchanged ones counted, not listed).
    pub all: Option<bool>,
    /// Output budget in tokens (default 600); a longer diff is stored under a handle.
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
    let handle = ctx.handles.put(HandleKind::Text, &label, full.as_bytes())?;
    t.note(&format!(
        "full diff {}: {} rows, {} bytes (tab-separated, as above)",
        handle.0,
        t.len(),
        full.len()
    ));
    t.note(&format!(
        "slice handle={} page=1 | grep=RE | lines=A-B",
        handle.0
    ));
    Ok(t.render_as(format, budget))
}

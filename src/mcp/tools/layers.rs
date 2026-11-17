//! Tool `layers`: layers, shadowed recipes and classes, bbappends. See docs/mcp-tools.md.
//!
//! - `list`: one row per indexed layer (the build directory pseudo-layer included) with
//!   priority, collections, `LAYERSERIES_COMPAT`, `LAYERDEPENDS`, writable, and file counts
//!   by kind. A note gives the workspace release.
//! - `shadowed`: one row per file of every recipe or class file name that more than one
//!   layer provides, the winner first.
//! - `appends`: one row per bbappend and recipe it applies to (in application order for
//!   each recipe); a bbappend that matches nothing is a row with `matches` = `-` and status
//!   `no-match`.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

use super::recipe::file_label;
use crate::error::Result;
use crate::index::Index;
use crate::index::query::{self, FileRow};
use crate::mcp::Ctx;
use crate::render::{self, Format, Table};
use crate::types::FileKind;

/// What to show.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Show {
    /// Name, priority, collections, compat, depends, writable, file counts.
    #[default]
    List,
    /// Recipes or classes provided by more than one layer, and which wins.
    Shadowed,
    /// bbappends and what they match, flagging those that match nothing.
    Appends,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// `list` (default), `shadowed` or `appends`.
    #[serde(default)]
    pub show: Show,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<String> {
    let mut index = ctx.index.lock().await;
    let report = index.refresh(&ctx.workspace)?;
    let mut t = match args.show {
        Show::List => list(&index, ctx)?,
        Show::Shadowed => shadowed(&index, ctx)?,
        Show::Appends => appends(&index)?,
    };
    if let Some(line) = render::freshness_line(&report) {
        t.note(&line);
    }
    let budget = args
        .budget_tokens
        .unwrap_or(ctx.config.output.budget_tokens);
    Ok(t.render_as(args.format.unwrap_or_default(), budget))
}

fn words(v: &[String]) -> String {
    if v.is_empty() {
        "-".into()
    } else {
        v.join(" ")
    }
}

fn list(index: &Index, ctx: &Ctx) -> Result<Table> {
    let mut t = Table::new(&[
        "layer",
        "priority",
        "collections",
        "compat",
        "depends",
        "writable",
        "bb",
        "bbappend",
        "inc",
        "bbclass",
        "conf",
    ]);
    for s in query::layer_stats(index)? {
        t.row(&[
            &s.name,
            &s.priority.to_string(),
            &words(&s.collections),
            &words(&s.compat),
            &words(&s.depends),
            if s.writable { "yes" } else { "no" },
            &s.recipes.to_string(),
            &s.appends.to_string(),
            &s.includes.to_string(),
            &s.classes.to_string(),
            &s.confs.to_string(),
        ]);
    }
    t.note(&format!("release: {}", ctx.workspace.release.describe()));
    Ok(t)
}

fn shadowed(index: &Index, ctx: &Ctx) -> Result<Table> {
    let mut t = Table::new(&["kind", "name", "status", "layer", "priority", "file"]);
    for row in query::shadowed(index, &ctx.workspace)? {
        for (i, f) in row.files.iter().enumerate() {
            t.row(&[
                &row.kind,
                &row.name,
                if i == 0 { "wins" } else { "shadowed" },
                &f.layer,
                &f.priority.to_string(),
                &file_label(f),
            ]);
        }
    }
    if t.is_empty() {
        t.note("no recipe or class file is provided by more than one layer");
    } else {
        t.note(
            "note: recipes: highest layer priority wins; classes: BitBake's class search order \
             for a recipe inherit (classes-recipe/ along BBPATH, then classes/)",
        );
    }
    Ok(t)
}

fn appends(index: &Index) -> Result<Table> {
    let mut t = Table::new(&["bbappend", "priority", "matches", "status"]);
    let mut orphans = 0;
    for a in append_files(index)? {
        let edges: Vec<_> = query::edges_from(index, a.id)?
            .into_iter()
            .filter(|e| e.kind == "bbappend")
            .collect();
        let matched: Vec<FileRow> = edges.iter().filter_map(|e| e.target.clone()).collect();
        if matched.is_empty() {
            orphans += 1;
            t.row(&[&file_label(&a), &a.priority.to_string(), "-", "no-match"]);
            continue;
        }
        for r in matched {
            t.row(&[
                &file_label(&a),
                &a.priority.to_string(),
                &file_label(&r),
                "applies",
            ]);
        }
    }
    if orphans > 0 {
        t.note(&format!(
            "note: {orphans} {} no recipe; BitBake stops with a fatal error on these \
             (cooker.py show_appends_with_no_recipes)",
            if orphans == 1 {
                "bbappend matches"
            } else {
                "bbappends match"
            }
        ));
    }
    Ok(t)
}

/// Private query (candidate for `index::query`): every bbappend, by layer row id then path.
fn append_files(index: &Index) -> Result<Vec<FileRow>> {
    let mut st = index.conn().prepare(
        "SELECT f.id, l.name, f.layer_id, f.path, f.sha256, f.pn, f.pv, l.priority \
         FROM files f JOIN layers l ON l.id = f.layer_id WHERE f.kind = 'bbappend' \
         ORDER BY l.id, f.path",
    )?;
    let rows = st.query_map([], |r| {
        Ok(FileRow {
            id: r.get(0)?,
            layer: r.get(1)?,
            layer_id: r.get(2)?,
            path: PathBuf::from(r.get::<_, String>(3)?),
            kind: FileKind::Append,
            sha256: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            pn: r.get(5)?,
            pv: r.get(6)?,
            priority: r.get::<_, Option<i32>>(7)?.unwrap_or(0),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

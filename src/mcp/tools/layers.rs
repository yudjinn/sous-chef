//! Tool `layers`: layers, shadowed recipes and classes, bbappends. See docs/mcp-tools.md.
//!
//! - `list`: one row per indexed layer (the build directory pseudo-layer included) with
//!   priority, collections, `LAYERSERIES_COMPAT`, `LAYERDEPENDS`, writable, and file counts
//!   by kind. A note gives the workspace release.
//! - `shadowed`: one row per file of every recipe or class file name that more than one
//!   layer provides, the winner first.
//! - `appends`: one row per bbappend and recipe it applies to (in application order for
//!   each recipe); a bbappend that `BBFILES` collects and that matches nothing is a row with
//!   `matches` = `-` and status `no-match` (the same set as `query::orphan_appends`, which
//!   `org/append-no-match` reports). What BitBake collects comes from the index
//!   (`files.not_collected`, the `bbappend` edges), which folds the active `BBFILES_DYNAMIC`
//!   globs into `BBFILES` (`resolve::collect`): a bbappend only an active entry collects has
//!   `(BBFILES_DYNAMIC)` after its status; one only an inactive entry matches is
//!   `not collected (dynamic layer ... not present)` (or `!<c>`: `... is present`); one no
//!   glob matches is `not collected (no BBFILES glob)`, one `BBMASK` masks
//!   `not collected (masked by BBMASK)`.
//! - `compat`: per layer in `BBLAYERS` order, the `LAYERSERIES_COMPAT` it claims against the
//!   workspace release and the core layer's `LAYERSERIES_CORENAMES`, then the problems
//!   `org/layer-conf` finds in its compat series and `LAYERDEPENDS`
//!   ([`org_layer_conf::layer_problems`], the same checks, so the view and the rule agree),
//!   then `BBLAYERS` order rows: a layer listed before a layer it depends on. BitBake does
//!   not require dependencies first (`LAYERDEPENDS` is checked against every enabled layer
//!   and priority does not come from the order, `cooker.py` `handleCollections`); the order
//!   matters because `BBPATH` follows it when the layer confs append to it, and `BBPATH`
//!   decides which file an `include` / `require`, an `inherit` or a conf lookup finds. The
//!   row says which files that changes.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Deserialize;

use super::recipe::file_label;
use crate::error::Result;
use crate::index::Index;
use crate::index::query::{self, FileRow, StatementFilter};
use crate::index::schema::{NOT_COLLECTED_BBMASK, NOT_COLLECTED_DYNAMIC};
use crate::lint::Severity;
use crate::lint::rules::org_layer_conf::{self, LayerCheck, LayerProblem};
use crate::mcp::Ctx;
use crate::render::{self, Format, Table};
use crate::types::{FileKind, LineSpan, Loc};
use crate::workspace::{BUILD_LAYER, Layer, Workspace, resolve};

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
    /// Per layer: the compat series it claims, missing `LAYERDEPENDS`, `BBLAYERS` order.
    Compat,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// `list` (default), `shadowed`, `appends` or `compat`.
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
        Show::Appends => appends(&index, &ctx.workspace)?,
        Show::Compat => compat(&index, &ctx.workspace)?,
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

// ---- show=compat ----

/// The checks of `org/layer-conf` this view shows.
fn compat_check(p: &LayerProblem) -> bool {
    matches!(p.check.group(), "compat" | "depends") || p.check == LayerCheck::NoCollections
}

/// The worst severity of `problems`, as the `result` cell; `ok` for none.
fn worst(problems: &[&LayerProblem]) -> &'static str {
    problems
        .iter()
        .map(|p| p.severity)
        .max()
        .map_or("ok", Severity::as_str)
}

fn compat(index: &Index, ws: &Workspace) -> Result<Table> {
    let mut t = Table::new(&["layer", "check", "result", "detail", "loc"]);
    let corenames = org_layer_conf::core_names(ws);
    let release = ws.release.release.codename();
    let mut counts = [0usize; 3];
    for layer in &ws.layers {
        let problems = org_layer_conf::layer_problems(ws, index, &layer.name);
        let shown: Vec<&LayerProblem> = problems.iter().filter(|p| compat_check(p)).collect();
        let compat_problems: Vec<&LayerProblem> = shown
            .iter()
            .copied()
            .filter(|p| p.check.group() == "compat")
            .collect();
        let (detail, loc) = compat_summary(index, layer, release, &corenames)?;
        t.row(&[
            &layer.name,
            "compat",
            worst(&compat_problems),
            &detail,
            &loc,
        ]);
        for p in &shown {
            counts[p.severity as usize] += 1;
            let check = match p.check.group() {
                "collections" => "compat",
                g => g,
            };
            t.row(&[
                &layer.name,
                check,
                p.severity.as_str(),
                &p.message,
                &p.loc.to_string(),
            ]);
        }
    }
    let order = order_problems(index, ws)?;
    for o in &order {
        counts[o.severity as usize] += 1;
        t.row(&[&o.layer, "order", o.severity.as_str(), &o.message, &o.loc]);
    }
    let [info, warning, error] = counts;
    t.note(&format!(
        "note: {error} error, {warning} warning, {info} info; the compat and depends checks are org/layer-conf's (lint shows the same findings)"
    ));
    t.note(&format!("release: {}", ws.release.describe()));
    if ws.build_dir.is_none() {
        t.note("note: no build directory, so no BBLAYERS order to check; layers are in discovery order");
    }
    Ok(t)
}

/// The `compat` row of a layer: what `LAYERSERIES_COMPAT_<c>` claims against the
/// workspace release and the core layer's `LAYERSERIES_CORENAMES`, and where it is set.
fn compat_summary(
    index: &Index,
    layer: &Layer,
    release: &str,
    corenames: &[String],
) -> Result<(String, String)> {
    let conf = Loc {
        layer: layer.name.clone(),
        path: PathBuf::from("conf/layer.conf"),
        lines: LineSpan::line(1),
    };
    let Some(c) = layer.collections.first() else {
        return Ok((
            "no BBFILE_COLLECTIONS, so BitBake checks no compat series".into(),
            conf.to_string(),
        ));
    };
    let var = format!("LAYERSERIES_COMPAT_{c}");
    let stmt = match query::file_by_path(index, &layer.name, Path::new("conf/layer.conf"))? {
        Some(f) => query::statements_in(index, f.id, &StatementFilter::default())?
            .into_iter()
            .find(|s| s.base.as_deref() == Some(var.as_str())),
        None => None,
    };
    let loc = stmt
        .as_ref()
        .map_or_else(|| conf.to_string(), |s| s.loc.to_string());
    if let Some(raw) = stmt
        .as_ref()
        .and_then(|s| s.value.as_deref())
        .filter(|v| v.contains("${LAYERSERIES_COMPAT_"))
    {
        // The workspace reader expands the copy; BitBake does not (cookerdata.py deletes
        // each layer's LAYERSERIES_COMPAT_<c> right after its layer.conf).
        return Ok((
            format!(
                "{var} = \"{raw}\" copies another layer's value; BitBake deletes each layer's \
                 LAYERSERIES_COMPAT after reading its layer.conf, so the reference is left \
                 unexpanded and names no release"
            ),
            loc,
        ));
    }
    if layer.compat.is_empty() {
        return Ok((format!("{var} is not set"), loc));
    }
    let claimed = layer.compat.join(" ");
    let names_release = if layer.compat.iter().any(|w| w == release) {
        format!("names {release} (the workspace release)")
    } else {
        format!("does not name {release} (the workspace release)")
    };
    let core = if corenames.is_empty() {
        "no layer sets LAYERSERIES_CORENAMES".to_string()
    } else {
        let shared: Vec<&str> = layer
            .compat
            .iter()
            .filter(|w| corenames.contains(w))
            .map(String::as_str)
            .collect();
        if shared.is_empty() {
            format!(
                "shares no name with LAYERSERIES_CORENAMES \"{}\"",
                corenames.join(" ")
            )
        } else {
            format!(
                "shares {} with LAYERSERIES_CORENAMES \"{}\"",
                shared.join(" "),
                corenames.join(" ")
            )
        }
    };
    Ok((
        format!("{var} = \"{claimed}\": {names_release}; {core}"),
        loc,
    ))
}

/// A `BBLAYERS` order row of [`order_problems`]: `layer` is listed before a layer it names
/// in `LAYERDEPENDS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderProblem {
    /// The layer listed too early.
    pub layer: String,
    /// Warning when a `BBPATH` lookup finds a different file because of the order, info
    /// otherwise.
    pub severity: Severity,
    /// What the order changes, in one paragraph.
    pub message: String,
    /// Pointer to the `BBLAYERS` assignment in `bblayers.conf` (`build:conf/bblayers.conf`
    /// when it is not found).
    pub loc: String,
}

/// Files whose relative path BitBake looks up along `BBPATH`: classes, `.inc` files and
/// conf files other than `conf/layer.conf`.
fn bbpath_files(index: &Index, layer: &str) -> Result<BTreeSet<PathBuf>> {
    Ok(query::files_in_layer(index, layer)?
        .into_iter()
        .filter(|f| match f.kind {
            FileKind::Class | FileKind::Include => true,
            FileKind::Conf => f.path != Path::new("conf/layer.conf"),
            _ => false,
        })
        .map(|f| f.path)
        .collect())
}

/// Layers listed in `BBLAYERS` before a layer whose collection they name in `LAYERDEPENDS`,
/// with what that order changes in `BBPATH` lookups (module docs, `compat`), in
/// `BBLAYERS` order. Empty without a build directory (no `BBLAYERS` to check). The rows of
/// `layers show=compat` with check `order`, and of the audit report's section 3.
pub fn order_problems(index: &Index, ws: &Workspace) -> Result<Vec<OrderProblem>> {
    let mut out = Vec::new();
    if ws.build_dir.is_none() {
        return Ok(out);
    }
    let bblayers =
        match query::file_by_path(index, BUILD_LAYER, Path::new("conf/bblayers.conf"))? {
            Some(f) => query::statements_in(index, f.id, &StatementFilter::default())?
                .into_iter()
                .find(|s| s.base.as_deref() == Some("BBLAYERS"))
                .map(|s| s.loc.to_string()),
            None => None,
        }
        .unwrap_or_else(|| format!("{BUILD_LAYER}:conf/bblayers.conf"));
    let bbpath = resolve::bbpath(ws);
    let at = |root: &Path| bbpath.iter().position(|p| p == root);
    for (i, layer) in ws.layers.iter().enumerate() {
        let var = match layer.collections.first() {
            Some(c) => format!("LAYERDEPENDS_{c}"),
            None => continue,
        };
        for d in &layer.depends {
            let Some((j, dep)) = ws
                .layers
                .iter()
                .enumerate()
                .find(|(_, l)| l.collections.contains(d))
            else {
                continue;
            };
            if j <= i {
                continue;
            }
            let head = format!(
                "{} comes before {} in BBLAYERS, though {var} names `{d}` (added by {}). \
                 BitBake does not need dependencies first: LAYERDEPENDS is checked against \
                 every enabled layer and priority does not come from the order",
                layer.name, dep.name, dep.name
            );
            let (severity, effect) = match (at(&layer.root), at(&dep.root)) {
                (Some(a), Some(b)) if a < b => {
                    let mine = bbpath_files(index, &layer.name)?;
                    let theirs = bbpath_files(index, &dep.name)?;
                    let both: Vec<String> = mine
                        .intersection(&theirs)
                        .map(|p| p.display().to_string())
                        .collect();
                    if both.is_empty() {
                        (
                            Severity::Info,
                            format!(
                                "; BBPATH lists {} first, but no class, include or conf file is \
                                 at the same path in both layers, so no lookup is affected today",
                                layer.name
                            ),
                        )
                    } else {
                        (
                            Severity::Warning,
                            format!(
                                "; BBPATH lists {} first, so its {} {} found instead of {}'s by \
                                 include/require, inherit and conf lookups; list {} first if \
                                 that is not intended",
                                layer.name,
                                both.join(", "),
                                if both.len() == 1 { "is" } else { "are" },
                                dep.name,
                                dep.name
                            ),
                        )
                    }
                }
                (Some(_), Some(_)) => (
                    Severity::Info,
                    format!(
                        "; BBPATH still lists {} before {} (a layer.conf prepends to it), so \
                         lookups are not affected",
                        dep.name, layer.name
                    ),
                ),
                _ => (
                    Severity::Info,
                    "; one of the two is not on the configured BBPATH, so the order does not \
                     change lookups between them"
                        .to_string(),
                ),
            };
            out.push(OrderProblem {
                layer: layer.name.clone(),
                severity,
                message: format!("{head}{effect}"),
                loc: bblayers.clone(),
            });
        }
    }
    Ok(out)
}

/// What a bbappend is to BitBake, as the `appends` view's `status` column says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendState {
    /// Collected, and applies to the row's `matches`.
    Applies,
    /// Collected, and matches no recipe (`query::orphan_appends`).
    NoMatch,
    /// Not collected: the view's text, `not collected (...)` with the reason.
    NotCollected(String),
}

/// One row of the `appends` view ([`append_rows`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendRow {
    /// `layer:path` of the bbappend.
    pub bbappend: String,
    /// Its layer's priority.
    pub priority: i32,
    /// `layer:path` of the recipe it applies to; `None` unless [`AppendState::Applies`].
    pub matches: Option<String>,
    pub state: AppendState,
    /// Collected only through an active `BBFILES_DYNAMIC` glob.
    pub via_dynamic: bool,
}

impl AppendRow {
    /// The view's `status` cell: `applies`, `no-match` (each with ` (BBFILES_DYNAMIC)` when
    /// [`AppendRow::via_dynamic`]) or the `not collected (...)` text.
    pub fn status(&self) -> String {
        let suffix = if self.via_dynamic {
            " (BBFILES_DYNAMIC)"
        } else {
            ""
        };
        match &self.state {
            AppendState::Applies => format!("applies{suffix}"),
            AppendState::NoMatch => format!("no-match{suffix}"),
            AppendState::NotCollected(why) => why.clone(),
        }
    }
}

/// The rows of `layers show=appends` (module docs), in the view's order, and the
/// `BBFILES_DYNAMIC` words that cannot be decided statically (left out). The view and the
/// audit report's section 3 both read these.
pub fn append_rows(index: &Index, ws: &Workspace) -> Result<(Vec<AppendRow>, Vec<String>)> {
    let mut rows = Vec::new();
    // The index says what BitBake collects (`files.not_collected`, `bbappend` edges);
    // `resolve::collect` says which `BBFILES_DYNAMIC` entry was involved.
    let collection = resolve::collect(ws);
    let not_collected: HashMap<i64, String> = query::uncollected_recipe_files(index)?
        .into_iter()
        .map(|(f, r)| (f.id, r))
        .collect();
    for a in query::files_by_kind(index, &[FileKind::Append])? {
        let label = file_label(&a);
        let abs = ws.abs_path(&a.loc(LineSpan::line(1)));
        let row = |matches: Option<String>, state: AppendState, via_dynamic: bool| AppendRow {
            bbappend: label.clone(),
            priority: a.priority,
            matches,
            state,
            via_dynamic,
        };
        if let Some(reason) = not_collected.get(&a.id) {
            let entry = abs.as_deref().and_then(|p| {
                collection
                    .inactive_dynamic
                    .iter()
                    .find(|(q, _)| q == p)
                    .and_then(|(_, i)| collection.dynamic.get(*i))
            });
            let why = match (reason.as_str(), entry) {
                (NOT_COLLECTED_DYNAMIC, Some(d)) if d.invert => format!(
                    "not collected (BBFILES_DYNAMIC `!{c}`: layer `{c}` is present)",
                    c = d.collection
                ),
                (NOT_COLLECTED_DYNAMIC, Some(d)) => format!(
                    "not collected (dynamic layer `{}` not present)",
                    d.collection
                ),
                (NOT_COLLECTED_BBMASK, _) => "not collected (masked by BBMASK)".to_string(),
                _ => "not collected (no BBFILES glob)".to_string(),
            };
            rows.push(row(None, AppendState::NotCollected(why), false));
            continue;
        }
        let dynamic = abs
            .as_deref()
            .is_some_and(|p| collection.via_dynamic.contains(p));
        let matched: Vec<FileRow> = query::edges_from(index, a.id)?
            .into_iter()
            .filter(|e| e.kind == "bbappend")
            .filter_map(|e| e.target)
            .collect();
        if matched.is_empty() {
            rows.push(row(None, AppendState::NoMatch, dynamic));
            continue;
        }
        for r in matched {
            rows.push(row(Some(file_label(&r)), AppendState::Applies, dynamic));
        }
    }
    Ok((rows, collection.unusable))
}

fn appends(index: &Index, ws: &Workspace) -> Result<Table> {
    let mut t = Table::new(&["bbappend", "priority", "matches", "status"]);
    let (rows, unusable) = append_rows(index, ws)?;
    let mut orphans = 0;
    let mut uncollected = 0;
    for r in &rows {
        match r.state {
            AppendState::NoMatch => orphans += 1,
            AppendState::NotCollected(_) => uncollected += 1,
            AppendState::Applies => {}
        }
        t.row(&[
            &r.bbappend,
            &r.priority.to_string(),
            r.matches.as_deref().unwrap_or("-"),
            &r.status(),
        ]);
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
    if !unusable.is_empty() {
        t.note(&format!(
            "note: BBFILES_DYNAMIC words sous-chef cannot decide statically (python, or a \
             variable set later), left out: {}",
            unusable.join(" ")
        ));
    }
    if uncollected > 0 {
        t.note(&format!(
            "note: {uncollected} {} not collected by BBFILES or BBFILES_DYNAMIC: BitBake never \
             reads {}, so {} no error",
            if uncollected == 1 {
                "bbappend is"
            } else {
                "bbappends are"
            },
            if uncollected == 1 { "it" } else { "them" },
            if uncollected == 1 {
                "it is"
            } else {
                "they are"
            },
        ));
    }
    Ok(t)
}

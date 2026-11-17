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
//!   `org/append-no-match` reports). A bbappend that no `BBFILES` glob collects is judged by
//!   the layers' `BBFILES_DYNAMIC` entries, which the index does not fold into `BBFILES`:
//!   inactive (`not collected (dynamic layer ... not present)`), active (matched here by
//!   file name), or none (`not collected (no BBFILES glob)`).
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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Deserialize;

use super::recipe::file_label;
use crate::error::Result;
use crate::index::Index;
use crate::index::query::{self, FileRow, StatementFilter};
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

/// A `BBLAYERS` order row.
struct OrderProblem {
    layer: String,
    severity: Severity,
    message: String,
    loc: String,
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
/// with what that order changes in `BBPATH` lookups.
fn order_problems(index: &Index, ws: &Workspace) -> Result<Vec<OrderProblem>> {
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

fn appends(index: &Index, ws: &Workspace) -> Result<Table> {
    let mut t = Table::new(&["bbappend", "priority", "matches", "status"]);
    let mut orphans = 0;
    let mut uncollected = 0;
    let dynamic = dynamic_entries(index, ws)?;
    let collections: BTreeSet<&str> = ws
        .layers
        .iter()
        .flat_map(|l| l.collections.iter().map(String::as_str))
        .collect();
    let mut recipes: Option<Vec<FileRow>> = None;
    for a in query::files_by_kind(index, &[FileKind::Append])? {
        let edges: Vec<_> = query::edges_from(index, a.id)?
            .into_iter()
            .filter(|e| e.kind == "bbappend")
            .collect();
        let matched: Vec<FileRow> = edges.iter().filter_map(|e| e.target.clone()).collect();
        if edges.is_empty() {
            // No `BBFILES` glob of the index collects it: BitBake never reads it, unless a
            // `BBFILES_DYNAMIC` entry adds a glob for it (which the index does not model).
            let label = file_label(&a);
            let prio = a.priority.to_string();
            let abs = ws.abs_path(&a.loc(LineSpan::line(1)));
            let entry = abs.as_deref().and_then(|p| {
                let p = p.to_string_lossy();
                dynamic.iter().find(|d| glob_match(&d.pattern, &p))
            });
            match entry {
                Some(d) if collections.contains(d.collection.as_str()) == d.invert => {
                    uncollected += 1;
                    let why = if d.invert {
                        format!(
                            "not collected (BBFILES_DYNAMIC `!{c}`: layer `{c}` is present)",
                            c = d.collection
                        )
                    } else {
                        format!(
                            "not collected (dynamic layer `{}` not present)",
                            d.collection
                        )
                    };
                    t.row(&[&label, &prio, "-", &why]);
                }
                Some(_) => {
                    // Collected by BitBake; match it the way BitBake does (by file name).
                    let all = match &recipes {
                        Some(r) => r,
                        None => recipes.insert(query::files_by_kind(index, &[FileKind::Recipe])?),
                    };
                    let hits: Vec<&FileRow> = all
                        .iter()
                        .filter(|r| resolve::append_matches(&a.path, &r.path))
                        .collect();
                    if hits.is_empty() {
                        orphans += 1;
                        t.row(&[&label, &prio, "-", "no-match (BBFILES_DYNAMIC)"]);
                    }
                    for r in hits {
                        t.row(&[&label, &prio, &file_label(r), "applies (BBFILES_DYNAMIC)"]);
                    }
                }
                None => {
                    uncollected += 1;
                    t.row(&[&label, &prio, "-", "not collected (no BBFILES glob)"]);
                }
            }
            continue;
        }
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

/// One `BBFILES_DYNAMIC` entry `{!}<collection>:<glob>` with `${LAYERDIR}` replaced by the
/// root of the layer whose file sets it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DynamicEntry {
    collection: String,
    /// `!<collection>`: the glob is added when the collection is absent.
    invert: bool,
    /// Absolute glob.
    pattern: String,
}

/// The `BBFILES_DYNAMIC` entries of the layers' `conf/layer.conf` files. BitBake
/// (`cookerdata.py` `CookerDataBuilder.parseConfigurationFiles`, 2.18 `6e260eb6`) splits
/// the value on whitespace, each word on its first `:` into a collection (a leading `!`
/// inverts it) and a pattern, and appends the pattern to `BBFILES` when the collection is
/// in `BBFILE_COLLECTIONS` (or, inverted, when it is not). Words with another `${...}` than
/// `${LAYERDIR}`, and `:remove` assignments, are left out.
fn dynamic_entries(index: &Index, ws: &Workspace) -> Result<Vec<DynamicEntry>> {
    let scope = StatementFilter {
        kinds: vec!["assign".into()],
        ..StatementFilter::default()
    };
    let mut out = Vec::new();
    for s in query::statements_by_base(index, "BBFILES_DYNAMIC", &scope)? {
        if s.flag.is_some() || s.overrides.iter().any(|o| o == "remove") {
            continue;
        }
        let Some(layer) = ws.layers.iter().find(|l| l.name == s.loc.layer) else {
            continue;
        };
        let root = layer.root.to_string_lossy();
        for word in s.value.as_deref().unwrap_or("").split_whitespace() {
            let Some((coll, pattern)) = word.split_once(':') else {
                continue;
            };
            let pattern = pattern.replace("${LAYERDIR}", &root);
            if pattern.contains("${") || coll.is_empty() {
                continue;
            }
            let (collection, invert) = match coll.strip_prefix('!') {
                Some(c) => (c.to_string(), true),
                None => (coll.to_string(), false),
            };
            out.push(DynamicEntry {
                collection,
                invert,
                pattern,
            });
        }
    }
    Ok(out)
}

/// Whether `path` matches the glob `pattern` as Python's `glob.glob` would find it: compared
/// component by component (`*` and `?` do not cross `/`; `[...]` is a character class).
fn glob_match(pattern: &str, path: &str) -> bool {
    let p: Vec<&str> = pattern.split('/').filter(|c| !c.is_empty()).collect();
    let f: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    p.len() == f.len()
        && p.iter()
            .zip(&f)
            .all(|(p, f)| fnmatch(p.as_bytes(), f.as_bytes()))
}

/// `fnmatch` for one path component: `*`, `?`, `[abc]`, `[a-z]`, `[!abc]`.
fn fnmatch(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| fnmatch(rest, &s[i..])),
        Some((b'?', rest)) => !s.is_empty() && fnmatch(rest, &s[1..]),
        Some((b'[', rest)) => {
            let Some(end) = rest.iter().skip(1).position(|&c| c == b']').map(|i| i + 1) else {
                return s.first() == Some(&b'[') && fnmatch(rest, &s[1..]);
            };
            let Some((&c, tail)) = s.split_first() else {
                return false;
            };
            let (negate, class) = match rest[..end].split_first() {
                Some((b'!', cls)) => (true, cls),
                _ => (false, &rest[..end]),
            };
            let mut hit = false;
            let mut i = 0;
            while i < class.len() {
                if i + 2 < class.len() && class[i + 1] == b'-' {
                    hit |= class[i] <= c && c <= class[i + 2];
                    i += 3;
                } else {
                    hit |= class[i] == c;
                    i += 1;
                }
            }
            hit != negate && fnmatch(&rest[end + 1..], tail)
        }
        Some((&c, rest)) => s.first() == Some(&c) && fnmatch(rest, &s[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        let p = "/l/dynamic-layers/meta-tpm2/*/*/*.bbappend";
        assert!(glob_match(
            p,
            "/l/dynamic-layers/meta-tpm2/recipes-tpm/t/t_5.7.bbappend"
        ));
        assert!(!glob_match(
            p,
            "/l/dynamic-layers/meta-tpm2/recipes-tpm/t/u/t.bbappend"
        ));
        assert!(!glob_match(
            p,
            "/l/dynamic-layers/meta-tpm2/recipes-tpm/t/t.bb"
        ));
        assert!(fnmatch(b"recipes-[a-c]*", b"recipes-bsp"));
        assert!(!fnmatch(b"recipes-[!a-c]*", b"recipes-bsp"));
        assert!(fnmatch(b"x?.bb", b"x1.bb"));
    }
}

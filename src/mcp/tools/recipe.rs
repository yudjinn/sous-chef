//! Tool `recipe`: resolve a recipe and show its shape. See docs/mcp-tools.md.
//!
//! - `summary`: the recipe file and layer, every version (and which one BitBake uses:
//!   `eval::providers`, so `PREFERRED_VERSION` and `DEFAULT_PREFERENCE` count for a bare
//!   `pn`), the bbappends in application order, the classes inherited (static walk) and a
//!   few key values (`SUMMARY`, `LICENSE`, `SRC_URI` and `DEPENDS` word counts) evaluated by
//!   `eval::evaluate` for the default machine and distro, with a note when a value depends
//!   on python that was not evaluated.
//! - `tree`: the files of the recipe in parse order: the `.bb` and what it pulls in, then
//!   each bbappend and what it pulls in, then deferred inherits. One row per file with the
//!   statement that pulled it in. Unresolved targets are rows too.
//! - `outline`: every function and task statement across that tree.
//! - `sources`, `tasks`, `packages` (T8.3, module `views`): evaluated views of `SRC_URI`
//!   (with `FILESPATH` lookup and patch status), the task graph and the packages. See
//!   docs/mcp-tools.md.
//!
//! This file also holds the recipe helpers the other lookup tools share: [`resolve_selected`]
//! (a recipe argument resolved the way BitBake picks the file: `recipe`, `item`, `search` and
//! the `bbappend` generator), [`resolve_recipe`] (static), [`walk_recipe`] (the recipe tree with its
//! statements in parse order) and [`file_ref`].
//!
//! The walk is static, like `query::tree`: an `include` or `inherit` whose argument needs
//! evaluation is unresolved, and the global configuration (`base.bbclass` and the `INHERIT`
//! classes, parsed before every recipe) is not part of it.
//!
//! **Why this is not the replay's visit list.** `eval::evaluate` also walks the recipe
//! (`DataStore::files`), with the live datastore. The two are kept apart because the tools
//! show what the visit list does not record (unresolved, cycle and "already inherited" rows,
//! the edge kind and the argument as written) and because they differ on real metadata:
//!
//! - an argument with a variable other than those that follow from the file's own path
//!   (`PN`, `PV`, `BPN`, `BP`, `THISDIR`, `COREBASE`; `PN` and friends only in a `.bb` /
//!   `.bbappend`, so not in an `.inc`) is unresolved here and followed by the replay; a
//!   recipe that sets `PN` itself expands differently;
//! - classes the configuration inherited (`base`, `INHERIT`) are in the replay's
//!   `__inherit_cache`, so a recipe's `inherit base` is parsed again here and skipped there;
//! - an `inherit_defer` inside a deferred class is dropped here and run by the replay;
//!   classes named in `BB_DEFER_BBCLASSES` are deferred only by the replay;
//! - lookups: here edges were resolved by the scanner along the configured `BBPATH`; the
//!   replay resolves along `BBPATH` as the store has it at that point;
//! - a missing `include` is a row here and silent in the replay (a missing `require` is a
//!   `RefIssue` there); `MAX_DEPTH` stops nesting here, only a cycle stops the replay.
//!
//! A class inherited by both the recipe and a bbappend is parsed once by both. On the fixture
//! recipes the two agree (`tests/eval_replay.rs`, `static_walk_and_replay_*`).

use std::collections::HashSet;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::eval::{self, DataStore, OpStatus, Target, providers};
use crate::index::Index;
use crate::index::query::{self, EdgeRow, FileRow, RecipeRow, StatementFilter, StatementRow};
use crate::mcp::Ctx;
use crate::render::{self, Format, Table};
use crate::types::{FileKind, LineSpan, Loc};
use crate::workspace::Workspace;

/// What to show.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Show {
    /// Recipe file, versions, bbappends in application order, classes, key values.
    #[default]
    Summary,
    /// The include/inherit tree in parse order.
    Tree,
    /// Every task and function across the tree.
    Outline,
    /// Each `SRC_URI` entry: type, the file a `file://` entry resolves to, patch status.
    Sources,
    /// The task graph in order, each task with the pieces that compose it.
    Tasks,
    /// `PACKAGES`, and per package `FILES`, `RDEPENDS`, `RPROVIDES`.
    Packages,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Recipe: `pn`, `pn_pv` or a pointer.
    pub name: String,
    /// Default: the preferred version.
    pub version: Option<String>,
    /// `summary` (default), `tree`, `outline`, `sources`, `tasks` or `packages`.
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
    let (recipe, chosen) =
        resolve_selected(&index, &ctx.workspace, &args.name, args.version.as_deref())?;
    let mut table = match args.show {
        Show::Summary => summary(
            &index,
            &ctx.workspace,
            &recipe,
            args.version.as_deref(),
            chosen,
        )?,
        Show::Tree => tree_table(&index, &recipe)?,
        Show::Outline => outline(&index, &recipe)?,
        Show::Sources | Show::Tasks | Show::Packages => {
            let target = Target {
                recipe: Some(file_label(&recipe.file)),
                ..Target::default()
            };
            let ds = eval::evaluate(&index, &ctx.workspace, &target)?;
            match args.show {
                Show::Sources => views::sources(&ctx.workspace, &ds)?,
                Show::Tasks => views::tasks(&index, &ds)?,
                _ => views::packages(&ds),
            }
        }
    };
    // The evaluated views carry it from the replay already.
    if matches!(args.show, Show::Summary | Show::Tree | Show::Outline)
        && let Some(why) = query::not_collected(&index, recipe.file.id)?
    {
        table.note(&format!(
            "note: {} is {why}: BitBake does not collect it, so no recipe name or provider resolves to it and no bbappend applies",
            file_label(&recipe.file)
        ));
    }
    if let Some(line) = render::freshness_line(&report) {
        table.note(&line);
    }
    let budget = args
        .budget_tokens
        .unwrap_or(ctx.config.output.budget_tokens);
    Ok(table.render_as(args.format.unwrap_or_default(), budget))
}

// ---- shared helpers ----

/// Splits `layer:path` or `layer:path:A-B` into the layer name and the path. `None` when
/// there is no `:` or either part is empty.
pub fn file_ref(s: &str) -> Option<(String, PathBuf)> {
    let (layer, rest) = s.trim().split_once(':')?;
    let path = match rest.rsplit_once(':') {
        Some((p, lines)) if lines.parse::<LineSpan>().is_ok() => p,
        _ => rest,
    };
    if layer.is_empty() || path.is_empty() {
        return None;
    }
    Some((layer.to_string(), PathBuf::from(path)))
}

/// `layer:path`, the way whole files are shown.
pub fn file_label(f: &FileRow) -> String {
    format!("{}:{}", f.layer, f.path.display())
}

fn recipe_label(r: &RecipeRow) -> String {
    format!("{}_{} ({})", r.pn, r.pv, file_label(&r.file))
}

/// Resolves a recipe argument: a `pn` (the preferred version, or `version`), a `pn_pv`, or a
/// pointer to a `.bb` (or to a `.bbappend`, meaning the recipe it applies to).
///
/// Without an exact match the error lists the recipe names that contain the argument.
pub fn resolve_recipe(index: &Index, name: &str, version: Option<&str>) -> Result<RecipeRow> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Invalid {
            msg: "recipe name is empty; give a pn such as busybox, a pn_pv or a pointer".into(),
        });
    }
    if name.contains(':') {
        return resolve_pointer(index, name, version);
    }
    let all = query::find_recipes(index, name)?;
    if !all.is_empty() {
        return query::preferred_recipe(index, name, version)?.ok_or_else(|| Error::NotFound {
            what: format!(
                "recipe {name} version {}; versions: {}",
                version.unwrap_or(""),
                versions(&all)
            ),
        });
    }
    if let Some((pn, pv)) = name.rsplit_once('_') {
        let rows = query::find_recipes(index, pn)?;
        if rows.iter().any(|r| r.pv == pv) {
            if let Some(v) = version
                && v != pv
            {
                return Err(Error::Invalid {
                    msg: format!("{name} names version {pv} but version is {v}; drop one"),
                });
            }
            if let Some(r) = query::preferred_recipe(index, pn, Some(pv))? {
                return Ok(r);
            }
        }
    }
    let candidates = query::recipe_names_like(index, name, 20)?;
    if candidates.is_empty() {
        return Err(Error::NotFound {
            what: format!("recipe {name}"),
        });
    }
    Err(Error::Ambiguous {
        what: format!("recipe name {name:?} (no recipe has exactly this name)"),
        candidates,
    })
}

/// Resolves a recipe argument to the file BitBake would use: [`resolve_recipe`], then, for
/// a bare `pn` without a version, provider selection for the default machine and distro
/// (`eval::providers`: `REQUIRED_VERSION`, `PREFERRED_VERSION`, `DEFAULT_PREFERENCE`, layer
/// priority, version) instead of layer priority and version alone. The second value says
/// why that version was chosen (`Selection::describe`); `None` when provider selection was
/// not used (a `pn_pv`, a pointer, an explicit version) or found nothing.
///
/// This is the one resolution `recipe`, `item`, `search` and the `bbappend` generator share,
/// so a bare `pn` names the same file in all of them. Provider selection needs the
/// configuration pass (cached per process by `eval::global_cache`).
pub fn resolve_selected(
    index: &Index,
    ws: &Workspace,
    name: &str,
    version: Option<&str>,
) -> Result<(RecipeRow, Option<String>)> {
    let recipe = resolve_recipe(index, name, version)?;
    let name = name.trim();
    if version.is_none()
        && !name.contains(':')
        && name == recipe.pn
        && let Some(sel) = providers::preferred_recipe(index, ws, name, None, None, None)?
    {
        return Ok((sel.recipe.clone(), Some(sel.describe())));
    }
    Ok((recipe, None))
}

fn versions(rows: &[RecipeRow]) -> String {
    let mut v: Vec<&str> = rows.iter().map(|r| r.pv.as_str()).collect();
    v.dedup();
    v.join(", ")
}

fn resolve_pointer(index: &Index, name: &str, version: Option<&str>) -> Result<RecipeRow> {
    let (layer, path) = file_ref(name).ok_or_else(|| Error::Invalid {
        msg: format!("bad recipe pointer {name:?}; expected layer:path or a recipe name"),
    })?;
    let file = query::file_by_path(index, &layer, &path)?.ok_or_else(|| Error::NotFound {
        what: format!("file {layer}:{} in the index", path.display()),
    })?;
    match file.kind {
        FileKind::Recipe => recipe_row(file).ok_or_else(|| Error::NotFound {
            what: format!("recipe name and version in the file name of {name}"),
        }),
        FileKind::Append => {
            let mut targets: Vec<RecipeRow> = query::edges_from(index, file.id)?
                .into_iter()
                .filter(|e| e.kind == "bbappend")
                .filter_map(|e| e.target.and_then(recipe_row))
                .collect();
            if let Some(v) = version {
                targets.retain(|r| query::version_matches(&r.pv, v));
            }
            match targets.len() {
                0 => Err(Error::NotFound {
                    what: match query::not_collected(index, file.id)? {
                        Some(why) => format!(
                            "a recipe that {} applies to: it is {why}, so BitBake does not apply it",
                            file_label(&file)
                        ),
                        None => format!("a recipe that {} applies to", file_label(&file)),
                    },
                }),
                1 => Ok(targets.remove(0)),
                _ => Err(Error::Ambiguous {
                    what: format!("recipe: {} applies to several recipes", file_label(&file)),
                    candidates: targets.iter().map(recipe_label).collect(),
                }),
            }
        }
        other => Err(Error::Invalid {
            msg: format!(
                "{} is a .{} file, not a recipe; give a .bb or .bbappend pointer or a recipe name",
                file_label(&file),
                other.as_str()
            ),
        }),
    }
}

fn recipe_row(file: FileRow) -> Option<RecipeRow> {
    Some(RecipeRow {
        pn: file.pn.clone()?,
        pv: file.pv.clone().unwrap_or_default(),
        file,
    })
}

// ---- the recipe walk ----

/// One file in the recipe tree.
#[derive(Debug, Clone)]
pub struct Node {
    /// 0 for the recipe and its bbappends.
    pub depth: u32,
    /// `recipe`, `bbappend`, or the edge kind (`require`, `include`, `include_all`,
    /// `inherit`, `inherit_defer`).
    pub kind: String,
    /// The argument as written (the file name for the recipe and bbappends).
    pub name: String,
    /// `None`: unresolved.
    pub file: Option<FileRow>,
    /// The statement that pulled it in.
    pub via: Option<Loc>,
    pub state: NodeState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeState {
    Parsed,
    /// Not found statically (missing, or the argument needs evaluation).
    Unresolved,
    /// Already open further up this branch: an include cycle.
    Cycle,
    /// A class inherited earlier: BitBake does not parse it again.
    Repeated,
}

impl NodeState {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeState::Parsed => "-",
            NodeState::Unresolved => "unresolved",
            NodeState::Cycle => "cycle",
            NodeState::Repeated => "already inherited",
        }
    }
}

/// The recipe's files and statements in parse order.
#[derive(Debug, Clone, Default)]
pub struct Walk {
    pub nodes: Vec<Node>,
    /// Every statement of every parsed file, in parse order (an include's statements follow
    /// the `include` statement).
    pub statements: Vec<StatementRow>,
    /// Parsed file ids, in first-visit order.
    pub files: Vec<i64>,
}

/// Guards against pathological nesting (a cycle is caught separately).
const MAX_DEPTH: u32 = 64;

/// Walks a recipe in BitBake's order: the `.bb`, statement by statement, following
/// `require` / `include` / `include_all` / `inherit` where they appear; then each bbappend
/// in application order the same way; then deferred inherits (`inherit_defer`). A class is
/// parsed once per recipe. Static resolution only.
pub fn walk_recipe(index: &Index, recipe: &RecipeRow) -> Result<Walk> {
    let mut w = Walker {
        index,
        stack: Vec::new(),
        inherited: HashSet::new(),
        seen: HashSet::new(),
        deferred: Vec::new(),
        out: Walk::default(),
    };
    w.top(&recipe.file, "recipe")?;
    for a in query::appends_for(index, recipe.file.id)? {
        w.top(&a, "bbappend")?;
    }
    let deferred = std::mem::take(&mut w.deferred);
    w.stack = vec![recipe.file.id];
    for e in deferred {
        w.edge(&e, "inherit_defer", 1)?;
    }
    Ok(w.out)
}

struct Walker<'a> {
    index: &'a Index,
    stack: Vec<i64>,
    inherited: HashSet<i64>,
    seen: HashSet<i64>,
    deferred: Vec<EdgeRow>,
    out: Walk,
}

impl Walker<'_> {
    fn top(&mut self, file: &FileRow, kind: &str) -> Result<()> {
        self.out.nodes.push(Node {
            depth: 0,
            kind: kind.into(),
            name: file
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            file: Some(file.clone()),
            via: None,
            state: NodeState::Parsed,
        });
        self.stack = vec![file.id];
        self.visit(file.id, 1)
    }

    fn visit(&mut self, file_id: i64, depth: u32) -> Result<()> {
        if self.seen.insert(file_id) {
            self.out.files.push(file_id);
        }
        if depth > MAX_DEPTH {
            return Ok(());
        }
        let stmts = query::statements_in(self.index, file_id, &StatementFilter::default())?;
        let edges: Vec<EdgeRow> = query::edges_from(self.index, file_id)?
            .into_iter()
            .filter(|e| e.kind != "bbappend")
            .collect();
        for s in stmts {
            let line = s.loc.lines.start;
            let is_ref = s.kind == "include" || s.kind == "inherit";
            let deferred = s.op.as_deref() == Some("inherit_defer");
            self.out.statements.push(s);
            if !is_ref {
                continue;
            }
            for e in edges.iter().filter(|e| e.line == Some(line)) {
                if deferred {
                    self.deferred.push(e.clone());
                } else {
                    self.edge(e, &e.kind, depth)?;
                }
            }
        }
        Ok(())
    }

    fn edge(&mut self, e: &EdgeRow, kind: &str, depth: u32) -> Result<()> {
        let target = e.target.clone();
        let id = target.as_ref().map(|t| t.id);
        let state = match id {
            None => NodeState::Unresolved,
            Some(t) if self.stack.contains(&t) => NodeState::Cycle,
            Some(t) if e.kind == "inherit" && self.inherited.contains(&t) => NodeState::Repeated,
            Some(_) => NodeState::Parsed,
        };
        self.out.nodes.push(Node {
            depth,
            kind: kind.into(),
            name: e.target_text.clone(),
            file: target,
            via: Some(e.via()),
            state,
        });
        if let (Some(t), NodeState::Parsed) = (id, state) {
            if e.kind == "inherit" {
                self.inherited.insert(t);
            }
            self.stack.push(t);
            self.visit(t, depth + 1)?;
            self.stack.pop();
        }
        Ok(())
    }
}

// ---- show=summary ----

fn summary(
    index: &Index,
    ws: &Workspace,
    recipe: &RecipeRow,
    version: Option<&str>,
    chosen: Option<String>,
) -> Result<Table> {
    let walk = walk_recipe(index, recipe)?;
    let mut t = Table::new(&["field", "value", "loc"]);
    t.row(&[
        "recipe",
        &format!("{} {}", recipe.pn, recipe.pv),
        &file_label(&recipe.file),
    ]);
    t.row(&[
        "layer",
        &format!("{} (priority {})", recipe.file.layer, recipe.file.priority),
        "-",
    ]);
    let chosen = match (version, chosen) {
        (Some(v), _) => format!("selected by version={v}"),
        (None, Some(c)) => c,
        (None, None) => "preferred by layer priority and version".into(),
    };
    for r in query::find_recipes(index, &recipe.pn)? {
        let v = if r.file.id == recipe.file.id {
            format!("{} ({chosen})", r.pv)
        } else {
            r.pv.clone()
        };
        t.row(&["version", &v, &file_label(&r.file)]);
    }
    for n in walk.nodes.iter().filter(|n| n.kind == "bbappend") {
        if let Some(f) = &n.file {
            t.row(&[
                "bbappend",
                &format!("{} (priority {})", f.layer, f.priority),
                &file_label(f),
            ]);
        }
    }
    for n in walk
        .nodes
        .iter()
        .filter(|n| n.kind.starts_with("inherit") && n.state != NodeState::Repeated)
    {
        let mut v = n.name.clone();
        if n.kind == "inherit_defer" {
            v.push_str(" (deferred)");
        }
        if n.state == NodeState::Unresolved {
            v.push_str(" (unresolved)");
        }
        let via = n.via.as_ref().map_or("-".into(), Loc::to_string);
        t.row(&["inherit", &v, &via]);
    }

    // Key values, evaluated (configuration, the recipe, its bbappends; default machine and
    // distro).
    let target = Target {
        recipe: Some(file_label(&recipe.file)),
        version: None,
        machine: None,
        distro: None,
    };
    let ds = eval::evaluate(index, ws, &target)?;
    let mut python = 0;
    for var in ["SUMMARY", "LICENSE"] {
        let (mut v, loc) = match ds.value(var) {
            Some(val) => {
                let mut s = render::truncate_value(&val.text, 100, None);
                if val.unevaluated_python {
                    python += 1;
                    s.push_str(" (depends on python, not evaluated)");
                }
                (s, last_applied(&ds, var))
            }
            None => ("(not set)".to_string(), None),
        };
        if v.is_empty() {
            v = "(empty)".into();
        }
        t.row(&[var, &v, &loc.map_or("-".into(), |l| l.to_string())]);
    }
    for var in ["SRC_URI", "DEPENDS"] {
        let (words, py) = ds
            .value(var)
            .map(|val| (val.text.split_whitespace().count(), val.unevaluated_python))
            .unwrap_or((0, false));
        if py {
            python += 1;
        }
        let history = ds.history(var);
        let applied = history
            .iter()
            .filter(|(_, s)| *s == OpStatus::Applied)
            .count();
        let inactive = history
            .iter()
            .filter(|(_, s)| *s == OpStatus::InactiveOverride)
            .count();
        let mut v = format!(
            "{words} {} from {applied} applied {}",
            plural(words, "entry"),
            plural(applied, "operation")
        );
        if inactive > 0 {
            v.push_str(&format!(
                "; {inactive} inactive {} (override not active)",
                plural(inactive, "operation")
            ));
        }
        if py {
            v.push_str("; depends on python, not evaluated");
        }
        t.row(&[var, &v, "-"]);
    }
    let machine = ds.value("MACHINE").map(|v| v.text).unwrap_or_default();
    let distro = ds.value("DISTRO").map(|v| v.text).unwrap_or_default();
    t.note(&format!(
        "note: key values are evaluated for MACHINE={machine} DISTRO={distro} (static replay of the configuration, classes, recipe and bbappends; python not run); use var for the full history"
    ));
    let anon = &ds.replay().anonymous;
    if anon.iter().any(|a| a.opaque) {
        t.note("warning: anonymous python changes variables through computed names; any key value may differ");
    } else if !anon.is_empty() {
        t.note(&format!(
            "note: {} anonymous python {} not run; values it changes may differ",
            anon.len(),
            if anon.len() == 1 {
                "function was"
            } else {
                "functions were"
            }
        ));
    }
    if let Some(n) = render::python_note(python) {
        t.note(&n);
    }
    Ok(t)
}

/// The last applied assignment to `var` (no flag).
fn last_applied(ds: &DataStore, var: &str) -> Option<Loc> {
    ds.history(var)
        .into_iter()
        .rev()
        .find(|(op, s)| *s == OpStatus::Applied && op.var.flag.is_none())
        .map(|(op, _)| op.loc)
}

fn plural(n: usize, word: &str) -> String {
    match (n, word) {
        (1, w) => w.to_string(),
        (_, "entry") => "entries".into(),
        (_, w) => format!("{w}s"),
    }
}

// ---- show=tree ----

fn tree_table(index: &Index, recipe: &RecipeRow) -> Result<Table> {
    let walk = walk_recipe(index, recipe)?;
    let mut t = Table::new(&["depth", "kind", "name", "file", "via", "state"]);
    let mut unresolved = 0;
    for n in &walk.nodes {
        if n.state == NodeState::Unresolved {
            unresolved += 1;
        }
        let file = n.file.as_ref().map_or("-".into(), file_label);
        let via = n.via.as_ref().map_or("-".into(), Loc::to_string);
        t.row(&[
            &n.depth.to_string(),
            &n.kind,
            &n.name,
            &file,
            &via,
            n.state.as_str(),
        ]);
    }
    t.note(
        "note: static parse order (recipe, then bbappends in application order, then deferred \
         inherits); base.bbclass and INHERIT classes are parsed before the recipe and not shown",
    );
    if unresolved > 0 {
        t.note(&format!(
            "note: {unresolved} {} not resolved statically (missing, or the argument needs evaluation)",
            if unresolved == 1 { "target" } else { "targets" }
        ));
    }
    Ok(t)
}

// ---- show=outline ----

/// The `kind` column of an outline row, or `None` for statements that are not functions or
/// tasks.
pub fn outline_kind(s: &StatementRow) -> Option<String> {
    match s.kind.as_str() {
        "function" => Some(s.op.clone().unwrap_or_else(|| "function".into())),
        "pydef" => Some("def".into()),
        "addtask" | "deltask" | "addhandler" => Some(s.kind.clone()),
        _ => None,
    }
}

fn outline(index: &Index, recipe: &RecipeRow) -> Result<Table> {
    let walk = walk_recipe(index, recipe)?;
    let mut t = Table::new(&["name", "kind", "file_kind", "loc"]);
    let mut kinds: std::collections::HashMap<i64, FileKind> = std::collections::HashMap::new();
    for n in &walk.nodes {
        if let Some(f) = &n.file {
            kinds.insert(f.id, f.kind);
        }
    }
    for s in &walk.statements {
        let Some(kind) = outline_kind(s) else {
            continue;
        };
        let fk = kinds.get(&s.file_id).map_or("-", |k| k.as_str());
        t.row(&[
            s.name.as_deref().unwrap_or("-"),
            &kind,
            fk,
            &s.loc.to_string(),
        ]);
    }
    Ok(t)
}

// ---- show=sources, show=tasks, show=packages (T8.3) ----

/// The evaluated views: each takes the datastore of `eval::evaluate` for the chosen file
/// (default machine and distro) and states what it does not evaluate.
mod views {
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::path::{Path, PathBuf};

    use regex::Regex;

    use crate::error::Result;
    use crate::eval::{DataStore, OpStatus};
    use crate::index::Index;
    use crate::index::query::{self, StatementFilter, StatementRow};
    use crate::render::{self, Table};
    use crate::types::{FileKind, LineSpan, Loc};
    use crate::workspace::Workspace;

    // ---- shared ----

    /// `note: evaluated for MACHINE=.. DISTRO=..` and the warnings about anonymous python
    /// (stronger for an opaque function) and about references the replay did not follow.
    /// `vars` are the base names the view shows.
    fn replay_notes(t: &mut Table, ds: &DataStore, vars: &[&str], what: &str) {
        let machine = ds.value("MACHINE").map(|v| v.text).unwrap_or_default();
        let distro = ds.value("DISTRO").map(|v| v.text).unwrap_or_default();
        t.note(&format!(
            "note: evaluated for MACHINE={machine} DISTRO={distro} (static replay of the configuration, classes, recipe and bbappends; python not run)"
        ));
        let notes = ds.replay();
        for n in &notes.target_notes {
            t.note(n);
        }
        let mut other = 0usize;
        for a in &notes.anonymous {
            let named: Vec<&str> = a
                .sets
                .iter()
                .map(|s| s.split(':').next().unwrap_or(s))
                .filter(|b| vars.contains(b))
                .collect();
            if a.opaque {
                t.note(&format!(
                    "warning: anonymous python at {} changes variables through computed names or hands d to other code; it is not run, so {what}",
                    a.loc
                ));
            } else if !named.is_empty() {
                let mut named = named;
                named.dedup();
                t.note(&format!(
                    "warning: anonymous python at {} changes {}; it is not run, so the values shown are the ones before it",
                    a.loc,
                    named.join(", ")
                ));
            } else {
                other += 1;
            }
        }
        if other > 0 {
            let functions = if other == 1 {
                "function was"
            } else {
                "functions were"
            };
            if vars.is_empty() {
                t.note(&format!(
                    "warning: {other} anonymous python {functions} not run, so {what}"
                ));
            } else {
                t.note(&format!(
                    "note: {other} anonymous python {functions} not run (none changes {} by name)",
                    vars.join(", ")
                ));
            }
        }
        if !notes.references.is_empty() {
            let n = notes.references.len();
            t.note(&format!(
                "note: {n} include/inherit {} not followed (python, undefined variables or missing files); values from those files are missing",
                if n == 1 { "reference was" } else { "references were" }
            ));
        }
    }

    /// The words of a value; a `${...}` (python included) stays inside one word.
    pub(super) fn split_words(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut depth = 0usize;
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '$' && chars.peek() == Some(&'{') {
                chars.next();
                depth += 1;
                cur.push_str("${");
                continue;
            }
            if depth > 0 {
                match c {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
                cur.push(c);
                continue;
            }
            if c.is_whitespace() {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            } else {
                cur.push(c);
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out
    }

    /// The applied operations on `name` (no flag), in parse order.
    fn applied_locs(ds: &DataStore, name: &str) -> Vec<Loc> {
        ds.history(name)
            .into_iter()
            .filter(|(op, st)| *st == OpStatus::Applied && op.var.flag.is_none())
            .map(|(op, _)| op.loc)
            .collect()
    }

    fn locs_cell(locs: &[Loc]) -> String {
        if locs.is_empty() {
            "-".into()
        } else {
            locs.iter()
                .map(Loc::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        }
    }

    /// `layer:path` for an absolute path in the workspace (a trailing `/` kept), else the
    /// path as it is.
    fn shown_path(ws: &Workspace, p: &str) -> String {
        let slash = if p.ends_with('/') { "/" } else { "" };
        match ws.loc_for(Path::new(p), LineSpan::line(1)) {
            Some(l) if l.path.as_os_str().is_empty() => format!("{}:{slash}", l.layer),
            Some(l) => format!("{}:{}{slash}", l.layer, l.path.display()),
            None => p.to_string(),
        }
    }

    // ---- sources ----

    /// One `SRC_URI` word: scheme, the path part (before the first `;`) and parameters.
    struct Uri {
        scheme: String,
        path: String,
        params: Vec<(String, String)>,
    }

    /// `bb.fetch.decodeurl` for what this view needs: `scheme://location;k=v;...`. For
    /// `file://a/b` the path is `a/b` (relative, searched along `FILESPATH`); for
    /// `file:///a/b` it is `/a/b`.
    fn parse_uri(word: &str) -> Option<Uri> {
        let (scheme, rest) = word.split_once("://")?;
        let mut parts = rest.split(';');
        let path = parts.next().unwrap_or("").to_string();
        let params = parts
            .filter(|p| !p.is_empty())
            .map(|p| match p.split_once('=') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (p.to_string(), String::new()),
            })
            .collect();
        Some(Uri {
            scheme: scheme.to_string(),
            path,
            params,
        })
    }

    impl Uri {
        fn param(&self, key: &str) -> Option<&str> {
            self.params
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        }

        /// OE-Core `oe.patch.patch_path`: `apply=` decides when given (`oe.types.boolean`);
        /// otherwise the extension, after a `.gz` / `.bz2` / `.xz` / `.Z`, is `.patch` or
        /// `.diff`. (A `file://` entry that is a directory is never a patch; the caller
        /// checks that.)
        fn is_patch(&self) -> bool {
            if let Some(v) = self.param("apply") {
                return matches!(v.to_lowercase().as_str(), "yes" | "y" | "true" | "t" | "1");
            }
            let base = self.path.rsplit('/').next().unwrap_or(&self.path);
            let base = [".gz", ".bz2", ".xz", ".Z"]
                .iter()
                .find_map(|c| base.strip_suffix(c))
                .unwrap_or(base);
            base.ends_with(".patch") || base.ends_with(".diff")
        }

        fn compressed(&self) -> bool {
            [".gz", ".bz2", ".xz", ".Z"]
                .iter()
                .any(|c| self.path.ends_with(c))
        }
    }

    /// The `FILESPATH` search list and how it was obtained.
    struct SearchPath {
        dirs: Vec<String>,
        reconstructed: bool,
        python: bool,
    }

    /// The arguments of OE-Core's `base_set_filespath([...], d)` in the expanded
    /// `FILESPATH` text: the quoted strings inside the list.
    fn filespath_args(text: &str) -> Option<Vec<String>> {
        let start = text.find("base_set_filespath(")? + "base_set_filespath(".len();
        let rest = &text[start..];
        let open = rest.find('[')?;
        let close = rest.find(']')?;
        let list = rest.get(open + 1..close)?;
        let re = Regex::new(r#""([^"]*)"|'([^']*)'"#).ok()?;
        let out: Vec<String> = re
            .captures_iter(list)
            .filter_map(|c| {
                c.get(1)
                    .or_else(|| c.get(2))
                    .map(|m| m.as_str().to_string())
            })
            .collect();
        (!out.is_empty()).then_some(out)
    }

    /// `FILESPATH` as OE-Core's `oe.utils.base_set_filespath(path, d)` builds it: the
    /// `FILESEXTRAPATHS` entries (with `__default:` removed) before `path`, and for every
    /// override of `":" + FILESOVERRIDES`, last override first and the empty override last,
    /// each entry of that list joined with the override (`os.path.join(p, o)`), empty
    /// entries skipped. A `FILESPATH` without python is used as it is.
    fn search_path(ds: &DataStore) -> Option<SearchPath> {
        let fp = ds.value("FILESPATH")?;
        if !fp.unevaluated_python {
            return Some(SearchPath {
                dirs: fp
                    .text
                    .split(':')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect(),
                reconstructed: false,
                python: false,
            });
        }
        let dir = ds.value("FILE_DIRNAME").map(|v| v.text).unwrap_or_default();
        let base = filespath_args(&fp.text).unwrap_or_else(|| {
            ["${BP}", "${BPN}", "files"]
                .iter()
                .map(|d| format!("{dir}/{}", ds.expand(d).text))
                .collect()
        });
        let extra = ds.value("FILESEXTRAPATHS");
        let overrides = ds.value("FILESOVERRIDES");
        let python = extra.as_ref().is_some_and(|v| v.unevaluated_python)
            || overrides.as_ref().is_some_and(|v| v.unevaluated_python)
            || base.iter().any(|b| b.contains("${@"));
        let extra = extra
            .map(|v| v.text)
            .unwrap_or_default()
            .replace("__default:", "");
        let mut path: Vec<String> = Vec::new();
        if !extra.is_empty() {
            path.extend(extra.split(':').map(str::to_string));
        }
        path.extend(base);
        let mut ovr: Vec<String> = format!(":{}", overrides.map(|v| v.text).unwrap_or_default())
            .split(':')
            .map(str::to_string)
            .collect();
        ovr.reverse();
        let mut dirs = Vec::new();
        for o in &ovr {
            for p in path.iter().filter(|p| !p.is_empty()) {
                dirs.push(if o.is_empty() {
                    format!("{}/", p.trim_end_matches('/'))
                } else {
                    format!("{}/{o}", p.trim_end_matches('/'))
                });
            }
        }
        Some(SearchPath {
            dirs,
            reconstructed: true,
            python,
        })
    }

    enum Found {
        File(PathBuf),
        Dir(PathBuf),
        Missing,
    }

    /// `bb.utils.which(FILESPATH, path)`: the first `<dir>/<path>` that exists
    /// (`os.path.exists`, so a directory counts); an absolute path is used as it is. Every
    /// probe goes through `Workspace::confine`; candidates outside the workspace are
    /// counted in `outside` and not probed.
    fn find(ws: &Workspace, sp: &SearchPath, rel: &str, outside: &mut usize) -> Found {
        let probe = |p: &Path, outside: &mut usize| -> Option<Found> {
            match ws.confine(p) {
                Ok(c) if c.is_dir() => Some(Found::Dir(p.to_path_buf())),
                Ok(c) if c.exists() => Some(Found::File(p.to_path_buf())),
                Ok(_) => None,
                Err(_) => {
                    *outside += 1;
                    None
                }
            }
        };
        if rel.starts_with('/') {
            return probe(Path::new(rel), outside).unwrap_or(Found::Missing);
        }
        for d in &sp.dirs {
            let cand = Path::new(d).join(rel);
            if let Some(f) = probe(&cand, outside) {
                return f;
            }
        }
        Found::Missing
    }

    /// OE-Core `oe.qa.check_upstream_status`: the strict header line, else a line that
    /// looks like one (malformed), else missing. Read through `Workspace::confine`.
    fn upstream_status(ws: &Workspace, path: &Path) -> String {
        let Ok(p) = ws.confine(path) else {
            return "not read (outside the workspace)".into();
        };
        let Ok(bytes) = fs::read(&p) else {
            return "not read".into();
        };
        let text = String::from_utf8_lossy(&bytes);
        let strict = Regex::new(
            r"(?m)^Upstream-Status: (Pending|Submitted|Denied|Inappropriate|Backport|Inactive-Upstream)( .+)?$",
        );
        let kinda = Regex::new(r"(?im)^.*upstream.*status.*$");
        if let Ok(re) = strict
            && let Some(m) = re.find(&text)
        {
            return render::snippet(m.as_str().trim_end_matches('\r'));
        }
        if let Ok(re) = kinda
            && let Some(m) = re.find(&text)
        {
            return format!(
                "malformed: {}",
                render::snippet(m.as_str().trim_end_matches('\r'))
            );
        }
        "missing Upstream-Status".into()
    }

    /// One `SRC_URI` word with the operation it came from and its state.
    struct Entry {
        word: String,
        state: String,
        loc: Option<Loc>,
    }

    /// The override condition of an operation (`append:boardx` -> `boardx`).
    fn condition(overrides: &[String]) -> String {
        overrides
            .iter()
            .filter(|o| !matches!(o.as_str(), "append" | "prepend" | "remove"))
            .cloned()
            .collect::<Vec<_>>()
            .join(":")
    }

    /// The final words of `SRC_URI`, each attributed to the first applied operation (in
    /// parse order) whose expanded value has it and has not given it already; then the
    /// words of inactive and removed operations.
    fn entries(ds: &DataStore) -> (Vec<Entry>, bool, usize) {
        let value = ds.value("SRC_URI");
        let python = value.as_ref().is_some_and(|v| v.unevaluated_python);
        let words = value.map(|v| split_words(&v.text)).unwrap_or_default();
        let history: Vec<_> = ds
            .history("SRC_URI")
            .into_iter()
            .filter(|(op, _)| op.var.flag.is_none())
            .collect();
        let mut pool: Vec<(Loc, Vec<String>)> = history
            .iter()
            .filter(|(_, st)| *st == OpStatus::Applied)
            .map(|(op, _)| (op.loc.clone(), split_words(&ds.expand(&op.value).text)))
            .collect();
        let mut out = Vec::new();
        for w in words {
            let loc = pool.iter_mut().find_map(|(l, ws)| {
                let i = ws.iter().position(|x| *x == w)?;
                ws.remove(i);
                Some(l.clone())
            });
            out.push(Entry {
                word: w,
                state: "active".into(),
                loc,
            });
        }
        let mut shadowed = 0;
        for (op, st) in &history {
            let state = match st {
                OpStatus::InactiveOverride => {
                    let c = condition(&op.var.overrides);
                    if op.var.legacy_syntax {
                        "inactive (legacy _ syntax)".to_string()
                    } else {
                        format!("inactive (override {c} not active)")
                    }
                }
                OpStatus::Removed => "removed by :remove".to_string(),
                OpStatus::Shadowed => {
                    shadowed += 1;
                    continue;
                }
                OpStatus::Applied => continue,
            };
            for w in split_words(&ds.expand(&op.value).text) {
                out.push(Entry {
                    word: w,
                    state: state.clone(),
                    loc: Some(op.loc.clone()),
                });
            }
        }
        (out, python, shadowed)
    }

    pub fn sources(ws: &Workspace, ds: &DataStore) -> Result<Table> {
        let mut t = Table::new(&["entry", "type", "state", "file", "status", "loc"]);
        let (entries, python, shadowed) = entries(ds);
        let sp = search_path(ds);
        let mut outside = 0usize;
        let mut missing: Vec<String> = Vec::new();
        let mut any_file = false;
        let mut python_words = 0usize;
        for e in &entries {
            let loc = e.loc.as_ref().map_or("-".into(), Loc::to_string);
            if e.word.contains("${@") {
                python_words += 1;
                t.row(&[
                    &render::snippet(&e.word),
                    "-",
                    &format!("{} (depends on python, not evaluated)", e.state),
                    "-",
                    "-",
                    &loc,
                ]);
                continue;
            }
            let Some(uri) = parse_uri(&e.word) else {
                t.row(&[
                    &render::snippet(&e.word),
                    "-",
                    &format!("{} (not a URL)", e.state),
                    "-",
                    "-",
                    &loc,
                ]);
                continue;
            };
            let mut kind = uri.scheme.clone();
            let mut file = "-".to_string();
            let mut status = "-".to_string();
            let mut patch = uri.is_patch();
            if uri.scheme == "file" {
                any_file = true;
                match &sp {
                    None => file = "not searched (FILESPATH is not set)".into(),
                    Some(sp) => match find(ws, sp, &uri.path, &mut outside) {
                        Found::File(p) => {
                            file = shown_path(ws, &p.display().to_string());
                            if patch {
                                status = if uri.compressed() {
                                    "not read (compressed)".into()
                                } else {
                                    upstream_status(ws, &p)
                                };
                            }
                        }
                        Found::Dir(p) => {
                            patch = false;
                            file =
                                format!("{} (directory)", shown_path(ws, &p.display().to_string()));
                        }
                        Found::Missing => {
                            file = if uri.path.starts_with('/') {
                                "not found".into()
                            } else {
                                format!("not found ({} paths searched, below)", sp.dirs.len())
                            };
                            if e.state == "active" && !uri.path.starts_with('/') {
                                missing.push(uri.path.clone());
                            }
                        }
                    },
                }
            } else if patch {
                status = "not read (remote)".into();
            }
            if patch {
                kind.push_str(" (patch)");
            }
            t.row(&[
                &render::snippet(&e.word),
                &kind,
                &e.state,
                &file,
                &status,
                &loc,
            ]);
        }
        // The search order for each active file:// entry that was not found.
        if let Some(sp) = &sp {
            for m in &missing {
                for (i, d) in sp.dirs.iter().enumerate() {
                    t.row(&[
                        &format!("file://{m}"),
                        "searched",
                        &format!("#{}", i + 1),
                        &shown_path(ws, d),
                        "missing",
                        "-",
                    ]);
                }
            }
        }
        if t.is_empty() {
            t.note("note: SRC_URI is empty or not set");
        }
        if any_file && let Some(sp) = &sp {
            if sp.reconstructed {
                let n = ds
                    .value("FILESOVERRIDES")
                    .map(|v| v.text.split(':').filter(|s| !s.is_empty()).count())
                    .unwrap_or(0);
                t.note(&format!(
                    "note: FILESPATH is built by OE-Core's base_set_filespath (python), which is not run; the search order is reconstructed: FILESEXTRAPATHS, then the recipe's ${{BP}}, ${{BPN}} and files directories, each tried with every FILESOVERRIDES entry (last first; {n} {}) and then without",
                    if n == 1 { "entry" } else { "entries" }
                ));
                if n == 0 {
                    t.note("note: FILESOVERRIDES is empty or not set, so no override subdirectory is searched; \":\" + \"\" splits into two empty overrides, so each directory is searched twice, as in OE-Core");
                }
            }
            if sp.python {
                t.note("warning: FILESEXTRAPATHS or FILESOVERRIDES depends on python, which is not evaluated; the search order may be wrong");
            }
        }
        if outside > 0 {
            t.note(&format!(
                "note: {outside} search {} outside the workspace and not checked",
                if outside == 1 { "path is" } else { "paths are" }
            ));
        }
        if shadowed > 0 {
            t.note(&format!(
                "note: {shadowed} shadowed SRC_URI {} not shown; use var SRC_URI",
                if shadowed == 1 {
                    "operation is"
                } else {
                    "operations are"
                }
            ));
        }
        if python || python_words > 0 {
            t.note(&render::python_note(python_words.max(1)).unwrap_or_default());
        }
        replay_notes(
            &mut t,
            ds,
            &["SRC_URI", "FILESEXTRAPATHS", "FILESOVERRIDES"],
            "the sources may differ",
        );
        Ok(t)
    }

    // ---- tasks ----

    enum Event {
        Add {
            name: String,
            before: Vec<String>,
            after: Vec<String>,
            loc: Loc,
        },
        Del {
            names: String,
            loc: Loc,
        },
    }

    /// `bb.build.ensure_task_prefix`.
    fn prefixed(name: &str) -> String {
        if name.starts_with("do_") {
            name.to_string()
        } else {
            format!("do_{name}")
        }
    }

    /// The `addtask` / `deltask` statements of the evaluated recipe in parse order: the
    /// files the replay visited (`DataStore::files`), each file's statements in line order,
    /// with an included or inherited file's statements at the `include` / `inherit` that
    /// pulled it in. bbappends, `INHERIT` classes and deferred inherits are not nested:
    /// they are parsed after the file that names them.
    fn task_events(index: &Index, ds: &DataStore) -> Result<Vec<Event>> {
        let visits = ds.files();
        let mut ids: Vec<Option<i64>> = Vec::with_capacity(visits.len());
        let mut stmts: HashMap<i64, Vec<StatementRow>> = HashMap::new();
        for v in visits {
            let id = query::file_for_loc(index, &v.loc)?.map(|f| f.id);
            if let Some(id) = id
                && !stmts.contains_key(&id)
            {
                stmts.insert(
                    id,
                    query::statements_in(index, id, &StatementFilter::default())?,
                );
            }
            ids.push(id);
        }
        // parent[j] = (i, line): visit j is parsed at line `line` of visit i.
        let mut children: HashMap<usize, Vec<(u32, usize)>> = HashMap::new();
        let mut top: Vec<usize> = Vec::new();
        for (j, v) in visits.iter().enumerate() {
            let parent = match (v.kind, v.via.last()) {
                (FileKind::Append, _) | (_, None) => None,
                (_, Some(at)) => (0..j).rev().find_map(|i| {
                    let p = &visits[i].loc;
                    if p.layer != at.layer || p.path != at.path {
                        return None;
                    }
                    let line = at.lines.start;
                    let nested = ids[i].and_then(|id| stmts.get(&id)).is_some_and(|ss| {
                        ss.iter().any(|s| {
                            s.loc.lines.start == line
                                && (s.kind == "include"
                                    || (s.kind == "inherit"
                                        && s.op.as_deref() != Some("inherit_defer")))
                        })
                    });
                    nested.then_some((i, line))
                }),
            };
            match parent {
                Some((i, line)) => children.entry(i).or_default().push((line, j)),
                None => top.push(j),
            }
        }
        let mut out = Vec::new();
        let mut tasks: HashMap<i64, Vec<query::TaskRow>> = HashMap::new();
        for id in ids.iter().flatten() {
            if !tasks.contains_key(id) {
                tasks.insert(*id, query::tasks_in(index, *id)?);
            }
        }
        let mut stack: Vec<usize> = top.into_iter().rev().collect();
        // Depth-first, children at their line: an explicit work list of visits and events.
        enum Item {
            Visit(usize),
            Event(Event),
        }
        let mut work: Vec<Item> = stack.drain(..).map(Item::Visit).collect();
        while let Some(item) = work.pop() {
            let i = match item {
                Item::Event(e) => {
                    out.push(e);
                    continue;
                }
                Item::Visit(i) => i,
            };
            let Some(id) = ids[i] else { continue };
            let file = &visits[i].loc;
            let at = |line: u32| Loc {
                layer: file.layer.clone(),
                path: file.path.clone(),
                lines: LineSpan::line(line),
            };
            // (line, rank, item): statements before a child pulled in at the same line.
            let mut here: Vec<(u32, usize, Item)> = Vec::new();
            for t in tasks.get(&id).into_iter().flatten() {
                here.push((
                    t.line,
                    0,
                    Item::Event(Event::Add {
                        name: t.name.clone(),
                        before: t.before.clone(),
                        after: t.after.clone(),
                        loc: at(t.line),
                    }),
                ));
            }
            for s in stmts.get(&id).into_iter().flatten() {
                if s.kind == "deltask" {
                    here.push((
                        s.loc.lines.start,
                        0,
                        Item::Event(Event::Del {
                            names: s.name.clone().unwrap_or_default(),
                            loc: s.loc.clone(),
                        }),
                    ));
                }
            }
            for (line, j) in children.get(&i).into_iter().flatten() {
                here.push((*line, 1 + *j, Item::Visit(*j)));
            }
            here.sort_by_key(|(l, r, _)| (*l, *r));
            for (_, _, item) in here.into_iter().rev() {
                work.push(item);
            }
        }
        Ok(out)
    }

    /// The pieces of a task's function as pointers; an `EXPORT_FUNCTIONS` stub also names
    /// the pieces of the function it calls.
    fn pieces(ds: &DataStore, task: &str) -> String {
        if !ds.is_function(task) {
            return "-".into();
        }
        let Some(ps) = ds.function(task) else {
            return "-".into();
        };
        if ps.is_empty() {
            return "-".into();
        }
        ps.iter()
            .map(|p| match &p.calls {
                Some(c) => {
                    let inner = ds
                        .function(c)
                        .unwrap_or_default()
                        .iter()
                        .map(|q| q.loc.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    if inner.is_empty() {
                        format!("{} -> {c}", p.loc)
                    } else {
                        format!("{} -> {c} ({inner})", p.loc)
                    }
                }
                None => p.loc.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn tasks(index: &Index, ds: &DataStore) -> Result<Table> {
        let mut t = Table::new(&["task", "state", "after", "pieces", "loc"]);
        // bb.build.addtask / deltask over __BBTASKS and the deps flags.
        let mut order: Vec<String> = Vec::new();
        let mut deps: HashMap<String, Vec<String>> = HashMap::new();
        let mut added: HashMap<String, Vec<Loc>> = HashMap::new();
        let mut deleted: Vec<(String, Loc)> = Vec::new();
        for e in task_events(index, ds)? {
            match e {
                Event::Add {
                    name,
                    before,
                    after,
                    loc,
                } => {
                    let task = prefixed(&name);
                    if !order.contains(&task) {
                        order.push(task.clone());
                    }
                    deleted.retain(|(d, _)| *d != task);
                    added.entry(task.clone()).or_default().push(loc);
                    let existing = deps.entry(task.clone()).or_default();
                    for a in &after {
                        let a = prefixed(a);
                        if !existing.contains(&a) {
                            existing.push(a);
                        }
                    }
                    for b in &before {
                        let b = prefixed(b);
                        let e = deps.entry(b).or_default();
                        if !e.contains(&task) {
                            e.insert(0, task.clone());
                        }
                    }
                }
                Event::Del { names, loc } => {
                    for n in ds.expand(&names).text.split_whitespace() {
                        let task = prefixed(n);
                        if let Some(i) = order.iter().position(|x| *x == task) {
                            order.remove(i);
                        }
                        deps.remove(&task);
                        for d in deps.values_mut() {
                            d.retain(|x| *x != task);
                        }
                        deleted.push((task, loc.clone()));
                    }
                }
            }
        }
        // Dependencies first, ties in __BBTASKS order (Kahn).
        let tasks: HashSet<&String> = order.iter().collect();
        let mut done: HashSet<String> = HashSet::new();
        let mut sorted: Vec<String> = Vec::new();
        while sorted.len() < order.len() {
            let next = order.iter().find(|x| {
                !done.contains(*x)
                    && deps
                        .get(*x)
                        .into_iter()
                        .flatten()
                        .all(|d| done.contains(d) || !tasks.contains(d))
            });
            let Some(next) = next.or_else(|| order.iter().find(|x| !done.contains(*x))) else {
                break;
            };
            done.insert(next.clone());
            sorted.push(next.clone());
        }
        let mut unknown = 0usize;
        for task in &sorted {
            let after: Vec<String> = deps
                .get(task)
                .into_iter()
                .flatten()
                .map(|d| {
                    if tasks.contains(d) {
                        d.clone()
                    } else {
                        unknown += 1;
                        format!("{d} (no such task)")
                    }
                })
                .collect();
            let state = if ds
                .flag(task, "noexec")
                .is_some_and(|v| v.text.trim() == "1")
            {
                "noexec"
            } else {
                "-"
            };
            let locs = added.get(task).cloned().unwrap_or_default();
            let loc = match locs.split_first() {
                Some((first, [])) => first.to_string(),
                Some((first, rest)) => format!("{first} (+{} more addtask)", rest.len()),
                None => "-".into(),
            };
            t.row(&[
                task,
                state,
                &if after.is_empty() {
                    "-".to_string()
                } else {
                    after.join(" ")
                },
                &pieces(ds, task),
                &loc,
            ]);
        }
        for (task, loc) in &deleted {
            t.row(&[task, "deleted", "-", "-", &loc.to_string()]);
        }
        t.note("note: order from the addtask / deltask statements in parse order (the index: configuration classes, recipe, includes and classes, bbappends, deferred inherits; the replay does not run them); dependencies first");
        t.note("note: not accounted for: tasks added or deleted by anonymous python (bb.build.addtask / deltask), do_x[deps] set directly, and dependencies on other recipes' tasks ([depends], [deptask], [rdeptask], [recrdeptask])");
        for task in &sorted {
            if let Some((op, _)) = ds.flag_history(task, "deps").into_iter().last() {
                t.note(&format!(
                    "warning: {task}[deps] is assigned at {}; the after column does not include it",
                    op.loc
                ));
            }
        }
        if unknown > 0 {
            t.note("note: (no such task): a dependency named in addtask that no addtask adds; BitBake ignores it");
        }
        replay_notes(
            &mut t,
            ds,
            &[],
            "the tasks may differ (anonymous python may add or delete tasks)",
        );
        Ok(t)
    }

    // ---- packages ----

    pub fn packages(ds: &DataStore) -> Table {
        let mut t = Table::new(&["package", "var", "value", "loc"]);
        let mut python = 0usize;
        let shown = |v: Option<crate::eval::Value>, python: &mut usize| -> String {
            match v {
                None => "(not set)".into(),
                Some(v) => {
                    let mut s = render::truncate_value(v.text.trim(), 200, None);
                    if s.is_empty() {
                        s = "(empty)".into();
                    }
                    if v.unevaluated_python {
                        *python += 1;
                        s.push_str(" (depends on python, not evaluated)");
                    }
                    s
                }
            }
        };
        let packages = ds.value("PACKAGES");
        let words = packages
            .as_ref()
            .map(|v| split_words(&v.text))
            .unwrap_or_default();
        let value = shown(packages, &mut python);
        t.row(&[
            "-",
            "PACKAGES",
            &value,
            &locs_cell(&applied_locs(ds, "PACKAGES")),
        ]);
        let dynamic = ds.value("PACKAGES_DYNAMIC");
        let has_dynamic = dynamic.as_ref().is_some_and(|v| !v.text.trim().is_empty());
        if has_dynamic {
            let value = shown(dynamic, &mut python);
            t.row(&[
                "-",
                "PACKAGES_DYNAMIC",
                &value,
                &locs_cell(&applied_locs(ds, "PACKAGES_DYNAMIC")),
            ]);
        }
        let mut skipped = 0usize;
        for pkg in &words {
            if pkg.contains("${") {
                skipped += 1;
                continue;
            }
            for var in ["FILES", "RDEPENDS", "RPROVIDES"] {
                let name = format!("{var}:{pkg}");
                let v = ds.value(&name);
                if v.is_none() && var != "FILES" {
                    continue;
                }
                let value = shown(v, &mut python);
                t.row(&[pkg, var, &value, &locs_cell(&applied_locs(ds, &name))]);
            }
        }
        t.note("note: values of the evaluated datastore (PACKAGES and FILES defaults come from bitbake.conf); RDEPENDS and RPROVIDES rows only where set");
        t.note("note: not included: what OE-Core adds while packaging (do_package: shared-library and pkg-config dependencies, packages split by python such as do_split_packages) and the RDEPENDS:${PN} that base.bbclass adds for PACKAGECONFIG with anonymous python (see deps kind=runtime)");
        if has_dynamic {
            t.note("note: PACKAGES_DYNAMIC names packages created by python during do_package; they are not listed");
        }
        if skipped > 0 {
            t.note(&format!(
                "note: {skipped} PACKAGES {} not expanded (python or an unresolved reference) and not shown",
                if skipped == 1 { "word is" } else { "words are" }
            ));
        }
        if let Some(n) = render::python_note(python) {
            t.note(&n);
        }
        replay_notes(
            &mut t,
            ds,
            &[
                "PACKAGES",
                "PACKAGES_DYNAMIC",
                "FILES",
                "RDEPENDS",
                "RPROVIDES",
            ],
            "the packages may differ",
        );
        t
    }
}

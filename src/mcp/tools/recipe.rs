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
//!
//! This file also holds the recipe helpers the other lookup tools share: [`resolve_recipe`]
//! (the `recipe` argument of `item` and `search`), [`walk_recipe`] (the recipe tree with its
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
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Recipe: `pn`, `pn_pv` or a pointer.
    pub name: String,
    /// Default: the preferred version.
    pub version: Option<String>,
    /// `summary` (default), `tree` or `outline`.
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
    };
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
                    what: format!("a recipe that {} applies to", file_label(&file)),
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

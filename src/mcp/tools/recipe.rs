//! Tool `recipe`: resolve a recipe and show its shape. See docs/mcp-tools.md.
//!
//! - `summary`: the recipe file and layer, every version (and which one is preferred by
//!   layer priority and version), the bbappends in application order, the classes inherited
//!   and a few key values (`SUMMARY`, `LICENSE`, `SRC_URI` and `DEPENDS` counts). Until the
//!   replay (M3) exists the key values are read from direct statements only, and the output
//!   says so.
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

use std::collections::HashSet;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::index::Index;
use crate::index::query::{self, EdgeRow, FileRow, RecipeRow, StatementFilter, StatementRow};
use crate::mcp::Ctx;
use crate::render::{self, Format, Table};
use crate::types::{FileKind, LineSpan, Loc};

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
    let recipe = resolve_recipe(&index, &args.name, args.version.as_deref())?;
    let mut table = match args.show {
        Show::Summary => summary(&index, &recipe, args.version.as_deref())?,
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

fn summary(index: &Index, recipe: &RecipeRow, version: Option<&str>) -> Result<Table> {
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
    let chosen = match version {
        Some(v) => format!("selected by version={v}"),
        None => "preferred by layer priority and version".into(),
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
    let mut python = 0;
    for var in ["SUMMARY", "LICENSE"] {
        let (value, loc, conditional) = last_value(&walk.statements, var);
        let mut v = match &value {
            Some(v) => render::truncate_value(v, 100, None),
            None => "(not set)".into(),
        };
        if value.as_deref().is_some_and(|v| v.contains("${@")) {
            python += 1;
        }
        if conditional > 0 {
            v.push_str(&format!(
                " (+{conditional} conditional {} not applied)",
                plural(conditional, "statement")
            ));
        }
        t.row(&[var, &v, &loc.map_or("-".into(), |l| l.to_string())]);
    }
    for var in ["SRC_URI", "DEPENDS"] {
        let c = count_words(&walk.statements, var);
        let mut v = format!(
            "{} {} in {} {}",
            c.words,
            plural(c.words, "entry"),
            c.statements,
            plural(c.statements, "statement")
        );
        if c.conditional > 0 {
            v.push_str(&format!(
                "; {} conditional {} not counted",
                c.conditional,
                plural(c.conditional, "statement")
            ));
        }
        python += c.python;
        t.row(&[var, &v, "-"]);
    }
    t.note(
        "note: key values come from direct statements only, not evaluated (no overrides, \
         expansion or python; base.bbclass and INHERIT classes not included); use var for the \
         full history",
    );
    if let Some(n) = render::python_note(python) {
        t.note(&n);
    }
    Ok(t)
}

fn plural(n: usize, word: &str) -> String {
    match (n, word) {
        (1, w) => w.to_string(),
        (_, "entry") => "entries".into(),
        (_, w) => format!("{w}s"),
    }
}

/// Assignments to `var` with no flag. `None` for an override condition other than
/// `append` / `prepend` / `remove` (a conditional statement), else the operation suffix.
fn plain_assigns<'a>(
    stmts: &'a [StatementRow],
    var: &'a str,
) -> impl Iterator<Item = (&'a StatementRow, Option<Option<&'a str>>)> + 'a {
    stmts
        .iter()
        .filter(move |s| s.kind == "assign" && s.base.as_deref() == Some(var) && s.flag.is_none())
        .map(|s| {
            let ops = ["append", "prepend", "remove"];
            let cond = s.overrides.iter().any(|o| !ops.contains(&o.as_str()));
            let suffix = s
                .overrides
                .iter()
                .find(|o| ops.contains(&o.as_str()))
                .map(String::as_str);
            (s, (!cond).then_some(suffix))
        })
}

/// The value of `var` from unconditional `=` / `:=` / `?=` / `??=` statements in parse
/// order (last `=` wins, else the first `?=`, else the last `??=`), its pointer, and how
/// many conditional statements were left out.
fn last_value(stmts: &[StatementRow], var: &str) -> (Option<String>, Option<Loc>, usize) {
    let mut set: Option<&StatementRow> = None;
    let mut default: Option<&StatementRow> = None;
    let mut weak: Option<&StatementRow> = None;
    let mut conditional = 0;
    for (s, kind) in plain_assigns(stmts, var) {
        match kind {
            None => conditional += 1,
            Some(Some(_)) => {}
            Some(None) => match s.op.as_deref() {
                Some("=") | Some(":=") => set = Some(s),
                Some("?=") if set.is_none() && default.is_none() => default = Some(s),
                Some("??=") => weak = Some(s),
                _ => {}
            },
        }
    }
    let pick = set.or(default).or(weak);
    (
        pick.and_then(|s| s.value.clone()),
        pick.map(|s| s.loc.clone()),
        conditional,
    )
}

struct WordCount {
    words: usize,
    statements: usize,
    conditional: usize,
    python: usize,
}

/// Counts the words `var` ends up with from its unconditional statements: `=` / `:=`
/// replace, `?=` sets when unset, `??=` only when nothing else set it, `+=` / `=+` / `.=`
/// / `=.` add; `:append` / `:prepend` are added and `:remove` removes at the end.
fn count_words(stmts: &[StatementRow], var: &str) -> WordCount {
    let mut words: Vec<String> = Vec::new();
    let mut set = false;
    let mut weak: Option<Vec<String>> = None;
    let mut late: Vec<String> = Vec::new();
    let mut removes: Vec<String> = Vec::new();
    let mut c = WordCount {
        words: 0,
        statements: 0,
        conditional: 0,
        python: 0,
    };
    for (s, kind) in plain_assigns(stmts, var) {
        c.statements += 1;
        let value = s.value.clone().unwrap_or_default();
        if value.contains("${@") {
            c.python += 1;
        }
        let w: Vec<String> = value.split_whitespace().map(str::to_string).collect();
        match kind {
            None => c.conditional += 1,
            Some(Some("remove")) => removes.extend(w),
            Some(Some(_)) => late.extend(w),
            Some(None) => match s.op.as_deref() {
                Some("=") | Some(":=") => {
                    words = w;
                    set = true;
                }
                Some("?=") => {
                    if !set {
                        words = w;
                        set = true;
                    }
                }
                Some("??=") => weak = Some(w),
                _ => {
                    words.extend(w);
                    set = true;
                }
            },
        }
    }
    if !set && let Some(w) = weak {
        words = w;
    }
    words.extend(late);
    words.retain(|w| !removes.contains(w));
    c.words = words.len();
    c
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

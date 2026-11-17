//! Tool `deps`: build, runtime and PACKAGECONFIG dependencies, reverse deps, providers. See docs/mcp-tools.md.
//!
//! - `build`: the words of the evaluated `DEPENDS`, each with the assignment that adds it
//!   (a class's `DEPENDS:prepend` points into the class) and the recipe that provides it
//!   (`eval::providers`: `PREFERRED_PROVIDER`, `PREFERRED_VERSION`, layer priority and
//!   version). Then the build dependencies of the selected `PACKAGECONFIG` options: OE-Core's
//!   `base.bbclass` adds them to `DEPENDS` with anonymous python, which is never run, so they
//!   are listed from the flags. `depth` follows the selected providers.
//! - `runtime`: per package of `PACKAGES`, the words of `RDEPENDS:<package>`, plus the
//!   runtime dependencies of the selected `PACKAGECONFIG` options (`base.bbclass` adds them to
//!   `RDEPENDS:${PN}`).
//! - `packageconfig`: one row per `PACKAGECONFIG[option]` flag: selected or not, the enable
//!   and disable arguments, build and runtime dependencies (fields 1 to 4 of the flag, OE-Core
//!   `base.bbclass` and the `PACKAGECONFIG` entry of the reference manual).
//! - `reverse`: recipes whose `DEPENDS`, `RDEPENDS:<pkg>` or `PACKAGECONFIG` build field
//!   names the recipe or something it provides, from the index (`depends` table: static,
//!   unconditional statements only).
//! - `providers`: `recipe` is a name such as `virtual/kernel`; every recipe that provides it,
//!   the selected one first, and the `PREFERRED_PROVIDER` / `PREFERRED_VERSION` assignments
//!   that decided it.

use std::collections::{BTreeSet, HashMap, VecDeque};

use schemars::JsonSchema;
use serde::Deserialize;

use super::var::{recipe_target, replay_notes};
use crate::error::Result;
use crate::eval::providers::{ProviderReason, ProviderSet, Providers, Setting, VersionReason};
use crate::eval::{self, DataStore, OpStatus};
use crate::index::Index;
use crate::index::query::{self, RecipeRow};
use crate::mcp::Ctx;
use crate::render::{self, Format, Table};
use crate::types::Loc;
use crate::workspace::Workspace;

/// Which dependencies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// `DEPENDS`.
    #[default]
    Build,
    /// `RDEPENDS` per package.
    Runtime,
    /// `PACKAGECONFIG` options and their effect.
    Packageconfig,
    /// Recipes depending on this one (static).
    Reverse,
    /// Providers of a name such as `virtual/kernel`, and the selected one.
    Providers,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Recipe, or for `kind: providers` a provided name such as `virtual/kernel`.
    pub recipe: String,
    /// `build` (default), `runtime`, `packageconfig`, `reverse` or `providers`.
    #[serde(default)]
    pub kind: Kind,
    /// Depth (default 1).
    pub depth: Option<u32>,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

/// Upper bound for `depth`.
const MAX_DEPTH: u32 = 5;

pub async fn run(ctx: &Ctx, args: Args) -> Result<String> {
    let depth = args.depth.unwrap_or(1).clamp(1, MAX_DEPTH);
    let mut index = ctx.index.lock().await;
    let report = index.refresh(&ctx.workspace)?;
    let ws = &ctx.workspace;
    let mut t = match args.kind {
        Kind::Providers => providers_table(&index, ws, args.recipe.trim())?,
        kind => {
            let target = recipe_target(&index, Some(&args.recipe), None, None)?;
            let ds = eval::evaluate(&index, ws, &target)?;
            let mut t = match kind {
                Kind::Build => build(&index, ws, &ds, depth)?,
                Kind::Runtime => runtime(&ds),
                Kind::Packageconfig => packageconfig(&ds),
                _ => reverse(&index, ws, &ds, depth)?,
            };
            if depth > 1 && matches!(kind, Kind::Runtime | Kind::Packageconfig) {
                t.note("note: depth applies to build and reverse only");
            }
            if kind != Kind::Reverse {
                for n in replay_notes(&ds, deps_var(kind)) {
                    t.note(&n);
                }
            }
            t
        }
    };
    drop(index);
    if let Some(line) = render::freshness_line(&report) {
        t.note(&line);
    }
    let budget = args
        .budget_tokens
        .unwrap_or(ctx.config.output.budget_tokens);
    Ok(t.render_as(args.format.unwrap_or_default(), budget))
}

fn deps_var(kind: Kind) -> &'static str {
    match kind {
        Kind::Runtime => "RDEPENDS",
        Kind::Packageconfig => "PACKAGECONFIG",
        _ => "DEPENDS",
    }
}

fn recipe_label(r: &RecipeRow) -> String {
    format!("{} {}", r.pn, r.pv)
}

fn file_loc(r: &RecipeRow) -> String {
    format!("{}:{}", r.file.layer, r.file.path.display())
}

/// The first applied assignment to `name` (or its variants) whose expanded value contains
/// `word`.
fn word_source(ds: &DataStore, name: &str, word: &str) -> Option<Loc> {
    ds.history(name)
        .into_iter()
        .filter(|(op, st)| *st == OpStatus::Applied && op.var.flag.is_none())
        .find(|(op, _)| {
            ds.expand(&op.value)
                .text
                .split_whitespace()
                .any(|w| w == word)
        })
        .map(|(op, _)| op.loc)
}

/// The last applied assignment to `name[flag]`.
fn flag_source(ds: &DataStore, name: &str, flag: &str) -> Option<Loc> {
    ds.flag_history(name, flag)
        .into_iter()
        .rev()
        .find(|(_, st)| *st == OpStatus::Applied)
        .map(|(op, _)| op.loc)
}

fn loc_str(l: Option<Loc>) -> String {
    l.map_or("-".into(), |l| l.to_string())
}

/// Dependency words with version constraints (`foo (>= 1.0)`) dropped.
fn dep_words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    for w in text.split_whitespace() {
        if w.starts_with('(') {
            depth += 1;
        }
        if depth == 0 && !w.contains('(') && !w.contains(')') {
            out.push(w.to_string());
        }
        if w.ends_with(')') && depth > 0 {
            depth -= 1;
        }
    }
    out
}

// ---- PACKAGECONFIG ----

struct PkgOption {
    name: String,
    selected: bool,
    fields: Vec<String>,
    loc: Option<Loc>,
    python: bool,
}

/// The `PACKAGECONFIG[option]` flags in sorted order (`base.bbclass` iterates
/// `sorted(pkgconfigflags.items())`), each split on `,`.
fn pkg_options(ds: &DataStore) -> (Vec<PkgOption>, Vec<String>, bool) {
    let value = ds.value("PACKAGECONFIG");
    let python = value.as_ref().is_some_and(|v| v.unevaluated_python);
    let selected: Vec<String> = value
        .map(|v| v.text.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    for flag in ds.flag_names("PACKAGECONFIG") {
        let Some(v) = ds.flag("PACKAGECONFIG", &flag) else {
            continue;
        };
        out.push(PkgOption {
            selected: selected.contains(&flag),
            fields: v.text.split(',').map(|s| s.trim().to_string()).collect(),
            loc: flag_source(ds, "PACKAGECONFIG", &flag),
            python: v.unevaluated_python,
            name: flag,
        });
    }
    let unknown: Vec<String> = selected
        .into_iter()
        .filter(|s| !out.iter().any(|o| o.name == *s))
        .collect();
    (out, unknown, python)
}

impl PkgOption {
    fn field(&self, i: usize) -> &str {
        self.fields.get(i).map_or("", String::as_str)
    }
}

const PACKAGECONFIG_NOTE: &str = "note: PACKAGECONFIG rows: OE-Core's base.bbclass adds the build (field 3) and runtime (field 4) dependencies of the selected options with anonymous python, which is not run; they are listed from the flags";

fn packageconfig(ds: &DataStore) -> Table {
    let mut t = Table::new(&[
        "option", "selected", "enable", "disable", "build", "runtime", "loc",
    ]);
    let (opts, unknown, python) = pkg_options(ds);
    let dash = |s: &str| {
        if s.is_empty() {
            "-".to_string()
        } else {
            s.to_string()
        }
    };
    let mut extra = 0;
    for o in &opts {
        if o.fields.len() > 4 {
            extra += 1;
        }
        let mut sel = if o.selected { "yes" } else { "no" }.to_string();
        if o.python {
            sel.push_str(" (flag value has python)");
        }
        t.row(&[
            &o.name,
            &sel,
            &dash(o.field(0)),
            &dash(o.field(1)),
            &dash(o.field(2)),
            &dash(o.field(3)),
            &loc_str(o.loc.clone()),
        ]);
    }
    for u in &unknown {
        t.row(&[
            u,
            "yes (no PACKAGECONFIG flag: ignored)",
            "-",
            "-",
            "-",
            "-",
            "-",
        ]);
    }
    let value_loc = ds
        .history("PACKAGECONFIG")
        .into_iter()
        .rev()
        .find(|(op, st)| *st == OpStatus::Applied && op.var.flag.is_none())
        .map(|(op, _)| op.loc);
    let value = ds
        .value("PACKAGECONFIG")
        .map(|v| v.text)
        .unwrap_or_default();
    t.note(&format!(
        "note: PACKAGECONFIG = \"{}\" ({})",
        value.trim(),
        loc_str(value_loc)
    ));
    if python {
        t.note("note: PACKAGECONFIG depends on python, which is not evaluated; the selection may differ");
    }
    if extra > 0 {
        t.note("note: fields 5 (RRECOMMENDS) and 6 (conflicting options) are not shown; use var PACKAGECONFIG[option]");
    }
    t
}

// ---- build ----

struct Resolved {
    provider: String,
    recipe: Option<RecipeRow>,
}

fn resolve_dep(
    p: &Providers<'_>,
    cache: &mut HashMap<String, Resolved>,
    name: &str,
) -> Result<String> {
    if !cache.contains_key(name) {
        let set = p.providers(name)?;
        let r = match set.selected() {
            None => Resolved {
                provider: "-".into(),
                recipe: None,
            },
            Some(sel) => {
                let mut s = recipe_label(&sel.selection.recipe);
                match set.reason {
                    ProviderReason::PreferredProvider => {
                        s.push_str(&format!(" (PREFERRED_PROVIDER_{name})"))
                    }
                    ProviderReason::FirstOfSeveral => s.push_str(" (first of several providers)"),
                    _ => {}
                }
                Resolved {
                    provider: s,
                    recipe: Some(sel.selection.recipe.clone()),
                }
            }
        };
        cache.insert(name.to_string(), r);
    }
    Ok(cache.get(name).map_or("-".into(), |r| r.provider.clone()))
}

fn build(index: &Index, ws: &Workspace, ds: &DataStore, depth: u32) -> Result<Table> {
    let mut t = Table::new(&["depth", "dep", "provider", "source", "loc"]);
    let p = Providers::new(index, ws, None, None)?;
    let mut cache: HashMap<String, Resolved> = HashMap::new();
    let mut queue: VecDeque<(u32, DataStore)> = VecDeque::from([(1, ds.clone())]);
    let mut seen: BTreeSet<i64> = BTreeSet::new();
    let mut python = 0;
    let mut pkgconfig = false;
    let mut missing = 0;
    while let Some((d, data)) = queue.pop_front() {
        let mut next: Vec<RecipeRow> = Vec::new();
        let value = data.value("DEPENDS");
        if value.as_ref().is_some_and(|v| v.unevaluated_python) {
            python += 1;
        }
        let words = value.map(|v| dep_words(&v.text)).unwrap_or_default();
        let mut rows: Vec<(String, String, Option<Loc>)> = words
            .iter()
            .map(|w| {
                (
                    w.clone(),
                    "DEPENDS".to_string(),
                    word_source(&data, "DEPENDS", w),
                )
            })
            .collect();
        let (opts, _, _) = pkg_options(&data);
        for o in opts.iter().filter(|o| o.selected) {
            for w in dep_words(o.field(2)) {
                if !rows.iter().any(|(x, _, _)| *x == w) {
                    pkgconfig = true;
                    rows.push((w, format!("PACKAGECONFIG[{}]", o.name), o.loc.clone()));
                }
            }
        }
        for (w, source, loc) in rows {
            let provider = resolve_dep(&p, &mut cache, &w)?;
            if provider == "-" {
                missing += 1;
            }
            t.row(&[&d.to_string(), &w, &provider, &source, &loc_str(loc)]);
            if let Some(r) = cache.get(&w).and_then(|r| r.recipe.clone())
                && d < depth
                && seen.insert(r.file.id)
            {
                next.push(r);
            }
        }
        for r in next {
            queue.push_back((d + 1, p.evaluate(&r)?));
        }
    }
    if pkgconfig {
        t.note(PACKAGECONFIG_NOTE);
    }
    if missing > 0 {
        t.note("note: provider - means no recipe in the workspace provides the name (BBCLASSEXTEND variants such as -native are not resolved)");
    }
    if let Some(n) = render::python_note(python) {
        t.note(&n);
    }
    Ok(t)
}

// ---- runtime ----

fn runtime(ds: &DataStore) -> Table {
    let mut t = Table::new(&["package", "dep", "source", "loc"]);
    let packages: Vec<String> = ds
        .value("PACKAGES")
        .map(|v| v.text.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    let pn = ds.value("PN").map(|v| v.text).unwrap_or_default();
    let mut python = 0;
    for pkg in &packages {
        let name = format!("RDEPENDS:{pkg}");
        if let Some(v) = ds.value(&name) {
            if v.unevaluated_python {
                python += 1;
            }
            for w in dep_words(&v.text) {
                let loc = word_source(ds, &name, &w);
                t.row(&[pkg, &w, &name, &loc_str(loc)]);
            }
        }
    }
    let (opts, _, _) = pkg_options(ds);
    let mut pkgconfig = false;
    for o in opts.iter().filter(|o| o.selected) {
        for w in dep_words(o.field(3)) {
            pkgconfig = true;
            t.row(&[
                &pn,
                &w,
                &format!("PACKAGECONFIG[{}]", o.name),
                &loc_str(o.loc.clone()),
            ]);
        }
    }
    if t.is_empty() {
        t.note(&format!(
            "note: no RDEPENDS for the packages {}",
            packages.join(" ")
        ));
    }
    if pkgconfig {
        t.note(PACKAGECONFIG_NOTE);
    }
    if let Some(n) = render::python_note(python) {
        t.note(&n);
    }
    t
}

// ---- reverse ----

fn reverse(index: &Index, ws: &Workspace, ds: &DataStore, depth: u32) -> Result<Table> {
    let mut t = Table::new(&["depth", "recipe", "kind", "target", "loc", "status"]);
    let p = Providers::new(index, ws, None, None)?;
    let pn = ds.value("PN").map(|v| v.text).unwrap_or_default();
    let mut queue: VecDeque<(u32, String, Vec<String>)> = VecDeque::new();
    queue.push_back((1, pn.clone(), provided_names(ds, &pn)));
    let mut done: BTreeSet<String> = BTreeSet::from([pn]);
    let mut selected: HashMap<String, Option<String>> = HashMap::new();
    while let Some((d, of, names)) = queue.pop_front() {
        for name in &names {
            for row in query::depends_on(index, name)? {
                let kind = match &row.pkg {
                    Some(pkg) if row.kind == "PACKAGECONFIG" => format!("PACKAGECONFIG[{pkg}]"),
                    Some(pkg) => format!("{}:{pkg}", row.kind),
                    None => row.kind.clone(),
                };
                let status = if *name == of {
                    "by name".to_string()
                } else {
                    if !selected.contains_key(name) {
                        let s = p.providers(name)?.selected().map(|s| s.pn.clone());
                        selected.insert(name.clone(), s);
                    }
                    match selected.get(name).cloned().flatten() {
                        Some(s) if s == of => format!("via {name} (selected provider)"),
                        Some(s) => format!("via {name} (selected provider is {s})"),
                        None => format!("via {name}"),
                    }
                };
                let loc = Loc {
                    layer: row.file.layer.clone(),
                    path: row.file.path.clone(),
                    lines: crate::types::LineSpan::line(row.line),
                };
                for user in eval::providers::recipes_using(index, &row.file)? {
                    t.row(&[
                        &d.to_string(),
                        &recipe_label(&user),
                        &kind,
                        name,
                        &loc.to_string(),
                        &status,
                    ]);
                    if d < depth && done.insert(user.pn.clone()) {
                        let names = match p.evaluate(&user) {
                            Ok(uds) => provided_names(&uds, &user.pn),
                            Err(_) => vec![user.pn.clone()],
                        };
                        queue.push_back((d + 1, user.pn.clone(), names));
                    }
                }
            }
        }
    }
    if t.is_empty() {
        t.note("note: no recipe names it in DEPENDS, RDEPENDS or a PACKAGECONFIG build field");
    }
    t.note("note: static: unconditional DEPENDS / RDEPENDS:<package> statements and PACKAGECONFIG build fields as written in the index (names with ${...} and conditional statements are not seen)");
    Ok(t)
}

/// `PN` and the words of the evaluated `PROVIDES`.
fn provided_names(ds: &DataStore, pn: &str) -> Vec<String> {
    let mut out = vec![pn.to_string()];
    if let Some(v) = ds.value("PROVIDES") {
        for w in v.text.split_whitespace() {
            if !out.iter().any(|o| o == w) {
                out.push(w.to_string());
            }
        }
    }
    out
}

// ---- providers ----

fn setting_row(t: &mut Table, s: &Setting, status: &str) {
    let mut v = s.value.clone();
    if s.python {
        v.push_str(" (python)");
    }
    t.row(&[&s.name, &v, status, &loc_str(s.loc.clone())]);
}

fn providers_table(index: &Index, ws: &Workspace, name: &str) -> Result<Table> {
    let p = Providers::new(index, ws, None, None)?;
    let set: ProviderSet = p.providers(name)?;
    let mut t = Table::new(&["name", "version", "status", "loc"]);
    for (i, prov) in set.providers.iter().enumerate() {
        let status = if i == 0 {
            match set.reason {
                ProviderReason::PreferredProvider => {
                    format!("selected by PREFERRED_PROVIDER_{name}")
                }
                ProviderReason::Only => "selected (only provider)".into(),
                ProviderReason::SameName => format!("selected (its PN is {name})"),
                ProviderReason::FirstOfSeveral => format!(
                    "selected: first of several, no PREFERRED_PROVIDER_{name} (BitBake notes multiple providers)"
                ),
                ProviderReason::None => "-".into(),
            }
        } else {
            "not selected".into()
        };
        let loc = prov
            .provides_loc
            .clone()
            .map_or_else(|| file_loc(&prov.selection.recipe), |l| l.to_string());
        let version = format!(
            "{} ({})",
            prov.selection.recipe.pv,
            prov.selection.describe()
        );
        t.row(&[&prov.pn, &version, &status, &loc]);
    }
    if let Some(pp) = &set.preferred_provider {
        let used = set.reason == ProviderReason::PreferredProvider;
        setting_row(
            &mut t,
            pp,
            if used { "applied" } else { "names no provider" },
        );
    }
    for prov in &set.providers {
        if let Some(s) = &prov.selection.setting {
            let status = match prov.selection.reason {
                VersionReason::Preferred | VersionReason::Required => "applied",
                _ => "matches no version",
            };
            setting_row(&mut t, s, status);
        }
    }
    for r in &set.excluded {
        t.row(&[
            &r.pn,
            &r.pv,
            "indexed as a provider, but its evaluated PROVIDES does not name it",
            &file_loc(r),
        ]);
    }
    if set.providers.is_empty() {
        t.note(&format!("note: no recipe in the workspace provides {name}"));
    }
    for n in &set.notes {
        t.note(&format!("note: {n}"));
    }
    for prov in &set.providers {
        for n in &prov.selection.notes {
            t.note(&format!("note: {n}"));
        }
    }
    t.note("note: candidates are recipes whose PN or static PROVIDES names it (index), confirmed by evaluating each; a PROVIDES that needs expansion to name it is not found");
    Ok(t)
}

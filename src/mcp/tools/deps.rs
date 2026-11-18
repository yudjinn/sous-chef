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
//! - `mcdepends`: the cross-configuration task dependencies of the recipe
//!   (`do_x[mcdepends] = "mc:<from>:<to>:<recipe>:<task>"`, `eval::multiconfig::mcdepends`),
//!   one row per entry with its pointer and whether it applies in the configuration
//!   evaluated. They are listed, not followed: the other configuration is not evaluated.
//!
//! `mc` (or a recipe written `mc:<name>:<recipe>`) evaluates in that multiconfig, provider
//! selection included (`eval::providers` per configuration). `reverse` stays static (the
//! index, which is the same for every configuration).
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use schemars::JsonSchema;
use serde::Deserialize;

use super::var::{mc_notes, recipe_target_in, replay_notes};
use crate::error::Result;
use crate::eval::providers::{
    ProviderReason, ProviderSet, Providers, Selection, Setting, VersionReason,
};
use crate::eval::{self, ClassExtend, DataStore, OpStatus};
use crate::index::Index;
use crate::index::query::{self, RecipeRow};
use crate::mcp::Ctx;
use crate::parse::words;
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
    /// Cross-configuration task dependencies (`mcdepends`), listed, not followed.
    Mcdepends,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Recipe, or for `kind: providers` a provided name such as `virtual/kernel`.
    pub recipe: String,
    /// `build` (default), `runtime`, `packageconfig`, `reverse`, `providers` or `mcdepends`.
    #[serde(default)]
    pub kind: Kind,
    /// Multiconfig to evaluate in (default: the default configuration); a recipe written
    /// `mc:<name>:<recipe>` also selects it.
    pub mc: Option<String>,
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
        Kind::Providers => {
            let split = eval::multiconfig::normalize_target(&eval::Target {
                recipe: Some(args.recipe.trim().to_string()),
                mc: args.mc.clone(),
                ..eval::Target::default()
            })?;
            let name = split.recipe.clone().unwrap_or_default();
            let mut t = providers_table(&index, ws, &name, split.mc.as_deref())?;
            if split.mc.is_some() {
                // The configuration Providers::new_in evaluated (cached), for its note.
                let cfg = eval::evaluate(
                    &index,
                    ws,
                    &eval::Target {
                        recipe: None,
                        ..split.clone()
                    },
                )?;
                for n in mc_notes(&cfg) {
                    t.note(&n);
                }
            }
            t
        }
        kind => {
            let target =
                recipe_target_in(&index, Some(&args.recipe), None, None, args.mc.as_deref())?;
            let ds = eval::evaluate(&index, ws, &target)?;
            let mc = target.mc.as_deref();
            let mut t = match kind {
                Kind::Build => build(&index, ws, &ds, depth, mc)?,
                Kind::Runtime => runtime(&ds),
                Kind::Packageconfig => packageconfig(&ds),
                Kind::Mcdepends => mcdepends_table(&ds),
                _ => reverse(&index, ws, &ds, depth, mc)?,
            };
            if kind == Kind::Reverse {
                for n in mc_notes(&ds) {
                    t.note(&n);
                }
            }
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
        .find(|(op, _)| words::words(&ds.expand(&op.value).text).contains(&word))
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

/// Dependency words with version constraints (`foo (>= 1.0)`) dropped. An unevaluated
/// `${@...}` (or undefined `${VAR}`) is one word, kept whole (`parse::words`); the caller
/// shows it as what it is and never resolves it.
fn dep_words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    for w in words::words(text) {
        if words::is_unevaluated(w) {
            if depth == 0 {
                out.push(w.to_string());
            } else if w.ends_with(')') {
                // `(>= ${PV})`
                depth -= 1;
            }
            continue;
        }
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

/// Flags BitBake itself gives a meaning on any variable (BitBake 2.18.0: `doc`, read by
/// `cooker.py` `getAllKeysWithFlags(["doc", "func"])` and set by OE-core's
/// `documentation.conf`; `vardeps`, `vardepvalue`, `vardepsexclude`, `vardepvalueexclude`,
/// `exports`, `prefuncs`, `postfuncs`, `lineno`, `filename`, read by `data.py`
/// `build_dependencies`; `export`, `unexport`, `func`, `python`, read by `data.py` /
/// `data_smart.py`). OE-core's `bitbake.conf:942` sets `PACKAGECONFIG[vardepvalue] =
/// "${PACKAGECONFIG}"` and `documentation.conf:311` sets `PACKAGECONFIG[doc]`.
const BITBAKE_FLAGS: &[&str] = &[
    "doc",
    "vardeps",
    "vardepvalue",
    "vardepsexclude",
    "vardepvalueexclude",
    "exports",
    "prefuncs",
    "postfuncs",
    "lineno",
    "filename",
    "export",
    "unexport",
    "func",
    "python",
];

/// The `PACKAGECONFIG[option]` flags in sorted order (`base.bbclass` iterates
/// `sorted(pkgconfigflags.items())`), each split on `,` (a `${@...}` kept whole). Returns
/// the options, the selected words with no flag, the unevaluated words of `PACKAGECONFIG`,
/// the BitBake flags left out, and whether `PACKAGECONFIG` holds python.
///
/// `base.bbclass` iterates every flag `getVarFlags` returns (internal `_`/`:` flags only are
/// left out), so a BitBake flag such as `vardepvalue` goes through the same loop; it adds
/// nothing unless its name is a word of `PACKAGECONFIG` (fields 1, 3, 4, 5) or its value has
/// a non-empty second comma field (added to `PACKAGECONFIG_CONFARGS` when not selected). Only
/// then is it listed as an option.
fn pkg_options(ds: &DataStore) -> PkgOptions {
    let value = ds.value("PACKAGECONFIG");
    let python = value.as_ref().is_some_and(|v| v.unevaluated_python);
    let all: Vec<String> = value
        .map(|v| {
            words::words(&v.text)
                .into_iter()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let (unevaluated, selected): (Vec<String>, Vec<String>) =
        all.into_iter().partition(|w| words::is_unevaluated(w));
    let mut out = Vec::new();
    let mut bitbake_flags = Vec::new();
    for flag in ds.flag_names("PACKAGECONFIG") {
        let Some(v) = ds.flag("PACKAGECONFIG", &flag) else {
            continue;
        };
        let fields: Vec<String> = words::comma_fields(&v.text)
            .into_iter()
            .map(|s| s.trim().to_string())
            .collect();
        let is_selected = selected.contains(&flag);
        if BITBAKE_FLAGS.contains(&flag.as_str())
            && !is_selected
            && fields.get(1).is_none_or(|f| f.is_empty())
        {
            bitbake_flags.push(flag);
            continue;
        }
        out.push(PkgOption {
            selected: is_selected,
            fields,
            loc: flag_source(ds, "PACKAGECONFIG", &flag),
            python: v.unevaluated_python,
            name: flag,
        });
    }
    let unknown: Vec<String> = selected
        .into_iter()
        .filter(|s| !out.iter().any(|o| o.name == *s))
        .collect();
    PkgOptions {
        options: out,
        unknown,
        unevaluated,
        bitbake_flags,
        python,
    }
}

struct PkgOptions {
    options: Vec<PkgOption>,
    unknown: Vec<String>,
    unevaluated: Vec<String>,
    bitbake_flags: Vec<String>,
    python: bool,
}

impl PkgOption {
    fn field(&self, i: usize) -> &str {
        self.fields.get(i).map_or("", String::as_str)
    }
}

/// The provider column of a word that holds unevaluated python or an undefined variable.
const UNEVALUATED: &str = "unknown: not evaluated (python or an undefined variable); stands for dependencies that are not known";

const PACKAGECONFIG_NOTE: &str = "note: PACKAGECONFIG rows: OE-Core's base.bbclass adds the build (field 3) and runtime (field 4) dependencies of the selected options with anonymous python, which is not run; they are listed from the flags";

fn packageconfig(ds: &DataStore) -> Table {
    let mut t = Table::new(&[
        "option", "selected", "enable", "disable", "build", "runtime", "loc",
    ]);
    let PkgOptions {
        options: opts,
        unknown,
        unevaluated,
        bitbake_flags,
        python,
    } = pkg_options(ds);
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
    for u in &unevaluated {
        t.row(&[
            &words::abbreviate(u),
            "unknown (python in PACKAGECONFIG, not evaluated: it may select options)",
            "-",
            "-",
            "-",
            "-",
            "-",
        ]);
    }
    if !bitbake_flags.is_empty() {
        t.note(&format!(
            "note: not options: PACKAGECONFIG[{}] {} BitBake's own variable {} (documentation / signature); base.bbclass loops over {} too, but {} adds nothing",
            bitbake_flags.join("], ["),
            if bitbake_flags.len() == 1 { "is" } else { "are" },
            if bitbake_flags.len() == 1 { "flag" } else { "flags" },
            if bitbake_flags.len() == 1 { "it" } else { "them" },
            if bitbake_flags.len() == 1 { "it" } else { "each" },
        ));
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
    /// The selected provider (its file, or the class-extended variant made from it).
    selection: Option<Selection>,
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
                selection: None,
            },
            Some(sel) => {
                let mut s = recipe_label(&sel.selection.recipe);
                if let Some(ext) = &sel.selection.variant {
                    s = format!("{} ({} of {s})", sel.pn, ext.describe());
                }
                match set.reason {
                    ProviderReason::PreferredProvider => {
                        s.push_str(&format!(" (PREFERRED_PROVIDER_{name})"))
                    }
                    ProviderReason::PreferredProviders => {
                        s.push_str(&format!(" (PREFERRED_PROVIDERS {name}:{})", sel.pn))
                    }
                    ProviderReason::FirstOfSeveral => s.push_str(" (first of several providers)"),
                    _ => {}
                }
                Resolved {
                    provider: s,
                    selection: Some(sel.selection.clone()),
                }
            }
        };
        cache.insert(name.to_string(), r);
    }
    Ok(cache.get(name).map_or("-".into(), |r| r.provider.clone()))
}

fn build(
    index: &Index,
    ws: &Workspace,
    ds: &DataStore,
    depth: u32,
    mc: Option<&str>,
) -> Result<Table> {
    let mut t = Table::new(&["depth", "dep", "provider", "source", "loc"]);
    let p = Providers::new_in(index, ws, None, None, mc)?;
    let mut cache: HashMap<String, Resolved> = HashMap::new();
    let mut queue: VecDeque<(u32, DataStore)> = VecDeque::from([(1, ds.clone())]);
    let mut seen: HashSet<(i64, Option<ClassExtend>)> = HashSet::new();
    let mut python = 0;
    let mut pkgconfig = false;
    let mut missing = 0;
    while let Some((d, data)) = queue.pop_front() {
        let mut next: Vec<Selection> = Vec::new();
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
        let opts = pkg_options(&data).options;
        for o in opts.iter().filter(|o| o.selected) {
            for w in dep_words(o.field(2)) {
                if !rows.iter().any(|(x, _, _)| *x == w) {
                    pkgconfig = true;
                    rows.push((w, format!("PACKAGECONFIG[{}]", o.name), o.loc.clone()));
                }
            }
        }
        for (w, source, loc) in rows {
            if words::is_unevaluated(&w) {
                // Not a dependency name: stands for words that are not known.
                t.row(&[
                    &d.to_string(),
                    &words::abbreviate(&w),
                    UNEVALUATED,
                    &source,
                    &loc_str(loc),
                ]);
                continue;
            }
            let provider = resolve_dep(&p, &mut cache, &w)?;
            if provider == "-" {
                missing += 1;
            }
            t.row(&[&d.to_string(), &w, &provider, &source, &loc_str(loc)]);
            if let Some(r) = cache.get(&w).and_then(|r| r.selection.clone())
                && d < depth
                && seen.insert((r.recipe.file.id, r.variant.clone()))
            {
                next.push(r);
            }
        }
        for r in next {
            queue.push_back((d + 1, p.evaluate_selection(&r)?));
        }
    }
    if pkgconfig {
        t.note(PACKAGECONFIG_NOTE);
    }
    if missing > 0 {
        t.note("note: provider - means no recipe in the workspace provides the name (nor a BBCLASSEXTEND variant of a recipe whose BBCLASSEXTEND lists the class, by its name or through the PROVIDES its class rewrites)");
    }
    if let Some(n) = render::python_note(python) {
        t.note(&n);
    }
    let cross = eval::multiconfig::mcdepends(ds);
    if !cross.is_empty() {
        t.note(&format!(
            "note: {} cross-configuration task {} (mcdepends) not listed here; kind=mcdepends lists {}",
            cross.len(),
            if cross.len() == 1 {
                "dependency"
            } else {
                "dependencies"
            },
            if cross.len() == 1 { "it" } else { "them" }
        ));
    }
    Ok(t)
}

// ---- mcdepends ----

/// One row per `mcdepends` entry of the recipe's tasks (`eval::multiconfig::mcdepends`).
fn mcdepends_table(ds: &DataStore) -> Table {
    let mut t = Table::new(&["task", "from", "to", "recipe", "dep_task", "status", "loc"]);
    let current = ds
        .value("BB_CURRENT_MC")
        .map(|v| v.text)
        .unwrap_or_default();
    let all = eval::multiconfig::mcdepends(ds);
    let dash = |s: &Option<String>| match s.as_deref() {
        Some("") => "(default)".to_string(),
        Some(v) => v.to_string(),
        None => "-".to_string(),
    };
    for d in &all {
        let mut status = if d.from.is_none() {
            format!(
                "invalid: {:?} does not have the form mc:<from>:<to>:<recipe>:<task> (BitBake stops: \"does not contain exactly four ':' characters\")",
                d.entry
            )
        } else if d.applies {
            "applies here".to_string()
        } else {
            format!(
                "not in this configuration (BitBake adds it only when building in {})",
                dash(&d.from)
            )
        };
        if d.uncertain {
            status.push_str("; depends on python or undefined variables, not evaluated");
        }
        let loc = if d.locs.is_empty() {
            "-".to_string()
        } else {
            d.locs
                .iter()
                .map(Loc::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        t.row(&[
            &d.task,
            &dash(&d.from),
            &dash(&d.to),
            &dash(&d.recipe),
            &dash(&d.dep_task),
            &status,
            &loc,
        ]);
    }
    if all.is_empty() {
        t.note("note: no task of this recipe has an mcdepends flag (tasks added by anonymous python are not seen)");
    }
    t.note(&format!(
        "note: evaluated in {}; the dependencies are listed, not followed: the recipe each names is resolved by BitBake with the providers of the `to` configuration (evaluate it with mc=<to>)",
        if current.is_empty() {
            "the default configuration".to_string()
        } else {
            format!("multiconfig {current}")
        }
    ));
    t
}

// ---- runtime ----

fn runtime(ds: &DataStore) -> Table {
    let mut t = Table::new(&["package", "dep", "source", "loc"]);
    let all: Vec<String> = ds
        .value("PACKAGES")
        .map(|v| {
            words::words(&v.text)
                .into_iter()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let (unknown_pkgs, packages): (Vec<String>, Vec<String>) =
        all.into_iter().partition(|w| words::is_unevaluated(w));
    if !unknown_pkgs.is_empty() {
        t.note(&format!(
            "note: PACKAGES holds python that is not evaluated ({}); the packages it adds, and their RDEPENDS, are not listed",
            unknown_pkgs
                .iter()
                .map(|u| words::abbreviate(u))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
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
                if words::is_unevaluated(&w) {
                    let shown = format!("{} ({UNEVALUATED})", words::abbreviate(&w));
                    t.row(&[pkg, &shown, &name, &loc_str(loc)]);
                } else {
                    t.row(&[pkg, &w, &name, &loc_str(loc)]);
                }
            }
        }
    }
    let opts = pkg_options(ds).options;
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

fn reverse(
    index: &Index,
    ws: &Workspace,
    ds: &DataStore,
    depth: u32,
    mc: Option<&str>,
) -> Result<Table> {
    let mut t = Table::new(&["depth", "recipe", "kind", "target", "loc", "status"]);
    let p = Providers::new_in(index, ws, None, None, mc)?;
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
        for w in words::known_words(&v.text) {
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

fn providers_table(index: &Index, ws: &Workspace, name: &str, mc: Option<&str>) -> Result<Table> {
    let p = Providers::new_in(index, ws, None, None, mc)?;
    let set: ProviderSet = p.providers(name)?;
    let mut t = Table::new(&["name", "version", "status", "loc"]);
    for (i, prov) in set.providers.iter().enumerate() {
        let status = if i == 0 {
            match set.reason {
                ProviderReason::PreferredProvider => {
                    format!("selected by PREFERRED_PROVIDER_{name}")
                }
                ProviderReason::PreferredProviders => {
                    format!("selected by PREFERRED_PROVIDERS ({name}:{})", prov.pn)
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
        let used = matches!(
            set.reason,
            ProviderReason::PreferredProvider | ProviderReason::PreferredProviders
        );
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

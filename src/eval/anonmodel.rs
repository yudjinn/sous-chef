//! Exact models of a few OE-core anonymous python functions.
//!
//! The replay never runs anonymous python (`replay.rs`, `Replay::anonymous`): every variable
//! a function writes by name is flagged. For the functions below, which make most of the
//! remaining wrong values of the differential test (docs/differential.md, "After
//! acc-anon-models"), the replay reproduces the writes instead, at the point BitBake runs
//! the function, under three conditions:
//!
//! - **The function is the verified text.** A model applies only when the anonymous
//!   function's text (the lines of its statement, `python () {` to `}`) is, byte for byte
//!   (`\r` line ends and trailing blank lines aside, [`classfn::same_text`]), one of the
//!   texts in `anonmodel/`, copied from OE-core at the commits the live workspaces use
//!   (Wrynose `33a6bd6cc8`, Scarthgap `fc4316861b`, Kirkstone `9687b9409d`). The `def`
//!   functions a model's function calls must be the verified text too, as the last `def` of
//!   their name the parse executed (BitBake keeps one global method pool, so the last one
//!   wins: `ReplayNotes::model_defs`). A text that differs in any byte is not modelled, and
//!   its writes stay flagged as before.
//! - **Every input is certain.** A model reads the datastore as the function would when it
//!   runs: the static values plus the writes of the functions before it. A value is read
//!   only when it is certain (the evaluator's usual rules: no python left, no undefined
//!   variable, no unknown override decision, no file the replay could not read) and no
//!   earlier anonymous function that is not modelled may write it by name (functions after
//!   it run later and cannot change what it reads). When an input is not certain, or the
//!   function would stop with an error (`bb.error`, `bb.fatal`, an exception), the model
//!   gives nothing, and the writes stay flagged with the reason ([`Applied::Refused`]).
//! - **What it writes is what BitBake stores.** A write is reproduced as the datastore
//!   operation BitBake's method performs: `d.appendVar(v, x)` is `setVar(v + ":append", x,
//!   parsing=True)` and `d.prependVar` the `:prepend` form (BitBake `data_smart.py`, the same
//!   in 2.0, 2.8 and 2.18), so a later `:remove` still applies; `d.setVar(v, x)` outside
//!   parsing replaces the value and drops `v`'s `:append` / `:prepend` / `:remove` lists and
//!   its active override variants ([`DataStore::rewrite`]); `d.setVarFlag` sets the flag.
//!   Text is stored unexpanded where python passes unexpanded text, so `${...}` in it is
//!   expanded when the value is read, as in BitBake.
//!
//! A model may reproduce only some of a function's writes (the kernel function's
//! `PACKAGES`, not its per-image `FILES:` / `RDEPENDS:` writes); the others stay flagged.
//! The modelled names leave `python_vars` only when no other function that is not modelled
//! may write them. The sources and the semantics of each model are in
//! docs/bitbake-semantics.md, "Anonymous python reproduced exactly"; each model's results
//! are checked against the real function run with BitBake's datastore
//! (`tests/fixtures/anon_models/oracle.py`, `tests/eval_anon_models.rs`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::DataStore;
use super::classfn::same_text;
use super::pyexpr::PyVal;

/// A modelled anonymous function, by the verified text it matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Model {
    /// `base.bbclass`'s anonymous python: its `PACKAGECONFIG` handling.
    BasePackageconfig(BaseText),
    /// `cmake.bbclass`'s anonymous python (`OECMAKE_GENERATOR`).
    Cmake,
    /// `update-alternatives.bbclass`'s `python __anonymous()`.
    UpdateAlternatives(UaText),
    /// `packagegroup.bbclass`'s first anonymous function (`-dbg`, `-dev`, `-ptest`).
    Packagegroup,
    /// `kernel.bbclass`'s `python __anonymous()`: `KERNEL_IMAGETYPES`,
    /// `KERNEL_IMAGETYPE_FOR_MAKE`, `STAGING_KERNEL_DIR` / `_BUILDDIR` and `PACKAGES`.
    Kernel,
    /// `kernel-devicetree.bbclass`'s anonymous python (`PACKAGES`).
    KernelDevicetree,
    /// The `python3` recipe's manifest function (`python3-manifest.json`).
    Python3Manifest(PyText),
}

/// Which `base.bbclass` text: the wrapper's two lines differ (Wrynose filters
/// `RRECOMMENDS` and `cross` too).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BaseText {
    Wrynose,
    Scarthgap,
    Kirkstone,
}

/// Which `update-alternatives.bbclass` text: Scarthgap and Kirkstone call
/// `gen_updatealternativesvardeps(d)` first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UaText {
    Wrynose,
    ScarthgapKirkstone,
}

/// Which `python3` manifest function: Scarthgap and Kirkstone clear `FILES:<pkg>` first and
/// set no `ALLOW_EMPTY:${PN}-fcntl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PyText {
    Wrynose,
    ScarthgapKirkstone,
}

const BASE_WRYNOSE: &str = include_str!("anonmodel/base-wrynose.txt");
const BASE_SCARTHGAP: &str = include_str!("anonmodel/base-scarthgap.txt");
const BASE_KIRKSTONE: &str = include_str!("anonmodel/base-kirkstone.txt");
const CMAKE: &str = include_str!("anonmodel/cmake.txt");
const UA_WRYNOSE: &str = include_str!("anonmodel/ua-wrynose.txt");
const UA_SCARTHGAP: &str = include_str!("anonmodel/ua-scarthgap.txt");
const PACKAGEGROUP: &str = include_str!("anonmodel/packagegroup.txt");
const KERNEL_WRYNOSE: &str = include_str!("anonmodel/kernel-wrynose.txt");
const KERNEL_SCARTHGAP: &str = include_str!("anonmodel/kernel-scarthgap.txt");
const KERNEL_KIRKSTONE: &str = include_str!("anonmodel/kernel-kirkstone.txt");
const KERNEL_DEVICETREE: &str = include_str!("anonmodel/kernel-devicetree.txt");
const PYTHON3_WRYNOSE: &str = include_str!("anonmodel/python3-wrynose.txt");
const PYTHON3_SCARTHGAP: &str = include_str!("anonmodel/python3-scarthgap.txt");

/// The `def` functions a modelled function calls, with their verified texts (the same on the
/// three releases).
pub(crate) const DEFS: &[(&str, &str)] = &[
    (
        "update_alternatives_enabled",
        include_str!("anonmodel/update_alternatives_enabled.txt"),
    ),
    (
        "ua_extend_depends",
        include_str!("anonmodel/ua_extend_depends.txt"),
    ),
    (
        "gen_updatealternativesvardeps",
        include_str!("anonmodel/gen_updatealternativesvardeps.txt"),
    ),
];

/// Whether `name` is a `def` whose text a model checks.
pub(crate) fn is_model_def(name: &str) -> bool {
    DEFS.iter().any(|(n, _)| *n == name)
}

/// Whether `text` is the verified text of the `def` `name`.
pub(crate) fn def_verified(name: &str, text: &str) -> bool {
    DEFS.iter().any(|(n, t)| *n == name && same_text(text, t))
}

/// The model of the anonymous function whose text is `text`, if it is a verified one.
pub(crate) fn recognise(text: &str) -> Option<Model> {
    let table: [(&str, Model); 13] = [
        (BASE_WRYNOSE, Model::BasePackageconfig(BaseText::Wrynose)),
        (
            BASE_SCARTHGAP,
            Model::BasePackageconfig(BaseText::Scarthgap),
        ),
        (
            BASE_KIRKSTONE,
            Model::BasePackageconfig(BaseText::Kirkstone),
        ),
        (CMAKE, Model::Cmake),
        (UA_WRYNOSE, Model::UpdateAlternatives(UaText::Wrynose)),
        (
            UA_SCARTHGAP,
            Model::UpdateAlternatives(UaText::ScarthgapKirkstone),
        ),
        (PACKAGEGROUP, Model::Packagegroup),
        (KERNEL_WRYNOSE, Model::Kernel),
        (KERNEL_SCARTHGAP, Model::Kernel),
        (KERNEL_KIRKSTONE, Model::Kernel),
        (KERNEL_DEVICETREE, Model::KernelDevicetree),
        (PYTHON3_WRYNOSE, Model::Python3Manifest(PyText::Wrynose)),
        (
            PYTHON3_SCARTHGAP,
            Model::Python3Manifest(PyText::ScarthgapKirkstone),
        ),
    ];
    table
        .iter()
        .find(|(t, _)| same_text(text, t))
        .map(|(_, m)| *m)
}

/// How an entry of the function's `python_vars` contributions was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source<'a> {
    /// `d.<write>('<literal>', ...)`: the literal as written.
    Literal(&'a str),
    /// A call of the body's own wrapper (`appendVar('DEPENDS', ...)` in base.bbclass).
    Wrapper(&'a str),
    /// A literal write of a `def` the function calls (`Replay::def_call_writes`): the name
    /// of the `def` it calls.
    DefCall(&'a str),
}

impl Model {
    /// A short name for notes.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Model::BasePackageconfig(_) => "base.bbclass PACKAGECONFIG handling",
            Model::Cmake => "cmake.bbclass OECMAKE_GENERATOR handling",
            Model::UpdateAlternatives(_) => "update-alternatives.bbclass ua_extend_depends",
            Model::Packagegroup => "packagegroup.bbclass complementary packages",
            Model::Kernel => "kernel.bbclass image types",
            Model::KernelDevicetree => "kernel-devicetree.bbclass PACKAGES",
            Model::Python3Manifest(_) => "python3 manifest (python3-manifest.json)",
        }
    }

    /// Whether the model reproduces the write that produced an entry.
    pub(crate) fn covers(self, src: Source) -> bool {
        match (self, src) {
            (Model::BasePackageconfig(_), Source::Wrapper(n)) => matches!(
                n,
                "DEPENDS" | "RDEPENDS:${PN}" | "RRECOMMENDS:${PN}" | "PACKAGECONFIG_CONFARGS"
            ),
            (Model::Cmake, Source::Literal(n)) => {
                matches!(n, "OECMAKE_GENERATOR_ARGS" | "DEPENDS" | "do_compile")
            }
            (Model::UpdateAlternatives(_), Source::DefCall("ua_extend_depends")) => true,
            (Model::Packagegroup, Source::Literal(n)) => matches!(n, "PACKAGES" | "ALLOW_EMPTY:%s"),
            (Model::Kernel, Source::Literal(n)) => matches!(
                n,
                "STAGING_KERNEL_DIR"
                    | "STAGING_KERNEL_BUILDDIR"
                    | "KERNEL_IMAGETYPES"
                    | "KERNEL_IMAGETYPE_FOR_MAKE"
                    | "PACKAGES"
            ),
            (Model::KernelDevicetree, Source::Literal("PACKAGES")) => true,
            (Model::Python3Manifest(_), Source::Literal(n)) => matches!(
                n,
                "RPROVIDES:class-native"
                    | "FILES:"
                    | "RDEPENDS:"
                    | "RRECOMMENDS:"
                    | "SUMMARY:"
                    | "PACKAGES"
            ),
            _ => false,
        }
    }

    /// The `def` functions ([`DEFS`]) the function calls, which must be the verified text.
    pub(crate) fn defs(self) -> &'static [&'static str] {
        match self {
            Model::UpdateAlternatives(UaText::Wrynose) => {
                &["update_alternatives_enabled", "ua_extend_depends"]
            }
            Model::UpdateAlternatives(UaText::ScarthgapKirkstone) => &[
                "update_alternatives_enabled",
                "ua_extend_depends",
                "gen_updatealternativesvardeps",
            ],
            _ => &[],
        }
    }
}

/// One write a model reproduces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Write {
    /// `d.appendVar(name, text)`.
    Append(String, String),
    /// `d.setVar(name, text)` (not parsing).
    Set(String, String),
    /// `d.setVarFlag(name, flag, text)`.
    SetFlag(String, String, String),
}

/// What happened to a modelled function on this recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Applied {
    /// Its writes were reproduced (possibly none).
    Done,
    /// Not reproduced: why.
    Refused(String),
}

/// What a model may read besides the datastore.
pub(crate) struct Ctx<'a> {
    /// The datastore as the function sees it (see the module documentation).
    pub(crate) ds: &'a DataStore,
    /// Reads a layer file (`None` when it cannot be read inside the workspace).
    pub(crate) read_file: &'a dyn Fn(&Path) -> Option<String>,
}

/// The writes of `model` on `ctx`, in the order the function makes them, or why they
/// cannot be told.
pub(crate) fn run(model: Model, ctx: &Ctx) -> Result<Vec<Write>, String> {
    for d in model.defs() {
        match ctx.ds.notes.model_defs.get(*d) {
            Some((_, true)) => {}
            Some((loc, false)) => {
                return Err(format!(
                    "{d} is defined at {loc} with a text other than the verified one"
                ));
            }
            None => return Err(format!("{d} is not defined in this parse")),
        }
    }
    match model {
        Model::BasePackageconfig(t) => base_packageconfig(ctx, t),
        Model::Cmake => cmake(ctx),
        Model::UpdateAlternatives(_) => update_alternatives(ctx),
        Model::Packagegroup => packagegroup(ctx),
        Model::Kernel => kernel(ctx),
        Model::KernelDevicetree => kernel_devicetree(ctx),
        Model::Python3Manifest(t) => python3_manifest(ctx, t),
    }
}

// ---------------------------------------------------------------------------------------
// Reading the datastore
// ---------------------------------------------------------------------------------------

/// `d.getVar(name)`: `Ok(None)` when unset (and nothing may set it).
fn get(ds: &DataStore, name: &str) -> Result<Option<String>, String> {
    if name.contains(['\'', '\\', '\n']) {
        return Err(format!("{name} is not a plain name"));
    }
    match ds.python_value(&format!("d.getVar('{name}')")) {
        Some(PyVal::None) => Ok(None),
        Some(PyVal::Str(s)) => Ok(Some(s)),
        _ => Err(format!("{name} is not certain")),
    }
}

/// `d.getVar(name)` that must be set.
fn get_set(ds: &DataStore, name: &str) -> Result<String, String> {
    get(ds, name)?.ok_or_else(|| format!("{name} is unset (python would raise)"))
}

/// `d.expand(text)`.
fn expand(ds: &DataStore, text: &str) -> Result<String, String> {
    let v = ds.expand(text);
    if v.unevaluated_python || !v.unresolved.is_empty() {
        return Err(format!("the expansion of `{text}` is not certain"));
    }
    Ok(v.text)
}

/// `bb.data.inherits_class(klass, d)`.
fn inherits(ds: &DataStore, klass: &str) -> Result<bool, String> {
    ds.inherits_class_certain(klass)
        .ok_or_else(|| format!("whether the recipe inherits {klass} is not certain"))
}

/// A python expression of the `pyexpr` subset that gives a bool.
fn py_bool(ds: &DataStore, code: &str) -> Result<bool, String> {
    match ds.python_value(code) {
        Some(PyVal::Bool(b)) => Ok(b),
        _ => Err(format!("`{code}` is not certain")),
    }
}

/// `str.split()`: refused when the text holds a character python counts as whitespace and
/// Rust does not (`\x1c`..`\x1f`).
fn split(s: &str) -> Result<Vec<String>, String> {
    if s.chars().any(|c| ('\x1c'..='\x1f').contains(&c)) {
        return Err("a value holds a separator python and Rust split differently".into());
    }
    Ok(s.split_whitespace().map(str::to_string).collect())
}

/// `str.strip()` (no argument), under the same condition as [`split`].
fn strip(s: &str) -> Result<String, String> {
    if s.chars().any(|c| ('\x1c'..='\x1f').contains(&c)) {
        return Err("a value holds a separator python and Rust strip differently".into());
    }
    Ok(s.trim().to_string())
}

/// Why the flags of `base` (as `d.getVarFlags` returns them) may differ from the stored
/// ones: something the replay does not reproduce may set one.
fn flags_doubt(ds: &DataStore, base: &str) -> Option<String> {
    let n = &ds.notes;
    if n.python_vars.contains(base) {
        return Some(format!("anonymous python before it may change {base}"));
    }
    if n.uncertain_vars.contains(base) || n.unresolved_vars.contains_key(base) {
        return Some(format!(
            "{base} is set by a statement whose effect is not certain"
        ));
    }
    if n.ref_effects
        .iter()
        .any(|e| e.vars.as_ref().is_none_or(|m| m.contains_key(base)))
    {
        return Some(format!(
            "a reference that could not be followed exactly may set {base}"
        ));
    }
    // A name `expandKeys` may still rename onto `base` brings no flags: `renameVar` moves
    // the value and the `:append` / `:prepend` / `:remove` lists only (`DataStore::
    // expand_keys`); the value itself is read through `get`, which flags such a rename.
    None
}

/// `os.path.join(a, b)` for a relative `b` without `/` at its start.
fn path_join(a: &str, b: &str) -> String {
    if a.is_empty() || a.ends_with('/') {
        format!("{a}{b}")
    } else {
        format!("{a}/{b}")
    }
}

// ---------------------------------------------------------------------------------------
// base.bbclass
// ---------------------------------------------------------------------------------------

/// The `PACKAGECONFIG` block of `base.bbclass`'s anonymous python (the lines from
/// `pkgconfigflags = d.getVarFlags("PACKAGECONFIG") or {}` to `appendVar('PACKAGECONFIG_CONFARGS',
/// extraconf)`). The code before it only reads (`SKIP_RECIPE`; on Scarthgap and Kirkstone the
/// features backfill, which the replay applies itself, and `PSEUDO_IGNORE_PATHS`, which the
/// block does not read).
fn base_packageconfig(ctx: &Ctx, t: BaseText) -> Result<Vec<Write>, String> {
    let ds = ctx.ds;
    if let Some(why) = flags_doubt(ds, "PACKAGECONFIG") {
        return Err(why);
    }
    // `d.getVarFlags("PACKAGECONFIG")`: the stored flags, `??=` defaults included for flags
    // without a value on BitBake 2.18; internal flags (`_...`, `:...`) left out. A `??=`
    // flag is refused, so the releases that handle it differently do not matter.
    let has_default_flags = ds
        .vars
        .get("PACKAGECONFIG")
        .is_some_and(|v| !v.default_flags.is_empty());
    if has_default_flags {
        return Err("a PACKAGECONFIG flag has a ??= default".into());
    }
    let mut flags: Vec<(String, String)> = Vec::new();
    for f in ds.flag_names("PACKAGECONFIG") {
        if f.starts_with('_') || f.starts_with(':') {
            continue;
        }
        if f == "export" || f == "unexport" {
            return Err(format!("PACKAGECONFIG[{f}] is not text in BitBake"));
        }
        let raw = ds
            .raw_flag("PACKAGECONFIG", &f)
            .ok_or_else(|| format!("PACKAGECONFIG[{f}] has no text"))?;
        flags.push((f, raw));
    }
    if flags.is_empty() {
        return Ok(Vec::new());
    }
    // `sorted(pkgconfigflags.items())`: by flag name (code point order, as Rust's).
    flags.sort();
    let pkgconfig = split(&get(ds, "PACKAGECONFIG")?.unwrap_or_default())?;
    let names: BTreeSet<&str> = flags.iter().map(|(f, _)| f.as_str()).collect();
    let mut extradeps: Vec<String> = Vec::new();
    let mut extrardeps: Vec<String> = Vec::new();
    let mut extrarrecs: Vec<String> = Vec::new();
    let mut extraconf: Vec<String> = Vec::new();
    for (flag, val) in &flags {
        let items: Vec<&str> = val.split(',').collect();
        let num = items.len();
        if num > 6 {
            return Err(format!(
                "PACKAGECONFIG[{flag}] has more than six fields (bb.error)"
            ));
        }
        let on = pkgconfig.iter().any(|p| p == flag);
        if on {
            if num >= 3 && !items[2].is_empty() {
                extradeps.push(items[2].to_string());
            }
            if num >= 4 && !items[3].is_empty() {
                extrardeps.push(items[3].to_string());
            }
            if num >= 5 && !items[4].is_empty() {
                extrarrecs.push(items[4].to_string());
            }
            if num >= 1 && !items[0].is_empty() {
                extraconf.push(items[0].to_string());
            }
        } else if num >= 2 && !items[1].is_empty() {
            extraconf.push(items[1].to_string());
        }
        if num >= 6 && !items[5].is_empty() {
            let conflicts = split(items[5])?;
            if conflicts.iter().any(|c| !names.contains(c.as_str())) {
                return Err(format!(
                    "PACKAGECONFIG[{flag}] names an invalid conflict (bb.error)"
                ));
            }
            if on && conflicts.iter().any(|c| pkgconfig.contains(c)) {
                return Err(format!(
                    "PACKAGECONFIG[{flag}] conflicts with PACKAGECONFIG (bb.fatal)"
                ));
            }
        }
    }
    let mut out = Vec::new();
    for (varname, appends) in [
        ("DEPENDS", extradeps),
        ("RDEPENDS:${PN}", extrardeps),
        ("RRECOMMENDS:${PN}", extrarrecs),
        ("PACKAGECONFIG_CONFARGS", extraconf),
    ] {
        if appends.is_empty() {
            continue;
        }
        let filtered = match t {
            BaseText::Wrynose => varname.contains("DEPENDS") || varname.starts_with("RRECOMMENDS"),
            BaseText::Scarthgap | BaseText::Kirkstone => varname.contains("DEPENDS"),
        };
        let appends = if filtered {
            if inherits(ds, "nativesdk")? || inherits(ds, "cross-canadian")? {
                expand_filter(ds, &appends, "", "nativesdk-")?
            } else if inherits(ds, "native")? || (t == BaseText::Wrynose && inherits(ds, "cross")?)
            {
                expand_filter(ds, &appends, "-native", "")?
            } else {
                match get(ds, "MLPREFIX")? {
                    Some(p) if !p.is_empty() => expand_filter(ds, &appends, "", &p)?,
                    _ => appends,
                }
            }
        } else {
            appends
        };
        let name = expand(ds, varname)?;
        out.push(Write::Append(name, format!(" {}", appends.join(" "))));
    }
    Ok(out)
}

/// `expandFilter(appends, extension, prefix)` of base.bbclass.
fn expand_filter(
    ds: &DataStore,
    appends: &[String],
    extension: &str,
    prefix: &str,
) -> Result<Vec<String>, String> {
    let text = expand(ds, &appends.join(" "))?;
    let mut out = Vec::new();
    for a in explode_deps(&split(&text)?) {
        if a.ends_with("-native") || a.contains("-cross-") {
            out.push(a);
        } else if let Some(subs) = a.strip_prefix("virtual/") {
            if subs.starts_with(prefix) {
                out.push(format!("{a}{extension}"));
            } else {
                out.push(format!("virtual/{prefix}{subs}{extension}"));
            }
        } else if a.starts_with(prefix) {
            out.push(format!("{a}{extension}"));
        } else {
            out.push(format!("{prefix}{a}{extension}"));
        }
    }
    Ok(out)
}

/// `bb.utils.explode_deps` (BitBake `lib/bb/utils.py`, the same in 2.0, 2.8 and 2.18): the
/// words, without the `(...)` version constraints.
fn explode_deps(words: &[String]) -> Vec<String> {
    let mut r = Vec::new();
    let mut flag = false;
    for i in words {
        if i.starts_with('(') {
            flag = true;
        }
        if !flag {
            r.push(i.clone());
        }
        if flag && i.ends_with(')') {
            flag = false;
        }
    }
    r
}

// ---------------------------------------------------------------------------------------
// cmake.bbclass
// ---------------------------------------------------------------------------------------

fn cmake(ctx: &Ctx) -> Result<Vec<Write>, String> {
    let ds = ctx.ds;
    let generator = get_set(ds, "OECMAKE_GENERATOR")?;
    if generator.contains("Unix Makefiles") {
        let make = get_set(ds, "MAKE")?;
        Ok(vec![
            Write::Set(
                "OECMAKE_GENERATOR_ARGS".into(),
                format!("-G '{generator}' -DCMAKE_MAKE_PROGRAM={make}"),
            ),
            Write::SetFlag("do_compile".into(), "progress".into(), "percent".into()),
        ])
    } else if generator.contains("Ninja") {
        Ok(vec![
            Write::Append("DEPENDS".into(), " ninja-native".into()),
            Write::Set(
                "OECMAKE_GENERATOR_ARGS".into(),
                format!("-G '{generator}' -DCMAKE_MAKE_PROGRAM=ninja"),
            ),
            Write::SetFlag(
                "do_compile".into(),
                "progress".into(),
                r"outof:^\[(\d+)/(\d+)\]\s+".into(),
            ),
        ])
    } else {
        Err(format!("unknown CMake generator {generator} (bb.fatal)"))
    }
}

// ---------------------------------------------------------------------------------------
// update-alternatives.bbclass
// ---------------------------------------------------------------------------------------

/// `if not update_alternatives_enabled(d): return`, then `ua_extend_depends(d)`. On
/// Scarthgap and Kirkstone `gen_updatealternativesvardeps(d)` comes between; it writes only
/// names ending in `_VARDEPS` or holding `_VARDEPS_`, which this does not read.
fn update_alternatives(ctx: &Ctx) -> Result<Vec<Write>, String> {
    let ds = ctx.ds;
    // update_alternatives_enabled: `or` stops at the first true operand.
    for c in ["native", "cross", "crosssdk", "cross-canadian"] {
        if inherits(ds, c)? {
            return Ok(Vec::new());
        }
    }
    if get(ds, "TARGET_OS")?.as_deref() == Some("mingw32") {
        return Ok(Vec::new());
    }
    // ua_extend_depends: a substring test on the text.
    let provides = get_set(ds, "PROVIDES")?;
    if provides.contains("virtual/update-alternatives") {
        return Ok(Vec::new());
    }
    Ok(vec![Write::Append(
        "DEPENDS".into(),
        " virtual/${MLPREFIX}update-alternatives".into(),
    )])
}

// ---------------------------------------------------------------------------------------
// packagegroup.bbclass
// ---------------------------------------------------------------------------------------

fn packagegroup(ctx: &Ctx) -> Result<Vec<Write>, String> {
    let ds = ctx.ds;
    let mut packages = split(&get_set(ds, "PACKAGES")?)?;
    let mut out = Vec::new();
    if get(ds, "PACKAGEGROUP_DISABLE_COMPLEMENTARY")?.as_deref() != Some("1") {
        let mut types = vec!["", "-dbg", "-dev"];
        if py_bool(
            ds,
            "bb.utils.contains('DISTRO_FEATURES', 'ptest', True, False, d)",
        )? {
            types.push("-ptest");
        }
        packages = packages
            .iter()
            .flat_map(|p| types.iter().map(move |s| format!("{p}{s}")))
            .collect();
        out.push(Write::Set("PACKAGES".into(), packages.join(" ")));
    }
    for p in &packages {
        out.push(Write::Set(format!("ALLOW_EMPTY:{p}"), "1".into()));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// kernel.bbclass, kernel-devicetree.bbclass
// ---------------------------------------------------------------------------------------

fn kernel(ctx: &Ctx) -> Result<Vec<Write>, String> {
    let ds = ctx.ds;
    let mut out = Vec::new();
    let kpn = get(ds, "KERNEL_PACKAGE_NAME")?;
    if kpn.as_deref() != Some("kernel") {
        let workdir = get_set(ds, "WORKDIR")?;
        out.push(Write::Set(
            "STAGING_KERNEL_DIR".into(),
            path_join(&workdir, "kernel-source"),
        ));
        out.push(Write::Set(
            "STAGING_KERNEL_BUILDDIR".into(),
            path_join(&workdir, "kernel-build-artifacts"),
        ));
    }
    let ty = get(ds, "KERNEL_IMAGETYPE")?.unwrap_or_default();
    let alttype = get(ds, "KERNEL_ALT_IMAGETYPE")?.unwrap_or_default();
    let mut types = get(ds, "KERNEL_IMAGETYPES")?.unwrap_or_default();
    if !split(&types)?.contains(&ty) {
        types = strip(&format!("{ty} {types}"))?;
    }
    if !split(&types)?.contains(&alttype) {
        types = strip(&format!("{alttype} {types}"))?;
    }
    out.push(Write::Set("KERNEL_IMAGETYPES".into(), types.clone()));
    // Wrynose: `if "fitImage" in types: bb.error(...)`; the other texts have no such line,
    // and a fitImage type is refused on all of them (simpler, and never in the samples).
    if types.contains("fitImage") {
        return Err("KERNEL_IMAGETYPES holds fitImage (bb.error on Wrynose)".into());
    }
    let words = split(&types)?;
    if get(ds, "KERNEL_IMAGETYPE_FOR_MAKE")?.is_none_or(|v| v.is_empty()) {
        let set: BTreeSet<String> = words
            .iter()
            .map(|t| {
                if t == "vmlinux.gz" {
                    "vmlinux".to_string()
                } else {
                    t.clone()
                }
            })
            .collect();
        out.push(Write::Set(
            "KERNEL_IMAGETYPE_FOR_MAKE".into(),
            set.into_iter().collect::<Vec<_>>().join(" "),
        ));
    }
    let kname = match kpn {
        Some(k) if !k.is_empty() => k,
        _ => "kernel".to_string(),
    };
    if !words.is_empty() && !inherits(ds, "nopackages")? {
        for t in &words {
            if !t.is_ascii() {
                return Err(format!(
                    "image type {t} is not ASCII (python's lower() differs)"
                ));
            }
            out.push(Write::Append(
                "PACKAGES".into(),
                format!(" {kname}-image-{}", t.to_ascii_lowercase()),
            ));
        }
    }
    Ok(out)
}

fn kernel_devicetree(ctx: &Ctx) -> Result<Vec<Write>, String> {
    let ds = ctx.ds;
    if inherits(ds, "nopackages")? {
        return Ok(Vec::new());
    }
    let mut out = vec![Write::Append(
        "PACKAGES".into(),
        " ${KERNEL_PACKAGE_NAME}-devicetree".into(),
    )];
    if get(ds, "KERNEL_DEVICETREE_BUNDLE")?.as_deref() == Some("1") {
        out.push(Write::Append(
            "PACKAGES".into(),
            " ${KERNEL_PACKAGE_NAME}-image-zimage-bundle".into(),
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// python3 manifest
// ---------------------------------------------------------------------------------------

/// A JSON value with object members in document order (`object_pairs_hook=OrderedDict`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Json {
    Str(String),
    List(Vec<Json>),
    Obj(Vec<(String, Json)>),
    Other,
}

impl<'de> serde::Deserialize<'de> for Json {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Json, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Json;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_str<E>(self, v: &str) -> Result<Json, E> {
                Ok(Json::Str(v.to_string()))
            }
            fn visit_string<E>(self, v: String) -> Result<Json, E> {
                Ok(Json::Str(v))
            }
            fn visit_bool<E>(self, _: bool) -> Result<Json, E> {
                Ok(Json::Other)
            }
            fn visit_i64<E>(self, _: i64) -> Result<Json, E> {
                Ok(Json::Other)
            }
            fn visit_u64<E>(self, _: u64) -> Result<Json, E> {
                Ok(Json::Other)
            }
            fn visit_f64<E>(self, _: f64) -> Result<Json, E> {
                Ok(Json::Other)
            }
            fn visit_unit<E>(self) -> Result<Json, E> {
                Ok(Json::Other)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut a: A) -> Result<Json, A::Error> {
                let mut v = Vec::new();
                while let Some(x) = a.next_element::<Json>()? {
                    v.push(x);
                }
                Ok(Json::List(v))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut a: A) -> Result<Json, A::Error> {
                let mut v = Vec::new();
                while let Some((k, x)) = a.next_entry::<String, Json>()? {
                    v.push((k, x));
                }
                Ok(Json::Obj(v))
            }
        }
        de.deserialize_any(V)
    }
}

impl Json {
    /// The members of an object, refused when a name repeats (python would keep the first
    /// position with the last value).
    fn members(&self) -> Result<&[(String, Json)], String> {
        let Json::Obj(m) = self else {
            return Err("a manifest entry is not an object".into());
        };
        let mut seen = BTreeSet::new();
        if m.iter().any(|(k, _)| !seen.insert(k.as_str())) {
            return Err("a manifest object repeats a name".into());
        }
        Ok(m)
    }

    fn get(&self, key: &str) -> Result<Option<&Json>, String> {
        Ok(self
            .members()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v))
    }

    /// A list of strings (python iterates any iterable; a string would give characters).
    fn strings(&self) -> Result<Vec<&str>, String> {
        let Json::List(l) = self else {
            return Err("a manifest field is not a list".into());
        };
        l.iter()
            .map(|x| match x {
                Json::Str(s) => Ok(s.as_str()),
                _ => Err("a manifest list holds something other than text".to_string()),
            })
            .collect()
    }
}

/// The manifest as the function reads it: the text after `# EOC` and one more character,
/// parsed as JSON.
pub(crate) fn parse_manifest(text: &str) -> Result<Json, String> {
    let at = text
        .find("# EOC")
        .ok_or("the manifest has no `# EOC` line (python would parse from offset 5)")?;
    let start = at + 6;
    // `str.find` counts characters and `seek` bytes: the same only for ASCII.
    if !text.is_char_boundary(start.min(text.len())) || !text[..start.min(text.len())].is_ascii() {
        return Err("the manifest header is not ASCII".into());
    }
    let json = text.get(start..).unwrap_or("");
    serde_json::from_str::<Json>(json).map_err(|e| format!("the manifest is not JSON: {e}"))
}

fn python3_manifest(ctx: &Ctx, t: PyText) -> Result<Vec<Write>, String> {
    let ds = ctx.ds;
    let thisdir = get_set(ds, "THISDIR")?;
    let filename = path_join(&path_join(&thisdir, "python3"), "python3-manifest.json");
    let text = (ctx.read_file)(&PathBuf::from(&filename))
        .ok_or_else(|| format!("{filename} cannot be read"))?;
    let manifest = parse_manifest(&text)?;
    let keys = manifest.members()?;
    let mut out = Vec::new();
    // RPROVIDES for the native case.
    let pn0 = "python3";
    let mut rprovides = split(&get(ds, "RPROVIDES")?.unwrap_or_default())?;
    rprovides.push(format!("{pn0}-misc-native"));
    for (key, _) in keys {
        let p = format!("{pn0}-{key}-native");
        if !rprovides.contains(&p) {
            rprovides.push(p);
        }
    }
    out.push(Write::Set(
        "RPROVIDES:class-native".into(),
        rprovides.join(" "),
    ));
    let include_pycs = get(ds, "INCLUDE_PYCS")?;
    let packages = split(&get_set(ds, "PACKAGES")?)?;
    let pn = get_set(ds, "PN")?;
    let mut newpackages = Vec::new();
    for (key, entry) in keys {
        let pypackage = format!("{pn}-{key}");
        if !packages.contains(&pypackage) {
            newpackages.push(pypackage.clone());
        }
        let field = |name: &str| -> Result<&Json, String> {
            entry
                .get(name)?
                .ok_or_else(|| format!("manifest entry {key} has no {name} (KeyError)"))
        };
        let files_var = format!("FILES:{pypackage}");
        if t == PyText::ScarthgapKirkstone {
            out.push(Write::Set(files_var.clone(), String::new()));
        }
        for v in field("files")?.strings()? {
            out.push(Write::Append(files_var.clone(), format!(" {v}")));
        }
        if include_pycs.as_deref() == Some("1") {
            for v in field("cached")?.strings()? {
                out.push(Write::Append(files_var.clone(), format!(" {v}")));
            }
        }
        for v in field("rdepends")?.strings()? {
            let v = strip_pn(v)?;
            out.push(Write::Append(
                format!("RDEPENDS:{pypackage}"),
                format!(" {pn}-{v}"),
            ));
        }
        if let Some(r) = entry.get("rrecommends")? {
            for v in r.strings()? {
                let v = strip_pn(v)?;
                out.push(Write::Append(
                    format!("RRECOMMENDS:{pypackage}"),
                    format!(" {pn}-{v}"),
                ));
            }
        }
        let Json::Str(summary) = field("summary")? else {
            return Err(format!(
                "manifest entry {key} has a summary that is not text"
            ));
        };
        out.push(Write::Set(format!("SUMMARY:{pypackage}"), summary.clone()));
    }
    let mut all = newpackages;
    all.extend(packages);
    out.push(Write::Set("PACKAGES".into(), all.join(" ")));
    // The `ALLOW_EMPTY:${PN}-...` writes are not reproduced (they stay flagged).
    let pkgconfig = get_set(ds, "PACKAGECONFIG")?;
    if split(&pkgconfig)?.iter().any(|w| w == "pgo")
        && !py_bool(
            ds,
            "bb.utils.contains('MACHINE_FEATURES', 'qemu-usermode', True, False, d)",
        )?
    {
        return Err("pgo without qemu-usermode (bb.fatal)".into());
    }
    Ok(out)
}

/// `if '${PN}' in value: value = value.split('-', 1)[1]`.
fn strip_pn(v: &str) -> Result<&str, String> {
    if !v.contains("${PN}") {
        return Ok(v);
    }
    v.split_once('-')
        .map(|(_, r)| r)
        .ok_or_else(|| format!("manifest dependency {v} has no '-' (IndexError)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_verified_text_is_recognised_once() {
        for t in [
            BASE_WRYNOSE,
            BASE_SCARTHGAP,
            BASE_KIRKSTONE,
            CMAKE,
            UA_WRYNOSE,
            UA_SCARTHGAP,
            PACKAGEGROUP,
            KERNEL_WRYNOSE,
            KERNEL_SCARTHGAP,
            KERNEL_KIRKSTONE,
            KERNEL_DEVICETREE,
            PYTHON3_WRYNOSE,
            PYTHON3_SCARTHGAP,
        ] {
            assert!(recognise(t).is_some());
            // One changed byte is not.
            let changed = t.replacen('\n', "\n#\n", 1);
            assert_eq!(recognise(&changed), None);
        }
        assert_eq!(recognise("python () {\n}"), None);
    }

    #[test]
    fn explode_deps_drops_versions() {
        let w: Vec<String> = "a (>= 1.0) b (= 2) c"
            .split_whitespace()
            .map(str::to_string)
            .collect();
        assert_eq!(explode_deps(&w), vec!["a", "b", "c"]);
    }

    #[test]
    fn manifest_header_and_order() {
        let m = parse_manifest("# x\n# EOC\n{\"b\": {\"files\": []}, \"a\": {}}").unwrap();
        let keys: Vec<&str> = m
            .members()
            .unwrap()
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert_eq!(keys, vec!["b", "a"]);
        assert!(parse_manifest("{}").is_err());
        assert!(parse_manifest("# \u{e9}\n# EOC\n{}").is_err());
        let dup = parse_manifest("# EOC\n{\"a\": 1, \"a\": 2}").unwrap();
        assert!(dup.members().is_err());
    }

    #[test]
    fn path_join_like_python() {
        assert_eq!(path_join("/a", "b"), "/a/b");
        assert_eq!(path_join("/a/", "b"), "/a/b");
        assert_eq!(path_join("", "b"), "b");
    }
}

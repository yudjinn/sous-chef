//! Replays configuration and recipe files in BitBake's parse order (T3.4).
//!
//! Statements come from the index (`statements` rows, `value` column), never from a reparse.
//! The order and the side effects follow BitBake `master` (2026-11): `cookerdata.py`
//! (`parseConfigurationFiles`, `_parse_recipe`), `parse/parse_py/ConfHandler.py` (`handle`,
//! `include_single_file`), `parse/parse_py/BBHandler.py` (`handle`, `inherit`) and
//! `parse/ast.py` (`MethodNode`, `ExportFuncsNode`, `multi_finalize`, `finalize`); see
//! `docs/bitbake-semantics.md`, "Parse order" and "Replay".
//!
//! **Configuration pass** (cached, see [`ConfigCache`]): `TOPDIR` is the build directory;
//! `conf/bblayers.conf`; every layer's `conf/layer.conf` in `BBLAYERS` order with `LAYERDIR`
//! and `LAYERDIR_RE` set, each `${LAYERDIR}` / `${LAYERDIR_RE}` reference replaced in place
//! afterwards (`expandVarref`) and both deleted after the last layer; `conf/bitbake.conf`
//! found on `BBPATH`, following its includes and its `addfragments` directive (OE
//! configuration fragments, [`Feature::AddFragments`]: built-in fragments set `MACHINE` /
//! `DISTRO`, named ones are included; see `Replay::add_fragments`) as written; then `base`
//! and every `INHERIT`
//! class, in that order, as global classes. `MACHINE` / `DISTRO` from the [`Target`] are set
//! first and set again after every configuration assignment to them, so they win over the
//! configured values (as an environment setting would win over `??=`, and more).
//!
//! **Recipe pass**, on a fork of the configuration: `FILE_LAYERNAME`; the `.bb`, statement by
//! statement, following `require` / `include` / `include_all` / `inherit` where they appear
//! (targets resolved with the store as it is at that point, along the current `BBPATH`; a
//! class is parsed once, `__inherit_cache`); each bbappend in application order; deferred
//! inherits; `expand_keys`. Anonymous python is stored, never run. On a release without
//! `Feature::InheritDefer` an `inherit_defer` line is not a directive (BitBake fails with
//! "unparsed line"): it is recorded in [`ReplayNotes::references`] as
//! [`RefProblem::NotInRelease`] and nothing is inherited.
//!
//! **Class-extended variants** (`BBCLASSEXTEND`, see [`super::variant`]): after the
//! bbappends the store is forked and the base finalised on the fork (`multi_finalize`), which
//! gives `BBCLASSEXTEND` and `PN`; the variant then starts again from the unfinalised store,
//! gets `PN` / `BBEXTENDCURR` / `BBEXTENDVARIANT` ([`OpKind::ClassExtend`]) and its class
//! (deferred from BitBake 2.16, [`Feature::ClassExtendDeferred`]), and is finalised. In the
//! finalisation (`Replay::finalize`) the OE-core handlers BitBake runs at parse time for
//! class extension are emulated when the class defines them: `base_eventhandler` on
//! `RecipePreDeferredInherits` ([`Feature::DeferredInheritsClassOverride`]),
//! `native_virtclass_handler`, `nativesdk_virtclass_handler`, `multilib_virtclass_handler`
//! (and `_postkeyexp`): plain code on known values is reproduced ([`OpKind::EventHandler`]),
//! filters and renames flag the variables with a note.
//!
//! **`FILE` and the values computed from it.** BitBake sets `FILE` to every `.conf`, `.bb`,
//! `.inc` and `.bbappend` while it is parsed and restores it afterwards (not for classes).
//! OE-Core computes `PN`, `PV` (`bb.parse.vars_from_file(FILE)`), `BPN`
//! (`oe.utils.prune_suffix`), `FILE_DIRNAME` and `THISDIR` (`dirname(FILE)`) with python. In
//! the recipe pass the replay computes them the same way whenever the stored value is python
//! or absent, and again every time `FILE` changes, so `${PN}` inside a bbappend is the
//! append's name part and `${THISDIR}` with `:=` is the directory of the file being parsed.
//! `BP` is set to `${BPN}-${PV}` when absent. A plain assignment to one of them in the
//! metadata takes over.
//!
//! **Functions** are stored like variables, as BitBake does (`MethodNode.eval`): the name as
//! written (overrides included), flags `func`, `python`, `fakeroot`. The stored text is not
//! the body but a pointer, one line per piece (`layer:path:A-B`, or
//! `layer:path:A-B -> class_do_x` for an `EXPORT_FUNCTIONS` stub), so `value("do_install")`
//! lists the composed pieces in order; [`DataStore::function`] parses them. Anonymous
//! python is stored under BitBake's name `__anon_<line>_<file>` and listed in
//! [`ReplayNotes::anonymous`]; the variables its body changes by name (`d.setVar("X", ..)`
//! and the like) are recorded in [`ReplayNotes::python_vars`] and their values are flagged
//! `unevaluated_python`.
//!
//! Nothing here fails on bad metadata: a reference whose expanded text needs python or names
//! an undefined variable, is not found or closes a cycle is recorded in
//! [`ReplayNotes::references`] and skipped. A reference whose text expands completely but
//! whose value is uncertain (override decisions that depend on unknown `OVERRIDES` entries:
//! `inherit_defer toolchain/${TOOLCHAIN}` in OE-core) is followed, recorded in
//! [`ReplayNotes::uncertain_references`], and the variables its files change are flagged
//! ([`ReplayNotes::uncertain_vars`]).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use regex::Regex;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::datastore::{Content, OpKind, Var, base_of, full_name, split_keyword};
use super::expand::has_python;
use super::license;
use super::skips;
use super::variant::ClassExtend;
use super::{DataStore, FileVisit, OpStatus, Overlay, Target, VarOp};
use crate::error::{Error, Result};
use crate::index::query::{self, FileRow, StatementFilter, StatementRow};
use crate::index::{self, Index};
use crate::parse::words as pwords;
use crate::release::Feature;
use crate::types::{AssignOp, FileKind, LineSpan, Loc, VarRef};
use crate::workspace::Workspace;
use crate::workspace::resolve::{self, Resolver};

// ---------------------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------------------

/// What the replay recorded beside the variables. Read it with [`DataStore::replay`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReplayNotes {
    /// The recipe file (line 1); `None` for a configuration-only evaluation.
    pub recipe: Option<Loc>,
    /// The bbappends applied, in application order (line 1 of each).
    pub appends: Vec<Loc>,
    /// `FILE_LAYERNAME`: the collection whose `BBFILE_PATTERN` matches the recipe.
    pub layer_name: Option<String>,
    /// Notes about the evaluation as a whole that every tool showing evaluated values
    /// prints (`note: ...` lines): a recipe file BitBake does not collect, where the
    /// machine came from when not from the configuration, no `MACHINE` at all.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub target_notes: Vec<String>,
    /// Notes about one variable's value, by base name (`note: ...` lines): the
    /// `BB_RECIPE_VIRTUAL_PROVIDERS` rewrite of `DEPENDS`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub var_notes: BTreeMap<String, Vec<String>>,
    /// Anonymous python functions, in parse order (configuration classes included: BitBake
    /// runs those for every recipe too).
    pub anonymous: Vec<AnonPython>,
    /// Base names of variables that anonymous python changes by a literal name; their
    /// values carry `unevaluated_python`.
    pub python_vars: BTreeSet<String>,
    /// Base names of variables a statement the replay could not apply would have set
    /// (`MACHINE` and `DISTRO` when the `addfragments` list cannot be expanded), with the
    /// names that stood in the way; their values carry those names in `unresolved`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub unresolved_vars: BTreeMap<String, Vec<String>>,
    /// References (`include`, `require`, `include_all`, `inherit`, `inherit_defer`,
    /// `INHERIT`) that were not followed, and why.
    pub references: Vec<RefIssue>,
    /// References that were followed although the value of their argument is not certain:
    /// the expansion is complete text, but its value depends on override entries that are
    /// not all known or on variables python may change (`inherit_defer
    /// toolchain/${TOOLCHAIN}` while `OVERRIDES` holds python). See
    /// [`ReplayNotes::uncertain_vars`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uncertain_references: Vec<UncertainRef>,
    /// Base names of variables that a file reached through an [`UncertainRef`] changes;
    /// their values carry `unevaluated_python` when the reference's value did.
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub uncertain_vars: BTreeSet<String>,
    /// Names with `${...}` that `expand_keys` could not expand (python or undefined
    /// variables); they stay under their written name.
    pub incomplete_keys: Vec<String>,
    /// Paths probed for an `include` / `require` / `include_all` that held no file (absolute,
    /// normalised): a file created there would have been read. See [`super::Overlay`].
    #[serde(skip)]
    pub(crate) probed_missing: BTreeSet<PathBuf>,
    /// `__BBTASKS`: the tasks `addtask` added and `deltask` did not remove, in order (see
    /// [`DataStore::tasks`]).
    #[serde(skip)]
    pub(crate) tasks: Vec<String>,
    /// The `BBCLASSEXTEND` word of a class-extended variant (see [`super::variant`]);
    /// `None` for the base recipe and the configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<ClassExtend>,
    /// `__inherit_cache`: class files already parsed (absolute paths).
    #[serde(skip)]
    pub(crate) inherit_cache: Vec<PathBuf>,
    /// `__BBDEFINHERITS`: inherits waiting for the end of the recipe.
    #[serde(skip)]
    pub(crate) deferred: Vec<Deferred>,
    /// Names whose current value the replay computed (see the module documentation).
    #[serde(skip)]
    pub(crate) computed: BTreeSet<String>,
    /// Names a function statement wrote, as stored: the function's own name and, for a
    /// `NAME:append () {` / `NAME:prepend () {` piece, the name it is added to. Renamed with
    /// the variable by `expand_keys`. See [`DataStore::is_function`].
    #[serde(skip)]
    pub(crate) functions: BTreeSet<String>,
}

/// One anonymous python function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnonPython {
    /// The whole function.
    pub loc: Loc,
    pub via: Vec<Loc>,
    /// BitBake's name for it (`__anon_<line>_<file>`).
    pub name: String,
    /// Variables it changes by a literal name (`d.setVar("X", ...)`, `d.appendVar`, ...),
    /// expanded, as written.
    pub sets: Vec<String>,
    /// It also changes variables through a computed name, or passes `d` to other code, so
    /// its effect is not known even by name.
    pub opaque: bool,
}

/// A reference statement the replay did not follow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefIssue {
    /// The statement; `None` for a class from `INHERIT` or `base` that has no statement of
    /// its own, and for `conf/bitbake.conf`.
    pub loc: Option<Loc>,
    pub via: Vec<Loc>,
    /// `include`, `require`, `include_all`, `inherit`, `inherit_defer`, `INHERIT`, `conf`.
    pub kind: String,
    /// The argument as written.
    pub text: String,
    pub problem: RefProblem,
}

/// A reference statement the replay followed with a value that may be wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UncertainRef {
    /// The statement (`None` as for [`RefIssue::loc`]).
    pub loc: Option<Loc>,
    pub via: Vec<Loc>,
    /// `include`, `require`, `include_all`, `inherit`, `inherit_defer`.
    pub kind: String,
    /// The argument as written.
    pub text: String,
    /// The argument as the replay expanded it (what it followed).
    pub expanded: String,
    /// The expansion carried `unevaluated_python` (python in `OVERRIDES`, or a variable
    /// python may change).
    pub python: bool,
    /// Unresolved names the expansion carried that are still unset after the file was
    /// read (a name the file itself sets, `TCOVERRIDE` for `toolchain/gcc`, is dropped).
    pub unresolved: Vec<String>,
}

/// Why a reference was not followed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefProblem {
    /// The argument contains `${@...}`.
    Python,
    /// The argument references variables that are not set.
    Undefined(Vec<String>),
    /// No file for this word (`require`, `inherit`: an error in BitBake; `include`: silent
    /// in BitBake, not recorded).
    NotFound(String),
    /// The file is already being parsed further up (BitBake would recurse until it fails).
    Cycle(Loc),
    /// The directive does not exist on the workspace release (`inherit_defer` before
    /// Scarthgap): BitBake fails to parse the file there ("unparsed line"). The replay skips
    /// the statement and goes on.
    NotInRelease(Feature),
    /// BitBake stops with this error at the statement (an `addfragments` line whose form
    /// the release does not accept, a built-in fragment for a variable that already has a
    /// value, a fragment name without `/`). The replay skips what failed and goes on.
    Fatal(String),
}

/// One piece of a function's value (see [`DataStore::function`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FunctionPiece {
    /// The function statement, or the `EXPORT_FUNCTIONS` statement for a stub.
    pub loc: Loc,
    /// For an `EXPORT_FUNCTIONS` stub: the function it calls (`autotoolsish_do_install`).
    pub calls: Option<String>,
}

/// An uncertain reference being followed (see `Replay::under_taint`).
#[derive(Debug, Clone)]
struct Taint {
    loc: Option<Loc>,
    via: Vec<Loc>,
    kind: String,
    text: String,
    expanded: String,
    python: bool,
    unresolved: Vec<String>,
    /// Base names the files read under it changed.
    bases: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Deferred {
    text: String,
    loc: Option<Loc>,
    via: Vec<Loc>,
}

impl DataStore {
    /// What the replay recorded beside the variables.
    pub fn replay(&self) -> &ReplayNotes {
        &self.notes
    }

    /// Whether the value of `name` is a function body (its pieces are pointers, read them
    /// with [`DataStore::function`]): the `func` flag is set, or a function statement wrote
    /// the name.
    ///
    /// This is wider than BitBake's `func` flag, which `raw_flag(name, "func")` reports.
    /// BitBake loses the flag in two cases (BitBake 2.18, `lib/bb/data_smart.py`):
    /// `renameVar`, which `expandKeys` runs for a name containing `${...}`, copies only the
    /// `:append` / `:prepend` / `:remove` lists to the new name, so `pkg_postrm:${PN} () {`
    /// ends up as `pkg_postrm:dropbear` without the flag; and `NAME:prepend () {` stores the
    /// body in `NAME`'s prepend list and the flag on the name with `:prepend`, so a `NAME`
    /// built only from such pieces has no flag either. `bitbake -e` then prints the value
    /// as a variable; its text is still the shell body.
    pub fn is_function(&self, name: &str) -> bool {
        if self.raw_flag(name, "func").is_some() || self.notes.functions.contains(name) {
            return true;
        }
        split_keyword(name).is_some_and(|(base, _, _)| self.notes.functions.contains(&base))
    }

    /// `__BBTASKS`: the tasks `addtask` statements added (`do_` prefixed, the names not
    /// expanded, as `AddTaskNode.eval` does) and `deltask` statements (names expanded) did
    /// not remove, in the order first added (BitBake 2.18.0 `lib/bb/build.py` `addtask`,
    /// `deltask`). Tasks added by anonymous python (`bb.build.addtask`) are not seen.
    pub fn tasks(&self) -> &[String] {
        &self.notes.tasks
    }

    /// The composed pieces of a function, in order (override variant, `:append`,
    /// `:prepend` applied as for any variable). `None` if `name` has no value; lines that
    /// are not pointers are skipped.
    pub fn function(&self, name: &str) -> Option<Vec<FunctionPiece>> {
        let v = self.value(name)?;
        Some(v.text.lines().filter_map(parse_piece).collect())
    }
}

fn parse_piece(line: &str) -> Option<FunctionPiece> {
    let line = line.trim();
    let (loc, calls) = match line.split_once(" -> ") {
        Some((l, c)) => (l, Some(c.trim().to_string())),
        None => (line, None),
    };
    Some(FunctionPiece {
        loc: loc.parse().ok()?,
        calls,
    })
}

// ---------------------------------------------------------------------------------------
// Cache and entry points
// ---------------------------------------------------------------------------------------

/// Configuration passes, cached per index file, workspace, machine and distro. An entry is
/// reused while every `.conf` file and every file the pass visited keep their sha256 and
/// the set of `.bbclass` files is unchanged; otherwise the pass runs again.
#[derive(Debug, Default)]
pub struct ConfigCache {
    entries: Mutex<HashMap<CacheKey, CacheEntry>>,
    passes: AtomicU64,
}

type CacheKey = (PathBuf, String, Option<String>, Option<String>);

#[derive(Debug)]
struct CacheEntry {
    fingerprint: String,
    ds: DataStore,
}

impl ConfigCache {
    pub fn new() -> ConfigCache {
        ConfigCache::default()
    }

    /// How many configuration passes this cache has run.
    pub fn passes(&self) -> u64 {
        self.passes.load(Ordering::Relaxed)
    }

    /// Drops every entry.
    pub fn clear(&self) {
        self.lock().clear();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<CacheKey, CacheEntry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The process-wide cache [`evaluate`] uses.
pub fn global_cache() -> &'static ConfigCache {
    static CACHE: OnceLock<ConfigCache> = OnceLock::new();
    CACHE.get_or_init(ConfigCache::new)
}

/// [`super::evaluate`] with an explicit cache. The index must be refreshed by the caller.
pub fn evaluate_with(
    cache: &ConfigCache,
    index: &Index,
    ws: &Workspace,
    target: &Target,
) -> Result<DataStore> {
    evaluate_inner(cache, index, ws, target, None)
}

/// [`evaluate_with`] with some files replaced by in-memory contents ([`Overlay`]): wherever
/// the replay reads a file's statements, an overlay path is read from the overlay instead,
/// and an overlay path that is not indexed counts as an existing file. Nothing is written;
/// the index is only read; `cache` is used for the configuration pass only when no overlay
/// path could have changed it (see [`Overlay`]), and is never filled from an overlay
/// evaluation.
///
/// A recipe target must still name an indexed recipe (its contents may be replaced). For a
/// recipe *name*, the version is chosen without the overlay (`providers.rs`), then the chosen
/// file is evaluated with it. bbappends are the indexed ones of the recipe.
pub fn evaluate_overlay_with(
    cache: &ConfigCache,
    index: &Index,
    ws: &Workspace,
    target: &Target,
    overlay: &Overlay,
) -> Result<DataStore> {
    evaluate_inner(cache, index, ws, target, Some(overlay))
}

fn evaluate_inner(
    cache: &ConfigCache,
    index: &Index,
    ws: &Workspace,
    target: &Target,
    overlay: Option<&Overlay>,
) -> Result<DataStore> {
    let overlay = overlay.filter(|o| !o.is_empty());
    let files = Files::load(index, ws)?;
    let config = |cache: &ConfigCache| -> Result<DataStore> {
        let mut c = configuration(cache, index, ws, &files, target, overlay)?;
        if target.machine.is_some() || target.distro.is_some() {
            bbmask_note(index, ws, &mut c)?;
        }
        Ok(c)
    };
    let Some(name) = &target.recipe else {
        return config(cache);
    };
    // A variant pointer: `virtual:<word>:<recipe file>` (bb.cache.realfn2virtual).
    if let Some((ext, file)) = super::variant::split_virtual(name) {
        let r = find_recipe_file(index, ws, file)?.ok_or_else(|| recipe_not_found(name, None))?;
        let config = config(cache)?;
        return recipe_pass(index, ws, &files, &config, &r, overlay, Some(&ext));
    }
    // A path is resolved before the configuration pass; a recipe name needs it, because
    // the choice of file reads PREFERRED_VERSION (providers.rs).
    if let Some(r) = find_recipe_file(index, ws, name)? {
        let config = config(cache)?;
        return recipe_pass(index, ws, &files, &config, &r, overlay, None);
    }
    let config = config(cache)?;
    let version = target.version.as_deref();
    let chosen = super::providers::Providers::with_config(cache, index, ws, target, config.fork())?
        .best_version(name, version)?
        .ok_or_else(|| recipe_not_found(name, version))?;
    recipe_pass(
        index,
        ws,
        &files,
        &config,
        &chosen.recipe.file,
        overlay,
        chosen.variant.as_ref(),
    )
}

/// Most files listed by name in the `BBMASK` note.
const MAX_MASK_LISTED: usize = 10;

/// For an evaluation with an explicit machine or distro: when the `BBMASK` of this
/// configuration masks a different set of files than the one the index was marked with
/// (the default machine and distro, `files.not_collected = 'bbmask'`), a target note naming
/// the files it would additionally mask and those it would unmask. The candidates are the
/// files the index collects plus those it marked as masked (the `BBFILES` collection before
/// masking); `re.search` on their workspace paths, as the index does. A mask sous-chef
/// cannot apply already has its own note (`config_pass`).
fn bbmask_note(index: &Index, ws: &Workspace, config: &mut DataStore) -> Result<()> {
    let mask = match super::bbmask(config) {
        super::BbMask::Static(re) => Some(re),
        super::BbMask::None => None,
        super::BbMask::Unusable(_) => return Ok(()),
    };
    let masked_now: Vec<FileRow> = query::uncollected_recipe_files(index)?
        .into_iter()
        .filter(|(_, why)| why == crate::index::schema::NOT_COLLECTED_BBMASK)
        .map(|(f, _)| f)
        .collect();
    if mask.is_none() && masked_now.is_empty() {
        return Ok(());
    }
    let hit = |f: &FileRow| -> bool {
        let Some(re) = &mask else { return false };
        ws.abs_path(&f.loc(LineSpan::line(1)))
            .is_some_and(|p| re.is_match(&p.to_string_lossy()))
    };
    let label = |f: &FileRow| format!("{}:{}", f.layer, f.path.display());
    let more: Vec<String> = match &mask {
        None => Vec::new(),
        Some(_) => query::files_by_kind_in(
            index,
            &[FileKind::Recipe, FileKind::Append],
            query::FileSet::COLLECTED,
        )?
        .iter()
        .filter(|f| hit(f))
        .map(label)
        .collect(),
    };
    let fewer: Vec<String> = masked_now.iter().filter(|f| !hit(f)).map(label).collect();
    if more.is_empty() && fewer.is_empty() {
        return Ok(());
    }
    let list = |v: &[String]| -> String {
        let mut s = v
            .iter()
            .take(MAX_MASK_LISTED)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if v.len() > MAX_MASK_LISTED {
            s.push_str(&format!(" (+{} more)", v.len() - MAX_MASK_LISTED));
        }
        s
    };
    let mut parts = Vec::new();
    if !more.is_empty() {
        parts.push(format!(
            "it would also mask {} file{}: {}",
            more.len(),
            if more.len() == 1 { "" } else { "s" },
            list(&more)
        ));
    }
    if !fewer.is_empty() {
        parts.push(format!(
            "it would not mask {} file{} the index treats as masked: {}",
            fewer.len(),
            if fewer.len() == 1 { "" } else { "s" },
            list(&fewer)
        ));
    }
    Arc::make_mut(&mut config.notes).target_notes.push(format!(
        "note: BBMASK for this machine and distro differs from the one the index applied (default machine and distro): {}; recipe names, providers and bbappends here still follow the index's mask",
        parts.join("; ")
    ));
    Ok(())
}

/// The configuration pass: the cached one, unless an overlay path could have changed it
/// (a `.conf` or `.bbclass`, a file it visited, a path it probed and found empty), in which
/// case it is replayed with the overlay and not cached.
fn configuration(
    cache: &ConfigCache,
    index: &Index,
    ws: &Workspace,
    files: &Files,
    target: &Target,
    overlay: Option<&Overlay>,
) -> Result<DataStore> {
    let config = cached_configuration(cache, index, ws, files, target)?;
    let Some(o) = overlay else {
        return Ok(config);
    };
    let touched = o.paths().any(|p| {
        matches!(
            FileKind::from_path(p),
            Some(FileKind::Conf) | Some(FileKind::Class)
        ) || config.notes.probed_missing.contains(p)
            || config
                .files()
                .iter()
                .any(|v| ws.abs_path(&v.loc).is_some_and(|a| normalize(&a) == p))
    });
    if !touched {
        return Ok(config);
    }
    config_pass(index, ws, files, target, overlay)
}

fn cached_configuration(
    cache: &ConfigCache,
    index: &Index,
    ws: &Workspace,
    files: &Files,
    target: &Target,
) -> Result<DataStore> {
    let key: CacheKey = (
        index.path().to_path_buf(),
        index::workspace_key(ws),
        target.machine.clone(),
        target.distro.clone(),
    );
    if let Some(e) = cache.lock().get(&key)
        && e.fingerprint == files.fingerprint(e.ds.files())
    {
        return Ok(e.ds.fork());
    }
    let ds = config_pass(index, ws, files, target, None)?;
    cache.passes.fetch_add(1, Ordering::Relaxed);
    let fingerprint = files.fingerprint(ds.files());
    cache.lock().insert(
        key,
        CacheEntry {
            fingerprint,
            ds: ds.fork(),
        },
    );
    Ok(ds)
}

/// `NotFound` for a recipe target.
fn recipe_not_found(name: &str, version: Option<&str>) -> Error {
    Error::NotFound {
        what: match version {
            Some(v) => format!("recipe {name} version {v}; list recipes with the recipe tool"),
            None => format!("recipe {name}; list recipes with the recipe tool"),
        },
    }
}

/// The recipe file a path target names (an absolute path to a `.bb`, or `layer:path`);
/// `Ok(None)` when `name` is not a path but a recipe name, which
/// [`super::providers::Providers::best_version`] resolves (`PREFERRED_VERSION`,
/// `DEFAULT_PREFERENCE`, layer priority and version).
fn find_recipe_file(index: &Index, ws: &Workspace, name: &str) -> Result<Option<FileRow>> {
    if !name.ends_with(".bb") {
        return Ok(None);
    }
    let p = Path::new(name);
    let row = if p.is_absolute() {
        query::file_by_abs(index, ws, p)?
    } else if let Some((layer, path)) = name.split_once(':') {
        query::file_by_path(index, layer, Path::new(path))?
    } else {
        None
    };
    row.filter(|f| f.kind == FileKind::Recipe)
        .map(Some)
        .ok_or_else(|| recipe_not_found(name, None))
}

// ---------------------------------------------------------------------------------------
// Indexed files
// ---------------------------------------------------------------------------------------

/// `(layer, path, kind, statements)` of a file the replay reads.
type FileStatements = (String, PathBuf, FileKind, Vec<StatementRow>);

#[derive(Debug, Clone)]
struct FileInfo {
    id: i64,
    layer: String,
    path: PathBuf,
    kind: FileKind,
    sha256: String,
}

/// Every indexed file by absolute path (layer root joined with the indexed path).
struct Files {
    by_abs: HashMap<PathBuf, FileInfo>,
    by_loc: HashMap<(String, PathBuf), FileInfo>,
    known: HashSet<PathBuf>,
    /// The part of the cache fingerprint that does not depend on the visited files.
    base: String,
}

impl Files {
    fn load(index: &Index, ws: &Workspace) -> Result<Files> {
        // By layer name then path, compared as bytes (SQLite's text order): the order the
        // cache fingerprint is computed in.
        let mut rows: Vec<FileInfo> = query::all_files_in(index, query::FileSet::ALL)?
            .into_iter()
            .map(|f| FileInfo {
                id: f.id,
                layer: f.layer,
                path: f.path,
                kind: f.kind,
                sha256: f.sha256,
            })
            .collect();
        rows.sort_by(|a, b| {
            a.layer.cmp(&b.layer).then_with(|| {
                a.path
                    .as_os_str()
                    .as_encoded_bytes()
                    .cmp(b.path.as_os_str().as_encoded_bytes())
            })
        });
        let mut by_abs = HashMap::new();
        let mut by_loc = HashMap::new();
        let mut h = Sha256::new();
        for f in rows {
            match f.kind {
                FileKind::Conf => {
                    h.update(format!("c {} {} {}\n", f.layer, f.path.display(), f.sha256))
                }
                FileKind::Class => h.update(format!("k {} {}\n", f.layer, f.path.display())),
                _ => {}
            }
            let loc = Loc {
                layer: f.layer.clone(),
                path: f.path.clone(),
                lines: LineSpan::line(1),
            };
            if let Some(abs) = ws.abs_path(&loc) {
                by_abs.insert(normalize(&abs), f.clone());
            }
            by_loc.insert((f.layer.clone(), f.path.clone()), f);
        }
        let known = by_abs.keys().cloned().collect();
        Ok(Files {
            by_abs,
            by_loc,
            known,
            base: hex(&h.finalize()),
        })
    }

    /// Cache fingerprint of a configuration pass that visited `visits`.
    fn fingerprint(&self, visits: &[FileVisit]) -> String {
        let mut h = Sha256::new();
        h.update(self.base.as_bytes());
        for v in visits {
            let sha = self
                .by_loc
                .get(&(v.loc.layer.clone(), v.loc.path.clone()))
                .map(|f| f.sha256.as_str())
                .unwrap_or("-");
            h.update(format!(
                "v {} {} {sha}\n",
                v.loc.layer,
                v.loc.path.display()
            ));
        }
        hex(&h.finalize())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Lexical normalisation (`.` and `..` removed).
pub(crate) fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// The two passes
// ---------------------------------------------------------------------------------------

fn config_pass(
    index: &Index,
    ws: &Workspace,
    files: &Files,
    target: &Target,
    overlay: Option<&Overlay>,
) -> Result<DataStore> {
    let mut pins = Vec::new();
    if let Some(m) = &target.machine {
        pins.push(("MACHINE", m.clone()));
    }
    if let Some(d) = &target.distro {
        pins.push(("DISTRO", d.clone()));
    }
    let mut r = Replay::new(index, ws, files, DataStore::new(), true, false, pins);
    r.overlay = overlay;
    for (name, value) in r.pins.clone() {
        put(&mut r.ds, name, &value);
    }

    // bblayers.conf; TOPDIR is its build directory. BBPATH starts as the build directory:
    // OE-core's build environment exports `BBPATH=$BUILDDIR` (scripts/oe-buildenv-internal)
    // and BitBake takes it from the environment (`data.inheritFromOS`, "from env" in the
    // history), which is how `conf/local.conf` and `conf/toolcfg.conf` are found when
    // `bblayers.conf` does not set BBPATH (a bitbake-setup build).
    if let Some(build) = &ws.build_dir {
        put(&mut r.ds, "TOPDIR", &build.to_string_lossy());
        if !r.pins.iter().any(|(n, _)| *n == "BBPATH") {
            put(&mut r.ds, "BBPATH", &build.to_string_lossy());
        }
        let bblayers = normalize(&build.join("conf/bblayers.conf"));
        if r.exists(&bblayers) {
            r.visit(&bblayers, FileKind::Conf, Vec::new(), true)?;
        }
    }

    // Each layer.conf, with LAYERDIR set; references replaced afterwards (expandVarref).
    let bblayers_via = r.via_of_word("BBLAYERS", None);
    for layer in &ws.layers {
        let conf = normalize(&layer.root.join("conf/layer.conf"));
        if !r.exists(&conf) {
            continue;
        }
        let dir = layer.root.to_string_lossy().to_string();
        put(&mut r.ds, "LAYERDIR", &dir);
        put(&mut r.ds, "LAYERDIR_RE", &re_escape(&dir));
        r.visit(&conf, FileKind::Conf, bblayers_via.clone(), true)?;
        expand_varref(&mut r.ds, "LAYERDIR");
        expand_varref(&mut r.ds, "LAYERDIR_RE");
    }
    remove_var(&mut r.ds, "LAYERDIR_RE");
    remove_var(&mut r.ds, "LAYERDIR");

    // A kas project without a build directory: kas would write `MACHINE ??=` and
    // `DISTRO ??=` from its files into the `local.conf` it generates (kas 5.5 `libcmds.py`
    // `WriteBBConfig._write_local_conf`), so they are applied where `bitbake.conf`
    // includes `conf/local.conf` (`Replay::include_words`), with that operator.
    let kas = kas_defaults(ws);
    r.kas_local = kas.clone();

    // conf/bitbake.conf on BBPATH (ConfHandler.resolve_file: BBPATH only).
    let bitbake_conf = r
        .bbpath()
        .into_iter()
        .map(|e| normalize(&e.join("conf/bitbake.conf")))
        .find(|p| r.exists(p));
    match bitbake_conf {
        Some(p) => r.visit(&p, FileKind::Conf, Vec::new(), true)?,
        None => {
            r.issue(
                None,
                &[],
                "conf",
                "conf/bitbake.conf",
                RefProblem::NotFound("conf/bitbake.conf".into()),
            );
            // Nothing to include local.conf (before `kas checkout` OE-core is not there):
            // the kas values are still the best answer for the machine and distro.
            r.apply_kas_local();
        }
    }
    // A bitbake.conf that never includes conf/local.conf: kas's local.conf is not read.
    let kas: Vec<(String, String, String)> = if r.kas_local.is_empty() {
        kas
    } else {
        Vec::new()
    };

    // ["base"] + INHERIT, as global classes.
    let inherit = r.ds.value("INHERIT").map(|v| v.text).unwrap_or_default();
    let mut classes = vec!["base".to_string()];
    classes.extend(pwords::words(&inherit).into_iter().map(str::to_string));
    for c in classes {
        let via = if c == "base" {
            Vec::new()
        } else {
            r.via_of_word("INHERIT", Some(&c))
        };
        r.inherit(&c, None, &via, false, "INHERIT")?;
    }

    // Where the machine came from, when not from the configuration or the arguments.
    let mut target_notes = Vec::new();
    for (name, value, from) in &kas {
        let ours = r.ds.vars.get(name.as_str()).is_some_and(|v| {
            v.content.is_none()
                && v.default
                    .as_ref()
                    .is_some_and(|d| d.src.is_empty() && d.text == *value)
        });
        if ours {
            target_notes.push(format!(
                "note: {name} = \"{value}\" from {from} (kas project without a build directory; kas writes it into local.conf as {name} ??=)"
            ));
        }
    }
    if target.machine.is_none() && r.ds.value("MACHINE").is_none_or(|v| v.text.is_empty()) {
        target_notes.push(
            "note: no MACHINE is set (not in the configuration, its fragments or the arguments), so machine-specific values are not applied (conf/machine/<machine>.conf, machine overrides); pass machine to evaluate for one"
                .to_string(),
        );
    }
    // A static BBMASK is applied by the index (`files.not_collected = 'bbmask'`); one the
    // replay cannot expand or compile is said to be unapplied.
    if let super::BbMask::Unusable(why) = super::bbmask(&r.ds) {
        target_notes.push(format!(
            "note: BBMASK is set but sous-chef does not apply it ({why}); recipes and bbappends it may mask are still treated as collected"
        ));
    }
    if !target_notes.is_empty() {
        Arc::make_mut(&mut r.ds.notes)
            .target_notes
            .extend(target_notes);
    }
    Ok(r.ds)
}

/// `(variable, value, where it came from)` for `MACHINE` and `DISTRO` of a kas project
/// whose build directory does not exist yet (`KasProject::machine_or_default`,
/// `distro_or_default`); empty otherwise.
fn kas_defaults(ws: &Workspace) -> Vec<(String, String, String)> {
    let Some(k) = ws.kas.as_ref().filter(|k| !k.files.is_empty()) else {
        return Vec::new();
    };
    if ws.build_dir.is_some() {
        return Vec::new();
    }
    vec![
        (
            "MACHINE".to_string(),
            k.machine_or_default().to_string(),
            kas_origin(k, true),
        ),
        (
            "DISTRO".to_string(),
            k.distro_or_default().to_string(),
            kas_origin(k, false),
        ),
    ]
}

/// Where kas's machine (`machine`) or distro value comes from, for the target note
/// ("`MACHINE = "x"` from <this>"): `KasProject::setting_source`, which names a
/// `KAS_MACHINE` / `KAS_DISTRO` environment override as such.
fn kas_origin(k: &crate::workspace::kas::KasProject, machine: bool) -> String {
    k.setting_source(machine)
}

fn recipe_pass(
    index: &Index,
    ws: &Workspace,
    files: &Files,
    config: &DataStore,
    recipe: &FileRow,
    overlay: Option<&Overlay>,
    variant: Option<&ClassExtend>,
) -> Result<DataStore> {
    let mut r = Replay::new(index, ws, files, config.fork(), false, true, Vec::new());
    r.overlay = overlay;
    let recipe_loc = recipe.loc(LineSpan::line(1));
    let abs = ws
        .abs_path(&recipe_loc)
        .map(|p| normalize(&p))
        .ok_or_else(|| Error::NotFound {
            what: format!("layer of {recipe_loc}"),
        })?;
    let layername = file_layername(&r.ds, ws, &abs, &recipe.layer);
    if let Some(l) = &layername {
        put(&mut r.ds, "FILE_LAYERNAME", l);
    }
    let not_collected = query::not_collected(index, recipe.id)?;
    {
        let notes = Arc::make_mut(&mut r.ds.notes);
        notes.recipe = Some(recipe_loc.clone());
        notes.layer_name = layername;
        if let Some(why) = not_collected {
            // cooker.py: only collected files are recipes (`collect_bbfiles`); a file named
            // with `bitbake -b` is parsed anyway (`matchFiles` takes any existing path).
            notes.target_notes.push(format!(
                "note: {} is {why}: BitBake does not collect it, so no recipe name or \
                 provider resolves to it and no bbappend applies; it is evaluated as \
                 `bitbake -b` would parse it",
                format_args!("{}:{}", recipe.layer, recipe.path.display())
            ));
        }
    }
    r.visit(&abs, FileKind::Recipe, Vec::new(), false)?;

    for a in query::appends_for(index, recipe.id)? {
        let loc = a.loc(LineSpan::line(1));
        let Some(aabs) = ws.abs_path(&loc).map(|p| normalize(&p)) else {
            continue;
        };
        Arc::make_mut(&mut r.ds.notes).appends.push(loc);
        r.visit(&aabs, FileKind::Append, vec![recipe_loc.clone()], true)?;
    }

    let Some(ext) = variant else {
        r.finalize(&recipe_loc)?;
        return Ok(r.ds);
    };
    // ast.multi_finalize: the base is finalised on a copy, and BBCLASSEXTEND and PN are
    // read from it; the variant starts again from the datastore before finalisation.
    let safe = r.ds.fork();
    r.finalize(&recipe_loc)?;
    let label = format!("{}:{}", recipe.layer, recipe.path.display());
    let base_pn = match static_value(&r.ds, "PN") {
        Some(Some(pn)) => pn,
        _ => {
            return Err(Error::NotFound {
                what: format!(
                    "the {} variant of {label}: the base recipe's PN is not known statically",
                    ext.word
                ),
            });
        }
    };
    let extend = r.ds.value("BBCLASSEXTEND").unwrap_or_default();
    let words = pwords::known_words(&extend.text);
    if !words.contains(&ext.word.as_str()) {
        let unsure = extend.unevaluated_python
            || !extend.unresolved.is_empty()
            || r.ds.replay().python_vars.contains("BBCLASSEXTEND");
        return Err(Error::NotFound {
            what: format!(
                "{} of {base_pn} ({label}): its BBCLASSEXTEND (\"{}\") does not list `{}`{}",
                ext.describe(),
                extend.text,
                ext.word,
                if unsure {
                    "; the value depends on python that was not evaluated, so BitBake may still make the variant"
                } else {
                    ""
                }
            ),
        });
    }
    r.ds = safe;
    r.extend_variant(ext, &base_pn, &recipe_loc)?;
    r.finalize(&recipe_loc)?;
    // The name comes from OE-core's naming (eval::variant); the variant's PN is what its
    // class made of it.
    if let Some(name) = ext.variant_name(&base_pn) {
        let pn = r.ds.value("PN").map(|v| v.text).unwrap_or_default();
        if pn != name {
            Arc::make_mut(&mut r.ds.notes).target_notes.push(format!(
                "note: this is {} of {base_pn}, which OE-core names {name}, but the evaluated PN is \"{pn}\": the `{}` class here does not rename it as OE-core's does, so BitBake may know this variant only as \"{pn}\"",
                ext.describe(),
                ext.class()
            ));
        }
    }
    Ok(r.ds)
}

/// `bb.utils.to_boolean` on a static value: `Some(true)` / `Some(false)`, `None` when it
/// cannot be told (python, undefined variables, or a word BitBake rejects).
fn static_bool(ds: &DataStore, name: &str) -> Option<bool> {
    let Some(v) = ds.value(name) else {
        return Some(false);
    };
    if v.unevaluated_python || !v.unresolved.is_empty() {
        return None;
    }
    match v.text.trim().to_ascii_lowercase().as_str() {
        "" | "no" | "n" | "false" | "f" | "off" | "0" => Some(false),
        "yes" | "y" | "true" | "t" | "on" | "1" => Some(true),
        _ => None,
    }
}

/// A value with no python and no undefined reference, and no anonymous python that sets the
/// variable by name.
fn static_value(ds: &DataStore, name: &str) -> Option<Option<String>> {
    if ds.replay().python_vars.contains(name) {
        return None;
    }
    match ds.value(name) {
        None => Some(None),
        Some(v) if v.unevaluated_python || !v.unresolved.is_empty() => None,
        Some(v) => Some(Some(v.text)),
    }
}

/// What is known of the list value `name` ([`skips::Words`]): unset is empty and known; a
/// value anonymous python or a handler changes by name is not known at all; otherwise its
/// known words, complete when it holds no python and no undefined variable.
fn words_of(ds: &DataStore, name: &str) -> skips::Words {
    if ds.replay().python_vars.contains(name) {
        return skips::Words {
            known: BTreeSet::new(),
            complete: false,
            why: Some(unknown_why(ds, name)),
        };
    }
    let Some(v) = ds.value(name) else {
        return skips::Words::of("");
    };
    let all = pwords::words(&v.text);
    let known: BTreeSet<String> = all
        .iter()
        .filter(|w| !pwords::is_unevaluated(w))
        .map(|w| w.to_string())
        .collect();
    let complete = !v.unevaluated_python
        && v.unresolved.is_empty()
        && all.iter().all(|w| !pwords::is_unevaluated(w));
    skips::Words {
        known,
        complete,
        why: (!complete).then(|| unknown_why(ds, name)),
    }
}

/// The reasons BitBake would, or may, skip the recipe, as `note:` lines. OE-core
/// `classes-global/base.bbclass` (Wrynose `33a6bd6c`) anonymous python, in this order:
/// `SKIP_RECIPE[<PN>]` non-empty raises `SkipRecipe("Recipe will be skipped because:
/// <msg>")`; a non-empty `COMPATIBLE_MACHINE`, unless `PARSE_ALL_RECIPES` is true, must
/// `re.match` (anchored at the start only) some entry of `(MACHINEOVERRIDES or
/// "").split(":")`, else "incompatible with machine <MACHINE> (not in COMPATIBLE_MACHINE)";
/// unless `SOURCE_MIRROR_FETCH` or `PARSE_ALL_RECIPES` is true, a non-empty
/// `COMPATIBLE_HOST` must `re.match` `HOST_SYS`, else "incompatible with host <HOST_SYS>
/// (not in COMPATIBLE_HOST)", and `INCOMPATIBLE_LICENSE` is checked ([`license_skip`]).
///
/// Three answers, never a guess: "would skip" when every value involved is known and the
/// check fails; nothing when the check passes on what is known (a `COMPATIBLE_MACHINE`
/// that matches a known `MACHINEOVERRIDES` entry matches); "may skip" when it fails on what
/// is known but values that are not known (python, undefined variables, anonymous python
/// that sets them by name, a regular expression Rust cannot compile) could still decide it.
/// `SKIP_RECIPE` is only said when its flag is static.
fn skip_reasons(ds: &DataStore) -> Vec<String> {
    const TAIL: &str = "(decided from static values; anonymous python is not run)";
    let mut out = Vec::new();
    if let Some(Some(pn)) = static_value(ds, "PN")
        && !ds.replay().python_vars.contains("SKIP_RECIPE")
        && let Some(msg) = ds.flag("SKIP_RECIPE", &pn)
        && !msg.unevaluated_python
        && msg.unresolved.is_empty()
        && !msg.text.is_empty()
    {
        out.push(format!(
            "note: BitBake would skip this recipe: SKIP_RECIPE[{pn}] = \"{}\" {TAIL}",
            msg.text
        ));
    }
    const MAY: &str = "note: BitBake may skip this recipe:";
    let parse_all = static_bool(ds, "PARSE_ALL_RECIPES");
    match static_value(ds, "COMPATIBLE_MACHINE") {
        _ if parse_all == Some(true) => {}
        Some(Some(need)) if need.is_empty() => {}
        Some(None) => {}
        None => out.push(format!(
            "{MAY} COMPATIBLE_MACHINE is set but not known statically ({}), so whether it matches MACHINEOVERRIDES cannot be told",
            unknown_why(ds, "COMPATIBLE_MACHINE")
        )),
        Some(Some(need)) => {
            let mo = ds.value("MACHINEOVERRIDES").unwrap_or_default();
            let entries = pwords::colon_parts(&mo.text);
            let known: Vec<&str> = entries
                .iter()
                .copied()
                .filter(|e| !pwords::is_unevaluated(e))
                .collect();
            match compatible_machine(&need, known.iter().copied()) {
            None => out.push(format!(
                "{MAY} COMPATIBLE_MACHINE = \"{need}\" is a regular expression sous-chef cannot compile, so whether it matches MACHINEOVERRIDES cannot be told"
            )),
            Some(hit) => {
                let unknown = entries.len() - known.len();
                let by_python = ds.replay().python_vars.contains("MACHINEOVERRIDES");
                if hit {
                    // A match on a known entry is a match: not skipped.
                } else if unknown > 0 || by_python || parse_all.is_none() {
                    let mut why = Vec::new();
                    if unknown > 0 {
                        why.push(format!(
                            "{unknown} part{} of MACHINEOVERRIDES {} not evaluated",
                            if unknown == 1 { "" } else { "s" },
                            if unknown == 1 { "is" } else { "are" }
                        ));
                    }
                    if by_python {
                        why.push("anonymous python may change MACHINEOVERRIDES".into());
                    }
                    if parse_all.is_none() {
                        why.push("PARSE_ALL_RECIPES is not known statically".into());
                    }
                    out.push(format!(
                        "{MAY} COMPATIBLE_MACHINE = \"{need}\" matches none of the known machine overrides ({}); {}",
                        if known.iter().all(|k| k.is_empty()) {
                            "none is known".to_string()
                        } else {
                            known.join(":")
                        },
                        why.join("; ")
                    ));
                } else {
                    let machine = ds.value("MACHINE").map(|v| v.text).unwrap_or_default();
                    out.push(format!(
                        "note: BitBake would skip this recipe: incompatible with machine {machine} (COMPATIBLE_MACHINE = \"{need}\" matches no entry of MACHINEOVERRIDES = \"{}\") {TAIL}",
                        mo.text
                    ));
                }
            }
            }
        }
    }
    let mirror = static_bool(ds, "SOURCE_MIRROR_FETCH");
    let host_checked = match (parse_all, mirror) {
        (Some(false), Some(false)) => Some(true),
        (Some(true), _) | (_, Some(true)) => Some(false),
        _ => None,
    };
    if host_checked == Some(false) {
        return out;
    }
    let unsure_check = if host_checked.is_none() {
        "; SOURCE_MIRROR_FETCH or PARSE_ALL_RECIPES is not known statically"
    } else {
        ""
    };
    match static_value(ds, "COMPATIBLE_HOST") {
        Some(None) => {}
        Some(Some(need)) if need.is_empty() => {}
        None => out.push(format!(
            "{MAY} COMPATIBLE_HOST is set but not known statically ({}), so whether it matches HOST_SYS cannot be told",
            unknown_why(ds, "COMPATIBLE_HOST")
        )),
        Some(Some(need)) => match (anchored(&need), static_value(ds, "HOST_SYS")) {
            (None, _) => out.push(format!(
                "{MAY} COMPATIBLE_HOST = \"{need}\" is a regular expression sous-chef cannot compile, so whether it matches HOST_SYS cannot be told"
            )),
            (Some(_), None) => out.push(format!(
                "{MAY} COMPATIBLE_HOST = \"{need}\"; HOST_SYS is not known statically ({})",
                unknown_why(ds, "HOST_SYS")
            )),
            (Some(_), Some(None)) => out.push(format!(
                "{MAY} COMPATIBLE_HOST = \"{need}\"; HOST_SYS is not set (BitBake's re.match would raise)"
            )),
            (Some(_), Some(Some(host))) => {
                if compatible_host(&need, &host) == Some(false) {
                    if host_checked.is_none() {
                        out.push(format!(
                            "{MAY} COMPATIBLE_HOST = \"{need}\" does not match HOST_SYS = \"{host}\"{unsure_check}"
                        ));
                    } else {
                        out.push(format!(
                            "note: BitBake would skip this recipe: incompatible with host {host} (COMPATIBLE_HOST = \"{need}\" does not match HOST_SYS) {TAIL}"
                        ));
                    }
                }
            }
        },
    }
    out.extend(license_skip(ds, host_checked.is_none()));
    out
}

/// `re.match(need, ...)`: the regular expression anchored at the start only; `None` when
/// Rust's `regex` cannot compile it (Python's `re` may: lookaround, backreferences).
fn anchored(need: &str) -> Option<Regex> {
    Regex::new(&format!("^(?:{need})")).ok()
}

/// OE-core `classes-global/base.bbclass` (Wrynose `33a6bd6c`, anonymous python): a non-empty
/// `COMPATIBLE_MACHINE` is compatible when `re.match(COMPATIBLE_MACHINE, m)` for some entry
/// `m` of `MACHINEOVERRIDES.split(":")` (given here as `entries`). `None` when the regular
/// expression cannot be compiled. The check the skip notes make, callable with given values.
pub fn compatible_machine<'a>(
    need: &str,
    entries: impl IntoIterator<Item = &'a str>,
) -> Option<bool> {
    let re = anchored(need)?;
    Some(entries.into_iter().any(|e| re.is_match(e)))
}

/// OE-core `classes-global/base.bbclass` (Wrynose `33a6bd6c`, anonymous python): a non-empty
/// `COMPATIBLE_HOST` is compatible when `re.match(COMPATIBLE_HOST, HOST_SYS)`. `None` when
/// the regular expression cannot be compiled. The check the skip notes make, callable with
/// given values.
pub fn compatible_host(need: &str, host_sys: &str) -> Option<bool> {
    Some(anchored(need)?.is_match(host_sys))
}

/// Why `name`'s value is not static, for a note.
fn unknown_why(ds: &DataStore, name: &str) -> String {
    if ds.replay().python_vars.contains(name) {
        return "anonymous python sets it".into();
    }
    match ds.value(name) {
        Some(v) if v.unevaluated_python => "it holds python".into(),
        Some(v) if !v.unresolved.is_empty() => {
            format!("it uses undefined variables: {}", v.unresolved.join(", "))
        }
        _ => "not known".into(),
    }
}

/// One package's outcome in `skip_incompatible_package_licenses`.
enum PkgLicense {
    /// Incompatible licenses: the package is skipped.
    Skipped(Vec<String>),
    Kept,
    /// Cannot be told; why.
    Unknown(String),
}

/// `INCOMPATIBLE_LICENSE` (OE-core Wrynose `33a6bd6c`, `classes-global/base.bbclass`
/// anonymous python and `lib/oe/license.py` `skip_incompatible_package_licenses`,
/// `incompatible_license`, `incompatible_pkg_license`, `expand_wildcard_licenses`,
/// `apply_pkg_license_exception`, `canonical_license`). With `bad =
/// INCOMPATIBLE_LICENSE.split()` and `pkgs = PACKAGES.split()`: nothing when `pkgs` is
/// empty. A package is skipped when `incompatible_license(d, remaining, pkg)` is not empty,
/// where `remaining` is `expand_wildcard_licenses(bad)` (`AGPL-3.0*`, `GPL-3.0*`,
/// `LGPL-3.0*` become their `-only` / `-or-later` names; an obsolete name or another `*` is
/// `bb.fatal`) without the `<pkg>:<lic>` entries of `INCOMPATIBLE_LICENSE_EXCEPTIONS`, and
/// the license is `LICENSE:<pkg>` or else `LICENSE`, flattened choosing, at each `|`, the
/// left side when all its licenses (mapped through `SPDXLICENSEMAP[lic]`) match no pattern
/// (`fnmatchcase`), else the right. No package is skipped when `PN` starts with
/// `nativesdk-` or `gcc-source-`, or ends with `-native`, `-cross-${TARGET_ARCH}`,
/// `-cross-initial-${TARGET_ARCH}`, `-crosssdk-${SDK_SYS}`, `-crosssdk-initial-${SDK_SYS}`
/// or `-cross-canadian-${TRANSLATED_TARGET_ARCH}`. The recipe is skipped only when every
/// package is, with "it has incompatible license(s): <sorted, unique: the recipe's own
/// incompatible licenses (against `bad` unexpanded) and every package's>".
fn license_skip(ds: &DataStore, check_unsure: bool) -> Vec<String> {
    const MAY: &str = "note: BitBake may skip this recipe:";
    const TAIL: &str = "(decided from static values; anonymous python is not run)";
    let may = |why: String| vec![format!("{MAY} {why}")];
    let unsure = if check_unsure {
        "; SOURCE_MIRROR_FETCH or PARSE_ALL_RECIPES is not known statically"
    } else {
        ""
    };
    let bad: Vec<String> = match static_value(ds, "INCOMPATIBLE_LICENSE") {
        Some(v) => pwords::words(&v.unwrap_or_default())
            .into_iter()
            .map(str::to_string)
            .collect(),
        None => {
            return may(format!(
                "INCOMPATIBLE_LICENSE is set but not known statically ({})",
                unknown_why(ds, "INCOMPATIBLE_LICENSE")
            ));
        }
    };
    if bad.is_empty() {
        return Vec::new();
    }
    let Some(Some(pn)) = static_value(ds, "PN") else {
        return may("INCOMPATIBLE_LICENSE is set and PN is not known statically".into());
    };
    if pn.starts_with("nativesdk-") || pn.starts_with("gcc-source-") {
        return Vec::new();
    }
    for t in [
        "-native",
        "-cross-${TARGET_ARCH}",
        "-cross-initial-${TARGET_ARCH}",
        "-crosssdk-${SDK_SYS}",
        "-crosssdk-initial-${SDK_SYS}",
        "-cross-canadian-${TRANSLATED_TARGET_ARCH}",
    ] {
        let e = ds.expand(t);
        if e.unevaluated_python || !e.unresolved.is_empty() {
            // The suffix is not known; only a PN that could end with it is in doubt.
            let stem = t.split("${").next().unwrap_or(t);
            if pn.contains(stem) {
                return may(format!(
                    "INCOMPATIBLE_LICENSE is set and whether {pn} ends with {t} (no license check) cannot be told"
                ));
            }
            continue;
        }
        if pn.ends_with(&e.text) {
            return Vec::new();
        }
    }
    let expanded = match license::expand_wildcards(&bad) {
        Ok(x) => x,
        Err(fatal) => {
            return vec![format!(
                "note: BitBake would stop parsing this recipe: {fatal} {TAIL}"
            )];
        }
    };
    let exceptions: Vec<String> = match static_value(ds, "INCOMPATIBLE_LICENSE_EXCEPTIONS") {
        Some(v) => pwords::words(&v.unwrap_or_default())
            .into_iter()
            .map(str::to_string)
            .collect(),
        None => {
            return may(format!(
                "INCOMPATIBLE_LICENSE_EXCEPTIONS is not known statically ({})",
                unknown_why(ds, "INCOMPATIBLE_LICENSE_EXCEPTIONS")
            ));
        }
    };
    for e in &exceptions {
        let lic = e.split(':').nth(1).unwrap_or(e);
        if license::OBSOLETE.contains(&lic) {
            return vec![format!(
                "note: BitBake would stop parsing this recipe: Obsolete license {lic} used in INCOMPATIBLE_LICENSE_EXCEPTIONS {TAIL}"
            )];
        }
    }
    let pkgs: Vec<String> = match static_value(ds, "PACKAGES") {
        Some(v) => pwords::words(&v.unwrap_or_default())
            .into_iter()
            .map(str::to_string)
            .collect(),
        None => {
            return may(format!(
                "INCOMPATIBLE_LICENSE is set and PACKAGES is not known statically ({})",
                unknown_why(ds, "PACKAGES")
            ));
        }
    };
    if pkgs.is_empty() {
        return Vec::new();
    }
    let mut skipped: Vec<(String, Vec<String>)> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    for pkg in &pkgs {
        let remaining: Vec<String> = expanded
            .iter()
            .filter(|l| !exceptions.contains(&format!("{pkg}:{l}")))
            .cloned()
            .collect();
        match package_license(ds, Some(pkg), &remaining) {
            PkgLicense::Kept => return Vec::new(),
            PkgLicense::Skipped(l) => skipped.push((pkg.clone(), l)),
            PkgLicense::Unknown(why) => unknown.push(why),
        }
    }
    if !unknown.is_empty() {
        let n = unknown.len();
        unknown.sort();
        unknown.dedup();
        return may(format!(
            "INCOMPATIBLE_LICENSE = \"{}\": {} of {} package{} have incompatible licenses; for the other {n} it cannot be told ({}){unsure}",
            bad.join(" "),
            skipped.len(),
            pkgs.len(),
            if pkgs.len() == 1 { "" } else { "s" },
            unknown.join("; ")
        ));
    }
    let mut all: Vec<String> = match package_license(ds, None, &bad) {
        PkgLicense::Skipped(l) => l,
        PkgLicense::Kept => Vec::new(),
        PkgLicense::Unknown(why) => {
            return may(format!(
                "INCOMPATIBLE_LICENSE = \"{}\": every package has incompatible licenses, and the recipe's LICENSE cannot be read ({why}){unsure}",
                bad.join(" ")
            ));
        }
    };
    for (_, l) in skipped {
        all.extend(l);
    }
    all.sort();
    all.dedup();
    if check_unsure {
        return may(format!(
            "it has incompatible license(s): {} (INCOMPATIBLE_LICENSE = \"{}\"){unsure}",
            all.join(" "),
            bad.join(" ")
        ));
    }
    vec![format!(
        "note: BitBake would skip this recipe: it has incompatible license(s): {} (INCOMPATIBLE_LICENSE = \"{}\") {TAIL}",
        all.join(" "),
        bad.join(" ")
    )]
}

/// `incompatible_license(d, dont_want, package)`: the incompatible licenses of
/// `LICENSE:<package>` (or `LICENSE` when that is empty or `package` is `None`).
fn package_license(ds: &DataStore, package: Option<&str>, dont_want: &[String]) -> PkgLicense {
    let own = package.map(|p| format!("LICENSE:{p}"));
    let name = match &own {
        Some(n) => match static_value(ds, n) {
            Some(Some(v)) if !v.is_empty() => n.as_str(),
            Some(_) => "LICENSE",
            None => return PkgLicense::Unknown(format!("{n}: {}", unknown_why(ds, n))),
        },
        None => "LICENSE",
    };
    let text = match static_value(ds, name) {
        Some(v) => v.unwrap_or_default(),
        None => return PkgLicense::Unknown(format!("{name}: {}", unknown_why(ds, name))),
    };
    // canonical_license: SPDXLICENSEMAP[lic] or lic. `None` when the flag is not static.
    let canonical = |lic: &str| -> Option<String> {
        match ds.flag("SPDXLICENSEMAP", lic) {
            None => Some(lic.to_string()),
            Some(v) if v.unevaluated_python || !v.unresolved.is_empty() => None,
            Some(v) if v.text.is_empty() => Some(lic.to_string()),
            Some(v) => Some(v.text),
        }
    };
    let ok = |lic: &str| !dont_want.iter().any(|p| license::fnmatch(lic, p));
    let unknown_map = std::cell::Cell::new(false);
    let choose = |a: &[String], _: &[String]| -> bool {
        a.iter().all(|l| match canonical(l) {
            Some(c) => ok(&c),
            None => {
                unknown_map.set(true);
                true
            }
        })
    };
    let lics = match license::flatten(&text, &choose) {
        Ok(l) => l,
        Err(e) => return PkgLicense::Unknown(format!("{name} = \"{text}\": {e}")),
    };
    let mut bad = Vec::new();
    for l in &lics {
        match canonical(l) {
            Some(c) => {
                if !ok(&c) {
                    bad.push(c);
                }
            }
            None => unknown_map.set(true),
        }
    }
    if unknown_map.get() {
        return PkgLicense::Unknown("SPDXLICENSEMAP is not known statically".into());
    }
    bad.sort();
    if bad.is_empty() {
        PkgLicense::Kept
    } else {
        PkgLicense::Skipped(bad)
    }
}

/// `ast.handleVirtRecipeProviders` (BitBake 2.12 and later), on `DEPENDS`: each word listed
/// in `BB_RECIPE_VIRTUAL_PROVIDERS` is replaced with the value of
/// `PREFERRED_PROVIDER_<word>`, and the result is stored with a plain `setVar` (no more
/// `:append` / override variants), whether or not a word was replaced: the words joined
/// with single spaces (done here when that changes a certain value), and `""` when
/// `DEPENDS` is unset. An unset `BB_RECIPE_VIRTUAL_PROVIDERS` is an empty list. The task
/// `depends` flags are rewritten the same way ([`task_depends`]).
///
/// The new value keeps the flags of the old one: `unevaluated_python` (through
/// `python_vars`) and `unresolved` (through `unresolved_vars`). A list that needs python or
/// undefined variables flags `DEPENDS`; a word without a provider is BitBake's fatal "recipe
/// virtual provider PREFERRED_PROVIDER_x not set" (a reference issue; the word is kept).
fn virtual_recipe_providers(ds: &mut DataStore, recipe: &Loc) {
    const DEPENDS: &str = "DEPENDS";
    const LIST: &str = "BB_RECIPE_VIRTUAL_PROVIDERS";
    // `(d.getVar("BB_RECIPE_VIRTUAL_PROVIDERS") or "").split()`: unset is an empty list.
    let list = ds.value(LIST).unwrap_or_default();
    if list.unevaluated_python || !list.unresolved.is_empty() {
        let notes = Arc::make_mut(&mut ds.notes);
        if list.unevaluated_python {
            notes.python_vars.insert(DEPENDS.to_string());
        }
        let e = notes
            .unresolved_vars
            .entry(DEPENDS.to_string())
            .or_default();
        for u in list.unresolved.iter().map(String::as_str).chain([LIST]) {
            if !e.iter().any(|x| x == u) {
                e.push(u.to_string());
            }
        }
        ds.ovr_cache = OnceLock::new();
        return;
    }
    let virt: Vec<String> = pwords::words(&list.text)
        .into_iter()
        .map(str::to_string)
        .collect();
    task_depends(ds, &virt, recipe);
    let virt: Vec<&str> = virt.iter().map(String::as_str).collect();
    let Some(deps) = ds.value(DEPENDS) else {
        // `(d.getVar("DEPENDS") or "").split()` then `setVar("DEPENDS", "")`.
        ds.rewrite(
            DEPENDS,
            String::new(),
            recipe.clone(),
            OpKind::VirtualProviderRewrite,
        );
        Arc::make_mut(&mut ds.notes)
            .var_notes
            .entry(DEPENDS.to_string())
            .or_default()
            .push("note: DEPENDS was unset; BitBake sets it to \"\" at the end of parsing (handleVirtRecipeProviders)".to_string());
        return;
    };
    let mut words: Vec<String> = Vec::new();
    let mut rewrites: Vec<(String, String, Option<Loc>)> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut python = deps.unevaluated_python;
    let mut unresolved: Vec<String> = deps.unresolved.clone();
    // A `${@...}` in DEPENDS is one word, kept as written (BitBake splits the value after
    // running the python; the replay cannot, and must not cut the expression apart).
    for w in pwords::words(&deps.text) {
        if !virt.contains(&w) {
            words.push(w.to_string());
            continue;
        }
        let pp = format!("PREFERRED_PROVIDER_{w}");
        match preferred_provider(ds, w) {
            Some((v, loc)) => {
                python |= v.unevaluated_python;
                unresolved.extend(v.unresolved.iter().cloned());
                words.push(v.text.clone());
                rewrites.push((w.to_string(), v.text, loc));
            }
            None => {
                missing.push(pp);
                words.push(w.to_string());
            }
        }
    }
    for pp in &missing {
        Arc::make_mut(&mut ds.notes).references.push(RefIssue {
            loc: Some(recipe.clone()),
            via: Vec::new(),
            kind: "virtual_provider".into(),
            text: pp.clone(),
            problem: RefProblem::Fatal(format!(
                "Error, recipe virtual provider {pp} not set (BB_RECIPE_VIRTUAL_PROVIDERS)"
            )),
        });
    }
    let text = words.join(" ");
    if rewrites.is_empty() {
        // `setVar("DEPENDS", " ".join(newdeps))` all the same: the words joined with single
        // spaces. Done when that changes the text and the value is certain (an uncertain
        // value is flagged anyway, and its words may not be the ones BitBake splits).
        if text != deps.text && !python && unresolved.is_empty() {
            ds.rewrite(
                DEPENDS,
                text,
                recipe.clone(),
                OpKind::VirtualProviderRewrite,
            );
            Arc::make_mut(&mut ds.notes)
                .var_notes
                .entry(DEPENDS.to_string())
                .or_default()
                .push("note: DEPENDS stored again at the end of parsing with its words joined by single spaces (BitBake handleVirtRecipeProviders; no BB_RECIPE_VIRTUAL_PROVIDERS word in it)".to_string());
        }
        return;
    }
    // setVar(DEPENDS, text) after parsing: the keyword lists and the override variants go.
    // Recorded as a rewrite, not a statement: what made the old value stays applied.
    let loc = rewrites
        .iter()
        .find_map(|(_, _, l)| l.clone())
        .unwrap_or_else(|| recipe.clone());
    ds.rewrite(DEPENDS, text, loc, OpKind::VirtualProviderRewrite);
    let notes = Arc::make_mut(&mut ds.notes);
    if python {
        notes.python_vars.insert(DEPENDS.to_string());
    }
    if !unresolved.is_empty() {
        let e = notes
            .unresolved_vars
            .entry(DEPENDS.to_string())
            .or_default();
        for u in &unresolved {
            if !e.contains(u) {
                e.push(u.clone());
            }
        }
    }
    let line = rewrites
        .iter()
        .map(|(w, p, l)| match l {
            Some(l) => format!("{w} -> {p} (PREFERRED_PROVIDER_{w} at {l})"),
            None => format!("{w} -> {p} (PREFERRED_PROVIDER_{w})"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    notes.var_notes.entry(DEPENDS.to_string()).or_default().push(format!(
        "note: DEPENDS rewritten at the end of parsing (BB_RECIPE_VIRTUAL_PROVIDERS, BitBake handleVirtRecipeProviders): {line}"
    ));
    ds.ovr_cache = OnceLock::new();
}

/// `getVar("PREFERRED_PROVIDER_<word>")` when set and not empty, with the location of the
/// last applied operation on it.
fn preferred_provider(ds: &DataStore, word: &str) -> Option<(super::Value, Option<Loc>)> {
    let pp = format!("PREFERRED_PROVIDER_{word}");
    let v = ds.value(&pp).filter(|v| !v.text.is_empty())?;
    let loc = ds
        .history(&pp)
        .into_iter()
        .filter(|(_, s)| *s == OpStatus::Applied)
        .map(|(op, _)| op.loc)
        .next_back();
    Some((v, loc))
}

/// The task half of `handleVirtRecipeProviders` (BitBake 2.18.0 `lib/bb/parse/ast.py`): for
/// every task in `__BBTASKS` (see [`DataStore::tasks`]), `getVarFlag(task, "depends")`
/// (expanded) is split on whitespace, each entry split with `entry.split(":")` into
/// `r, t` (anything but exactly one `:` is a python `ValueError`, which fails the parse);
/// an `r` listed in `BB_RECIPE_VIRTUAL_PROVIDERS` becomes `getVar("PREFERRED_PROVIDER_" +
/// r)` (no check here: unset gives the text `None`), and the entries joined with single
/// spaces are stored with `setVarFlag`. Recorded as an operation on the flag of kind
/// [`OpKind::VirtualProviderRewrite`], when the text changes. Not reproduced: BitBake also
/// stores `""` on a task without the flag (every reader treats unset and empty alike), and
/// tasks or flags added by anonymous python are not seen; a flag whose value holds python
/// or undefined variables is left as it is.
fn task_depends(ds: &mut DataStore, virt: &[String], recipe: &Loc) {
    const FLAG: &str = "depends";
    for task in ds.notes.tasks.clone() {
        let Some(v) = ds.flag(&task, FLAG) else {
            continue;
        };
        if v.unevaluated_python || !v.unresolved.is_empty() {
            continue;
        }
        let mut out: Vec<String> = Vec::new();
        let mut rewrites: Vec<String> = Vec::new();
        let mut at: Option<Loc> = None;
        let mut failed = false;
        for entry in v.text.split_whitespace() {
            let parts: Vec<&str> = entry.split(':').collect();
            let [r, t] = parts.as_slice() else {
                Arc::make_mut(&mut ds.notes).references.push(RefIssue {
                    loc: Some(recipe.clone()),
                    via: Vec::new(),
                    kind: "virtual_provider".into(),
                    text: format!("{task}[{FLAG}]"),
                    problem: RefProblem::Fatal(format!(
                        "ValueError in handleVirtRecipeProviders: {task}[{FLAG}] entry {entry:?} is not `recipe:task`"
                    )),
                });
                failed = true;
                break;
            };
            if !virt.iter().any(|w| w == r) {
                out.push(entry.to_string());
                continue;
            }
            let (new, loc) = match preferred_provider(ds, r) {
                Some((p, loc)) => (p.text, loc),
                None => ("None".to_string(), None),
            };
            rewrites.push(match &loc {
                Some(l) => format!("{r} -> {new} (PREFERRED_PROVIDER_{r} at {l})"),
                None if new == "None" => {
                    format!("{r} -> None (PREFERRED_PROVIDER_{r} is not set; BitBake writes the text None)")
                }
                None => format!("{r} -> {new} (PREFERRED_PROVIDER_{r})"),
            });
            at = at.or(loc);
            out.push(format!("{new}:{t}"));
        }
        let text = out.join(" ");
        if failed || text == v.text {
            continue;
        }
        ds.apply(VarOp {
            var: VarRef {
                base: task.clone(),
                overrides: Vec::new(),
                flag: Some(FLAG.to_string()),
                legacy_syntax: false,
            },
            op: AssignOp::Set,
            value: text,
            loc: at.unwrap_or_else(|| recipe.clone()),
            via: Vec::new(),
            kind: OpKind::VirtualProviderRewrite,
        });
        let line = if rewrites.is_empty() {
            "its entries joined by single spaces".to_string()
        } else {
            rewrites.join(", ")
        };
        Arc::make_mut(&mut ds.notes)
            .var_notes
            .entry(task.clone())
            .or_default()
            .push(format!(
                "note: {task}[{FLAG}] rewritten at the end of parsing (BB_RECIPE_VIRTUAL_PROVIDERS, BitBake handleVirtRecipeProviders): {line}"
            ));
    }
}

/// `FILE_LAYERNAME` (`CookerCollectFiles.get_layername`): the collection whose
/// `BBFILE_PATTERN` regex matches the start of the recipe path, patterns tried in reverse
/// lexical order. Falls back to the first collection of the recipe's layer when no pattern
/// can be evaluated.
fn file_layername(ds: &DataStore, ws: &Workspace, abs: &Path, layer: &str) -> Option<String> {
    let mut pats: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();
    for l in &ws.layers {
        for c in &l.collections {
            if !seen.insert(c.clone()) {
                continue;
            }
            if let Some(v) = ds.value(&format!("BBFILE_PATTERN_{c}"))
                && !v.unevaluated_python
                && v.unresolved.is_empty()
                && !v.text.is_empty()
            {
                pats.push((c.clone(), v.text));
            }
        }
    }
    pats.sort_by(|a, b| b.1.cmp(&a.1));
    let path = abs.to_string_lossy();
    for (c, p) in &pats {
        if let Ok(re) = Regex::new(&format!("^(?:{p})"))
            && re.is_match(&path)
        {
            return Some(c.clone());
        }
    }
    ws.layer(layer).and_then(|l| l.collections.first().cloned())
}

// ---------------------------------------------------------------------------------------
// Direct datastore writes (values BitBake sets itself, not metadata operations)
// ---------------------------------------------------------------------------------------

/// Sets the plain value of `name` (BitBake `setVar` from code), keeping the sources of the
/// value it replaces. Not recorded in the history.
fn put_keep(ds: &mut DataStore, name: &str, text: &str, src: Vec<u64>) {
    ds.var_mut(name).content = Some(Content {
        text: text.to_string(),
        src,
    });
    ds.ovr_cache = OnceLock::new();
}

fn put(ds: &mut DataStore, name: &str, text: &str) {
    put_keep(ds, name, text, Vec::new());
}

fn put_flag(ds: &mut DataStore, name: &str, flag: &str, text: &str) {
    ds.var_mut(name).flags.insert(
        flag.to_string(),
        Content {
            text: text.to_string(),
            src: Vec::new(),
        },
    );
}

fn remove_var(ds: &mut DataStore, name: &str) {
    if ds.vars.contains_key(name) {
        Arc::make_mut(&mut ds.vars).remove(name);
        ds.ovr_cache = OnceLock::new();
    }
}

/// `DataSmart.expandVarref`: every stored text containing `${var}` gets it replaced by the
/// value of `var`. (BitBake does this on the finalised value of each variable and stores
/// the result with a plain `setVar`, folding pending `:append`s into it; replacing inside
/// each piece gives the same final values.)
fn expand_varref(ds: &mut DataStore, var: &str) {
    let Some(value) = ds.raw(var) else { return };
    let pat = format!("${{{var}}}");
    let names: Vec<Arc<str>> = ds
        .vars
        .iter()
        .filter(|(k, v)| k.as_ref() != var && mentions(v, &pat))
        .map(|(k, _)| k.clone())
        .collect();
    for n in names {
        let v = ds.var_mut(&n);
        for c in v.content.iter_mut().chain(v.default.iter_mut()) {
            c.text = c.text.replace(&pat, &value);
        }
        for p in v
            .appends
            .iter_mut()
            .chain(v.prepends.iter_mut())
            .chain(v.removes.iter_mut())
        {
            p.text = p.text.replace(&pat, &value);
        }
    }
    ds.ovr_cache = OnceLock::new();
}

fn mentions(v: &Var, pat: &str) -> bool {
    v.content
        .iter()
        .chain(v.default.iter())
        .any(|c| c.text.contains(pat))
        || v.appends
            .iter()
            .chain(v.prepends.iter())
            .chain(v.removes.iter())
            .any(|p| p.text.contains(pat))
}

/// Python's `re.escape` (3.7 and later).
fn re_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "()[]{}?*+-|^$\\.&~# \t\n\r\x0b\x0c".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `bb.parse.vars_from_file`: `(PN, PV)` from a `.bb` / `.bbappend` name; nothing for any
/// other file or a name with more than three `_` parts (an error in BitBake).
fn vars_from_file(file: &str) -> (Option<String>, Option<String>) {
    let p = Path::new(file);
    let ext = p.extension().and_then(|e| e.to_str());
    if !matches!(ext, Some("bb") | Some("bbappend")) {
        return (None, None);
    }
    let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
        return (None, None);
    };
    let parts: Vec<&str> = stem.split('_').collect();
    if parts.len() > 3 {
        return (None, None);
    }
    let get = |i: usize| {
        parts
            .get(i)
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    };
    (get(0), get(1))
}

/// `SPECIAL_PKGSUFFIX` of OE-Core's `bitbake.conf`, used when the variable is not set.
const SPECIAL_PKGSUFFIX: &[&str] = &[
    "-native",
    "-cross",
    "-initial",
    "-intermediate",
    "-crosssdk",
    "-cross-canadian",
];

/// Variables computed from `FILE` in the recipe pass, in dependency order.
const COMPUTED: &[&str] = &["PN", "PV", "BPN", "BP", "FILE_DIRNAME", "THISDIR"];

/// Assignments to these names make the computed values be looked at again.
const COMPUTED_INPUTS: &[&str] = &[
    "PN",
    "PV",
    "BPN",
    "BP",
    "FILE_DIRNAME",
    "THISDIR",
    "SPECIAL_PKGSUFFIX",
    "MLPREFIX",
];

// ---------------------------------------------------------------------------------------
// The replay engine
// ---------------------------------------------------------------------------------------

struct Replay<'a> {
    index: &'a Index,
    ws: &'a Workspace,
    files: &'a Files,
    ds: DataStore,
    /// Files being parsed, outermost first.
    stack: Vec<PathBuf>,
    /// `__bbclasstype == "global"`.
    global: bool,
    recipe_pass: bool,
    pins: Vec<(&'static str, String)>,
    resolver: Option<(Vec<PathBuf>, Resolver)>,
    /// Uncertain references being followed, innermost last.
    taint: Vec<Taint>,
    /// Files replaced in memory (see [`Overlay`]).
    overlay: Option<&'a Overlay>,
    /// `(variable, value, origin)` kas would write into `local.conf` as `VAR ??= "value"`,
    /// still to be applied (a kas project without a build directory; see `config_pass`).
    kas_local: Vec<(String, String, String)>,
    /// `native_virtclass_handler` set its `native_filter` on `PROVIDES`: `(pn, bpn, where)`
    /// as the handler captured them (`bpn` `None` when not known statically).
    native_provides: Option<(String, Option<String>, Loc)>,
}

impl<'a> Replay<'a> {
    fn new(
        index: &'a Index,
        ws: &'a Workspace,
        files: &'a Files,
        ds: DataStore,
        global: bool,
        recipe_pass: bool,
        pins: Vec<(&'static str, String)>,
    ) -> Replay<'a> {
        Replay {
            index,
            ws,
            files,
            ds,
            stack: Vec::new(),
            global,
            recipe_pass,
            pins,
            resolver: None,
            taint: Vec::new(),
            overlay: None,
            kas_local: Vec::new(),
            native_provides: None,
        }
    }

    /// The `MACHINE ??=` / `DISTRO ??=` lines of the `local.conf` kas would generate,
    /// applied now (a weak default: the last `??=` wins, any other assignment beats it).
    /// Not recorded in the history: there is no file to point at.
    fn apply_kas_local(&mut self) {
        for (name, value, _) in std::mem::take(&mut self.kas_local) {
            self.ds.var_mut(&name).default = Some(Content {
                text: value,
                src: Vec::new(),
            });
            self.ds.ovr_cache = OnceLock::new();
        }
    }

    /// Whether `abs` is a file the replay can read: indexed, or in the overlay.
    fn exists(&self, abs: &Path) -> bool {
        self.files.by_abs.contains_key(abs) || self.overlay.is_some_and(|o| o.contains(abs))
    }

    /// `(layer, path, kind, statements)` of a file: from the overlay when it has the path,
    /// else from the index. `None` when neither knows it.
    fn file_statements(&self, abs: &Path) -> Result<Option<FileStatements>> {
        if let Some(f) = self.overlay.and_then(|o| o.get(abs)) {
            return Ok(Some((
                f.layer.clone(),
                f.path.clone(),
                f.kind,
                f.rows.clone(),
            )));
        }
        let Some(info) = self.files.by_abs.get(abs) else {
            return Ok(None);
        };
        let stmts = query::statements_in(self.index, info.id, &StatementFilter::default())?;
        Ok(Some((
            info.layer.clone(),
            info.path.clone(),
            info.kind,
            stmts,
        )))
    }

    // ----- files -----

    /// Parses one file: records the visit, sets `FILE` (not for classes) and feeds its
    /// statements. `restore`: put the previous `FILE` back afterwards (every file except
    /// the top-level recipe, which BitBake parses with `include == 0`).
    fn visit(&mut self, abs: &Path, kind: FileKind, via: Vec<Loc>, restore: bool) -> Result<()> {
        let Some((layer, path, file_kind, stmts)) = self.file_statements(abs)? else {
            return Ok(());
        };
        let end = stmts
            .iter()
            .map(|s| s.loc.lines.end)
            .max()
            .unwrap_or(1)
            .max(1);
        self.ds.record_visit(FileVisit {
            loc: Loc {
                layer,
                path,
                lines: LineSpan::new(1, end),
            },
            kind,
            via: via.clone(),
        });
        let is_class = file_kind == FileKind::Class;
        let old_file = if is_class {
            None
        } else {
            let old = self.ds.raw("FILE");
            self.set_file(&abs.to_string_lossy());
            Some(old)
        };
        let classname = if is_class {
            abs.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        self.stack.push(abs.to_path_buf());
        for st in &stmts {
            self.statement(st, &via, abs, &classname)?;
        }
        self.stack.pop();
        if restore
            && let Some(Some(old)) = old_file
            && old != abs.to_string_lossy()
        {
            self.set_file(&old);
        }
        Ok(())
    }

    fn set_file(&mut self, file: &str) {
        let src = self.content_src("FILE");
        put_keep(&mut self.ds, "FILE", file, src);
        self.refresh_computed();
    }

    fn content_src(&self, name: &str) -> Vec<u64> {
        self.ds
            .vars
            .get(name)
            .and_then(|v| v.content.as_ref())
            .map(|c| c.src.clone())
            .unwrap_or_default()
    }

    /// Recomputes the values OE-Core derives from `FILE` with python (module docs).
    fn refresh_computed(&mut self) {
        if !self.recipe_pass {
            return;
        }
        let file = self.ds.raw("FILE").unwrap_or_default();
        let (pn, pv) = vars_from_file(&file);
        for &name in COMPUTED {
            let var = self.ds.vars.get(name);
            let content = var.and_then(|v| v.content.clone());
            let absent = var.is_none_or(|v| v.content.is_none() && v.default.is_none());
            let ours = self.ds.notes.computed.contains(name);
            let python = content.as_ref().is_some_and(|c| has_python(&c.text));
            if !(absent || ours || python) {
                continue;
            }
            if name == "BP" && !(absent || ours) {
                // OE-Core's BP is plain text; python here is somebody else's.
                continue;
            }
            let text = match name {
                "PN" => pn.clone().unwrap_or_else(|| "defaultpkgname".into()),
                "PV" => pv.clone().unwrap_or_else(|| "1.0".into()),
                "BPN" => self.prune_suffix(),
                "BP" => "${BPN}-${PV}".to_string(),
                _ => Path::new(&file)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
            };
            let src = content.map(|c| c.src).unwrap_or_default();
            put_keep(&mut self.ds, name, &text, src);
            Arc::make_mut(&mut self.ds.notes)
                .computed
                .insert(name.to_string());
        }
    }

    /// `oe.utils.prune_suffix(PN, SPECIAL_PKGSUFFIX, d)`, with the `MLPREFIX` strip.
    fn prune_suffix(&self) -> String {
        let mut pn = self.ds.value("PN").map(|v| v.text).unwrap_or_default();
        let suffixes: Vec<String> = match self.ds.value("SPECIAL_PKGSUFFIX") {
            Some(v) => v.text.split_whitespace().map(str::to_string).collect(),
            None => SPECIAL_PKGSUFFIX.iter().map(|s| s.to_string()).collect(),
        };
        for s in &suffixes {
            if !s.is_empty()
                && let Some(p) = pn.strip_suffix(s.as_str())
            {
                pn = p.to_string();
            }
        }
        if let Some(prefix) = self.ds.value("MLPREFIX").map(|v| v.text)
            && !prefix.is_empty()
            && let Some(p) = pn.strip_prefix(&prefix)
        {
            pn = p.to_string();
        }
        pn
    }

    // ----- statements -----

    fn statement(
        &mut self,
        st: &StatementRow,
        via: &[Loc],
        file: &Path,
        classname: &str,
    ) -> Result<()> {
        match st.kind.as_str() {
            "assign" => self.assign(st, via),
            "export" => {
                if let Some(n) = &st.name {
                    self.ds.mark_exported(n);
                }
            }
            "unset" => {
                if let Some(n) = &st.name {
                    self.tainted(n);
                    self.ds
                        .unset(n, st.flag.as_deref(), st.loc.clone(), via.to_vec());
                    self.after_write(n, &[], st.flag.as_deref(), true);
                }
            }
            "function" => self.function(st, via, file),
            "pydef" => {
                if let Some(n) = &st.name {
                    put_flag(&mut self.ds, n, "func", "1");
                    put_flag(&mut self.ds, n, "python", "1");
                }
            }
            "inherit" => {
                let text = st.name.clone().unwrap_or_default();
                let deferred = st.op.as_deref() == Some("inherit_defer");
                if FileKind::from_path(file) == Some(FileKind::Conf) {
                    // A `.conf` is parsed by ConfHandler, which has no `inherit` /
                    // `inherit_defer` (BitBake 2.18.0 `ConfHandler.feeder`): "unparsed line".
                    // An `.inc` is parsed by BBHandler whoever includes it, so an `inherit`
                    // there is followed, in the class type of the pass (`self.global`).
                    self.issue(
                        Some(&st.loc),
                        via,
                        if deferred { "inherit_defer" } else { "inherit" },
                        &text,
                        RefProblem::Fatal(
                            "unparsed line: a .conf file is parsed by ConfHandler, which has no inherit"
                                .into(),
                        ),
                    );
                } else if deferred && !self.ws.release.release.has(Feature::InheritDefer) {
                    // BitBake before 2.8 has no such directive: the line falls through to
                    // ConfHandler.feeder and fails with "unparsed line" (bitbake-semantics.md,
                    // "Directives"). Recorded, and the class is not inherited.
                    self.issue(
                        Some(&st.loc),
                        via,
                        "inherit_defer",
                        &text,
                        RefProblem::NotInRelease(Feature::InheritDefer),
                    );
                } else if deferred {
                    Arc::make_mut(&mut self.ds.notes).deferred.push(Deferred {
                        text,
                        loc: Some(st.loc.clone()),
                        via: via.to_vec(),
                    });
                } else {
                    self.inherit(&text, Some(&st.loc), via, false, "inherit")?;
                }
            }
            "include" => self.include(st, via, file)?,
            "addfragments" => self.add_fragments(st, via)?,
            "export_functions" => self.export_functions(st, via, classname),
            // The task list only (`bb.build.addtask` / `deltask`); their `task` and `deps`
            // flags are not stored.
            "addtask" => {
                let names = st.name.clone().unwrap_or_default();
                let notes = Arc::make_mut(&mut self.ds.notes);
                for t in names.split_whitespace() {
                    let t = task_name(t);
                    if !notes.tasks.contains(&t) {
                        notes.tasks.push(t);
                    }
                }
            }
            "deltask" => {
                let names = self.ds.expand(st.name.as_deref().unwrap_or("")).text;
                let notes = Arc::make_mut(&mut self.ds.notes);
                for t in names.split_whitespace() {
                    let t = task_name(t);
                    notes.tasks.retain(|x| *x != t);
                }
            }
            // addhandler: handler bookkeeping, not values.
            _ => {}
        }
        Ok(())
    }

    fn assign(&mut self, st: &StatementRow, via: &[Loc]) {
        let (Some(base), Some(op)) = (&st.base, &st.op) else {
            return;
        };
        let Ok(op) = op.parse::<AssignOp>() else {
            return;
        };
        let var = VarRef {
            base: base.clone(),
            overrides: st.overrides.clone(),
            flag: st.flag.clone(),
            legacy_syntax: st.legacy,
        };
        let name = full_name(&var);
        self.tainted(base);
        let exported = is_exported(st);
        let overrides = var.overrides.clone();
        let flag = var.flag.clone();
        let before = self
            .ds
            .vars
            .get(base.as_str())
            .and_then(|v| v.content.clone());
        self.ds.apply(VarOp {
            var,
            op,
            value: st.value.clone().unwrap_or_default(),
            loc: st.loc.clone(),
            via: via.to_vec(),
            kind: OpKind::Statement,
        });
        if exported && !st.legacy {
            self.ds.mark_exported(&name);
        }
        let after = self
            .ds
            .vars
            .get(base.as_str())
            .and_then(|v| v.content.clone());
        self.after_write(base, &overrides, flag.as_deref(), before != after);
    }

    /// Records that the file being read under an uncertain reference changes `base` (a
    /// name with `${...}` is recorded as its expanded base).
    fn tainted(&mut self, base: &str) {
        if self.taint.is_empty() {
            return;
        }
        let b = if base.contains("${") {
            base_of(&self.ds.expand(base).text).to_string()
        } else {
            base_of(base).to_string()
        };
        if let Some(t) = self.taint.last_mut() {
            t.bases.insert(b);
        }
    }

    /// Keeps target pins and computed values in force after a write to `base` (`changed`:
    /// the plain value of `base` is different now).
    fn after_write(&mut self, base: &str, overrides: &[String], flag: Option<&str>, changed: bool) {
        if !overrides.is_empty() || flag.is_some() {
            return;
        }
        if let Some((n, v)) = self.pins.iter().find(|(n, _)| *n == base).cloned() {
            put(&mut self.ds, n, &v);
        }
        if self.recipe_pass && COMPUTED_INPUTS.contains(&base) {
            if changed && COMPUTED.contains(&base) {
                // A plain assignment takes over; python is computed again.
                Arc::make_mut(&mut self.ds.notes).computed.remove(base);
            }
            self.refresh_computed();
        }
    }

    /// `MethodNode.eval`.
    fn function(&mut self, st: &StatementRow, via: &[Loc], file: &Path) {
        let lang = st.op.clone().unwrap_or_default();
        let anonymous = st.name.as_deref() == Some("__anonymous");
        let (var, python) = if anonymous {
            let tr: String = file
                .to_string_lossy()
                .chars()
                .map(|c| if "/.+-@%&~".contains(c) { '_' } else { c })
                .collect();
            let name = format!("__anon_{}_{tr}", st.loc.lines.end);
            (
                VarRef {
                    base: name,
                    overrides: Vec::new(),
                    flag: None,
                    legacy_syntax: false,
                },
                true,
            )
        } else {
            let Some(base) = &st.base else { return };
            (
                VarRef {
                    base: base.clone(),
                    overrides: st.overrides.clone(),
                    flag: None,
                    legacy_syntax: st.legacy,
                },
                lang.contains("python"),
            )
        };
        let key = full_name(&var);
        if !var.legacy_syntax && self.ds.raw(&key).is_some() {
            let v = self.ds.var_mut(&key);
            v.flags.remove("python");
            v.flags.remove("fakeroot");
        }
        let legacy = var.legacy_syntax;
        self.ds.apply(VarOp {
            var,
            op: AssignOp::Set,
            value: format!("{}\n", st.loc),
            loc: st.loc.clone(),
            via: via.to_vec(),
            kind: OpKind::Statement,
        });
        if !legacy {
            if python {
                put_flag(&mut self.ds, &key, "python", "1");
            }
            if lang.starts_with("fakeroot") {
                put_flag(&mut self.ds, &key, "fakeroot", "1");
            }
            put_flag(&mut self.ds, &key, "func", "1");
        }
        {
            let notes = Arc::make_mut(&mut self.ds.notes);
            if let Some((target, _, _)) = split_keyword(&key) {
                notes.functions.insert(target);
            }
            notes.functions.insert(key.clone());
        }
        if anonymous {
            self.anonymous(st, via, key);
        }
    }

    /// Records an anonymous python function and the variables its body changes by name.
    fn anonymous(&mut self, st: &StatementRow, via: &[Loc], name: String) {
        let body = self.body_text(&st.loc);
        let (sets, opaque) = match &body {
            Some(b) => python_writes(b),
            None => (Vec::new(), true),
        };
        let mut bases = Vec::new();
        for s in &sets {
            let e = self.ds.expand(s);
            bases.push(base_of(&e.text).to_string());
        }
        // `ClassExtender.rename_package_variables(PACKAGEVARS...)` renames `<VAR>:<pkg>` for
        // each variable the caller lists, read from `PACKAGEVARS` when the function runs.
        if body
            .as_deref()
            .is_some_and(|b| b.contains(".rename_package_variables("))
            && let Some(v) = self.ds.value("PACKAGEVARS")
        {
            bases.extend(pwords::known_words(&v.text).into_iter().map(str::to_string));
        }
        let notes = Arc::make_mut(&mut self.ds.notes);
        notes.python_vars.extend(bases);
        notes.anonymous.push(AnonPython {
            loc: st.loc.clone(),
            via: via.to_vec(),
            name,
            sets,
            opaque,
        });
        self.ds.ovr_cache = OnceLock::new();
    }

    /// The text of the lines `loc` covers, read from disk (the index has no bodies).
    fn body_text(&self, loc: &Loc) -> Option<String> {
        if let Some(text) = self.overlay.and_then(|o| o.lines(self.ws, loc)) {
            return Some(text);
        }
        let abs = self.ws.abs_path(loc)?;
        let abs = self.ws.confine(&abs).ok()?;
        let text = fs::read_to_string(abs).ok()?;
        let lines: Vec<&str> = text
            .lines()
            .skip(loc.lines.start.saturating_sub(1) as usize)
            .take((loc.lines.end + 1).saturating_sub(loc.lines.start) as usize)
            .collect();
        Some(lines.join("\n"))
    }

    /// `ExportFuncsNode.eval`.
    fn export_functions(&mut self, st: &StatementRow, via: &[Loc], classname: &str) {
        const MARK: &str = " -> ";
        let names = st.name.clone().unwrap_or_default();
        for func in names.split_whitespace() {
            let called = format!("{classname}_{func}");
            let current = self.ds.raw(func);
            if current.as_ref().is_some_and(|b| !b.contains(MARK)) {
                continue;
            }
            if current.is_some() {
                let v = self.ds.var_mut(func);
                v.flags.remove("python");
                v.flags.remove("func");
            }
            for flag in ["func", "python"] {
                if let Some(f) = self.ds.raw_flag(&called, flag) {
                    put_flag(&mut self.ds, func, flag, &f);
                }
            }
            for flag in ["dirs", "cleandirs", "fakeroot"] {
                if let Some(f) = self.ds.raw_flag(func, flag) {
                    put_flag(&mut self.ds, &called, flag, &f);
                }
            }
            self.ds.apply(VarOp {
                var: VarRef {
                    base: func.to_string(),
                    overrides: Vec::new(),
                    flag: None,
                    legacy_syntax: false,
                },
                op: AssignOp::Set,
                value: format!("{}{MARK}{called}\n", st.loc),
                loc: st.loc.clone(),
                via: via.to_vec(),
                kind: OpKind::Statement,
            });
        }
    }

    // ----- references -----

    fn issue(
        &mut self,
        loc: Option<&Loc>,
        via: &[Loc],
        kind: &str,
        text: &str,
        problem: RefProblem,
    ) {
        Arc::make_mut(&mut self.ds.notes).references.push(RefIssue {
            loc: loc.cloned(),
            via: via.to_vec(),
            kind: kind.to_string(),
            text: text.to_string(),
            problem,
        });
    }

    /// Expands a reference argument the way BitBake does when the statement runs
    /// (`d.expand(...)`: for `inherit_defer`, at the end of the recipe, `ast.finalize`).
    /// `None` (and an issue) if the expanded text still holds `${@...}` or an undefined
    /// `${VAR}`: the file to read is not known. When the text is complete but the value is
    /// not certain (it carries `unevaluated_python` or unresolved names because an override
    /// decision depends on unknown `OVERRIDES` entries, or a variable python may change), the
    /// text is returned with the flags, and the caller follows it under a [`Taint`].
    fn expand_ref(
        &mut self,
        text: &str,
        loc: Option<&Loc>,
        via: &[Loc],
        kind: &str,
    ) -> Option<(String, Option<Taint>)> {
        let e = self.ds.expand(text);
        if has_python(&e.text) {
            self.issue(loc, via, kind, text, RefProblem::Python);
            return None;
        }
        if e.text.contains("${") && !e.unresolved.is_empty() {
            self.issue(loc, via, kind, text, RefProblem::Undefined(e.unresolved));
            return None;
        }
        let taint = (e.unevaluated_python || !e.unresolved.is_empty()).then(|| Taint {
            loc: loc.cloned(),
            via: via.to_vec(),
            kind: kind.to_string(),
            text: text.to_string(),
            expanded: e.text.clone(),
            python: e.unevaluated_python,
            unresolved: e.unresolved.clone(),
            bases: BTreeSet::new(),
        });
        Some((e.text, taint))
    }

    /// Runs `f` (which reads the files of one uncertain reference) with `taint` on the
    /// stack, then records the reference and flags every variable those files changed.
    fn under_taint(
        &mut self,
        taint: Option<Taint>,
        f: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        let Some(t) = taint else {
            return f(self);
        };
        self.taint.push(t);
        let r = f(self);
        let Some(t) = self.taint.pop() else {
            return r;
        };
        // A name the files set is no longer undefined (`TCOVERRIDE`, set by the
        // `toolchain/gcc` class that `${TOOLCHAIN}`'s uncertainty was partly about).
        let unresolved: Vec<String> = t
            .unresolved
            .iter()
            .filter(|u| self.ds.raw(u).is_none())
            .cloned()
            .collect();
        if !t.python && unresolved.is_empty() {
            // Only names the files themselves set stood in the way: the value was certain
            // after all (BitBake expands the argument before the file is read, when those
            // names are unset there too).
            if let Some(outer) = self.taint.last_mut() {
                outer.bases.extend(t.bases);
            }
            return r;
        }
        let notes = Arc::make_mut(&mut self.ds.notes);
        if t.python {
            notes.uncertain_vars.extend(t.bases.iter().cloned());
        }
        if !unresolved.is_empty() {
            for b in &t.bases {
                let e = notes.unresolved_vars.entry(b.clone()).or_default();
                for u in &unresolved {
                    if !e.contains(u) {
                        e.push(u.clone());
                    }
                }
            }
        }
        notes.uncertain_references.push(UncertainRef {
            loc: t.loc,
            via: t.via,
            kind: t.kind,
            text: t.text,
            expanded: t.expanded,
            python: t.python,
            unresolved,
        });
        // An outer uncertain reference covers what this one changed too.
        if let Some(outer) = self.taint.last_mut() {
            outer.bases.extend(t.bases);
        }
        self.ds.ovr_cache = OnceLock::new();
        r
    }

    /// `BBPATH` as the store has it now (absolute entries, canonical when they exist);
    /// the workspace's configured `BBPATH` if it is empty or cannot be expanded.
    fn bbpath(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        if let Some(v) = self.ds.value("BBPATH")
            && !v.unevaluated_python
        {
            for e in v
                .text
                .split(':')
                .filter(|e| !e.is_empty() && !e.contains("${"))
            {
                let p = normalize(Path::new(e));
                if !p.is_absolute() {
                    continue;
                }
                let p = fs::canonicalize(&p).unwrap_or(p);
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
        if out.is_empty() {
            out = resolve::bbpath(self.ws);
        }
        out
    }

    fn resolver(&mut self) -> &Resolver {
        let bbpath = self.bbpath();
        if self.resolver.as_ref().is_some_and(|(b, _)| *b != bbpath) {
            self.resolver = None;
        }
        let files = self.files;
        let overlay = self.overlay;
        &self
            .resolver
            .get_or_insert_with(|| {
                let mut known = files.known.clone();
                if let Some(o) = overlay {
                    known.extend(o.paths().map(Path::to_path_buf));
                }
                let r = Resolver::with_bbpath(bbpath.clone()).with_known_files(known);
                (bbpath, r)
            })
            .1
    }

    /// `include` / `require` / `include_all` (`ConfHandler.include`).
    fn include(&mut self, st: &StatementRow, via: &[Loc], from: &Path) -> Result<()> {
        let mode = st.op.clone().unwrap_or_else(|| "include".into());
        let text = st.name.clone().unwrap_or_default();
        let Some((expanded, taint)) = self.expand_ref(&text, Some(&st.loc), via, &mode) else {
            return Ok(());
        };
        self.under_taint(taint, |r| {
            r.include_words(st, via, from, &mode, &text, &expanded)
        })
    }

    fn include_words(
        &mut self,
        st: &StatementRow,
        via: &[Loc],
        from: &Path,
        mode: &str,
        text: &str,
        expanded: &str,
    ) -> Result<()> {
        let (mode, text) = (mode.to_string(), text.to_string());
        let words: Vec<String> = if mode == "include_all" {
            let w = expanded.trim();
            if w.is_empty() {
                Vec::new()
            } else {
                vec![w.to_string()]
            }
        } else {
            expanded.split_whitespace().map(str::to_string).collect()
        };
        let mut child_via = via.to_vec();
        child_via.push(st.loc.clone());
        for word in words {
            if word == "conf/local.conf" && !self.kas_local.is_empty() {
                // kas's build directory, which is first on BBPATH, would hold the
                // generated local.conf; it does not exist yet.
                self.apply_kas_local();
                continue;
            }
            let ws = self.ws;
            let targets: Vec<PathBuf> = if mode == "include_all" {
                self.resolver().include_all(ws, &word)
            } else {
                self.resolver()
                    .find_in_bbpath(ws, &word, from)
                    .into_iter()
                    .collect()
            };
            self.record_probed(&mode, &word, from, &targets);
            if targets.is_empty() && mode == "require" {
                self.issue(
                    Some(&st.loc),
                    via,
                    &mode,
                    &text,
                    RefProblem::NotFound(word.clone()),
                );
            }
            for t in targets {
                if self.cycle(&t, &st.loc, via, &mode, &text) {
                    continue;
                }
                let kind = FileKind::from_path(&t).unwrap_or(FileKind::Include);
                self.visit(&t, kind, child_via.clone(), true)?;
            }
        }
        Ok(())
    }

    /// Records the candidate paths an include of `word` looked at and found empty, in the
    /// resolver's order (`Resolver::find_in_bbpath`: absolute as is, else the including
    /// file's directory, then each `BBPATH` entry; `include_all`: every `BBPATH` entry),
    /// up to the first one found. A file created at one of them would have been read.
    fn record_probed(&mut self, mode: &str, word: &str, from: &Path, found: &[PathBuf]) {
        if word.is_empty() {
            return;
        }
        let p = Path::new(word);
        let bbpath = self.bbpath();
        let cands: Vec<PathBuf> = if p.is_absolute() {
            vec![normalize(p)]
        } else if mode == "include_all" {
            bbpath.iter().map(|e| normalize(&e.join(p))).collect()
        } else {
            from.parent()
                .map(Path::to_path_buf)
                .into_iter()
                .chain(bbpath)
                .filter(|d| d.is_absolute())
                .map(|d| normalize(&d.join(p)))
                .collect()
        };
        let notes = Arc::make_mut(&mut self.ds.notes);
        for c in cands {
            if found.contains(&c) {
                if mode != "include_all" {
                    break;
                }
                continue;
            }
            notes.probed_missing.insert(c);
        }
    }

    /// `AddFragmentsNode.eval` (BitBake 2.18 `lib/bb/parse/ast.py`; 2.12 without the
    /// built-in fragments). See docs/bitbake-semantics.md, "Configuration fragments".
    fn add_fragments(&mut self, st: &StatementRow, via: &[Loc]) -> Result<()> {
        const KIND: &str = "addfragments";
        let line = st.value.clone().unwrap_or_default();
        let Some(af) = crate::parse::ast::add_fragments_line(&line) else {
            return Ok(());
        };
        let release = self.ws.release.release;
        if !release.has(Feature::AddFragments) {
            // Before BitBake 2.12 the line is an unparsed line: the file fails to parse.
            self.issue(
                Some(&st.loc),
                via,
                KIND,
                &line,
                RefProblem::NotInRelease(Feature::AddFragments),
            );
            return Ok(());
        }
        let builtin_form = release.has(Feature::BuiltinFragments);
        let Some(groups) = af.groups(if builtin_form { 4 } else { 3 }) else {
            self.issue(
                Some(&st.loc),
                via,
                KIND,
                &line,
                RefProblem::Fatal(format!(
                    "unparsed line: `addfragments` takes {} arguments on {release}",
                    if builtin_form { 4 } else { 3 }
                )),
            );
            return Ok(());
        };
        let (prefix, frag_var, flagged_var) = (&groups[0], &groups[1], &groups[2]);
        let builtin_var = groups.get(3);
        if builtin_form
            && self
                .ds
                .value("_BB_SKIP_FRAGMENTS")
                .is_some_and(|v| v.text == "1")
        {
            return Ok(());
        }

        // BitBake reads the lists before it looks at the fragments; an unset list variable
        // is a python exception (`None.split()`).
        let list = |r: &mut Self, var: &str| -> Option<Vec<String>> {
            match r.ds.value(var) {
                Some(v) => Some(v.text.split_whitespace().map(str::to_string).collect()),
                None => {
                    r.issue(
                        Some(&st.loc),
                        via,
                        KIND,
                        &line,
                        RefProblem::Fatal(format!("{var} is not set (`None.split()`)")),
                    );
                    None
                }
            }
        };
        let Some(flagged) = list(self, flagged_var) else {
            return Ok(());
        };
        let mut builtin: Vec<(String, String)> = Vec::new();
        if let Some(bv) = builtin_var {
            let Some(words) = list(self, bv) else {
                return Ok(());
            };
            for w in words {
                // `{f[0]: f[1] for f in [f.split(':') ...]}`: a word without `:` raises.
                let parts: Vec<&str> = w.split(':').collect();
                if parts.len() < 2 {
                    self.issue(
                        Some(&st.loc),
                        via,
                        KIND,
                        &line,
                        RefProblem::Fatal(format!("{bv} word {w:?} has no `:` (IndexError)")),
                    );
                    return Ok(());
                }
                let (k, v) = (parts[0].to_string(), parts[1].to_string());
                match builtin.iter_mut().find(|(bk, _)| *bk == k) {
                    Some(e) => e.1 = v,
                    None => builtin.push((k, v)),
                }
            }
        }

        let Some(fragments) = self.ds.value(frag_var) else {
            return Ok(());
        };
        if fragments.unevaluated_python || !fragments.unresolved.is_empty() {
            let problem = if fragments.unevaluated_python {
                RefProblem::Python
            } else {
                RefProblem::Undefined(fragments.unresolved.clone())
            };
            self.issue(Some(&st.loc), via, KIND, frag_var, problem);
            // What the built-in fragments would set is not known.
            let notes = Arc::make_mut(&mut self.ds.notes);
            for (_, var) in &builtin {
                let e = notes.unresolved_vars.entry(var.clone()).or_default();
                if !e.contains(frag_var) {
                    e.push(frag_var.clone());
                }
            }
            self.ds.ovr_cache = OnceLock::new();
            return Ok(());
        }
        let words: Vec<String> = fragments
            .text
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let mut child_via = via.to_vec();
        child_via.push(st.loc.clone());
        for f in words {
            let Some((layerid, name)) = f.split_once('/') else {
                self.issue(
                    Some(&st.loc),
                    via,
                    KIND,
                    &f,
                    RefProblem::Fatal(format!(
                        "fragment {f:?} has no `/` (ValueError in `f.split('/', 1)`)"
                    )),
                );
                continue;
            };
            if let Some((_, var)) = builtin.iter().find(|(k, _)| k == layerid) {
                self.builtin_fragment(st, via, frag_var, &f, var, name);
                continue;
            }
            let Some((rel, taint)) =
                self.expand_ref(&format!("{prefix}/{name}.conf"), Some(&st.loc), via, KIND)
            else {
                continue;
            };
            match self.find_fragment(layerid, &rel) {
                Some(path) => {
                    if self.cycle(&path, &st.loc, via, KIND, &f) {
                        continue;
                    }
                    let cv = child_via.clone();
                    self.under_taint(taint, |r| r.visit(&path, FileKind::Conf, cv, true))?;
                    for v in &flagged {
                        // setVarFlag(v, f, getVar(v)); setVar(v, None)
                        if let Some(val) = self.ds.value(v) {
                            put_flag(&mut self.ds, v, &f, &val.text);
                        }
                        if let Some(var) = Arc::make_mut(&mut self.ds.vars).get_mut(v.as_str()) {
                            let var = Arc::make_mut(var);
                            var.content = None;
                            var.default = None;
                        }
                        self.ds.ovr_cache = OnceLock::new();
                    }
                }
                None => self.issue(
                    Some(&st.loc),
                    via,
                    KIND,
                    &f,
                    RefProblem::NotFound(format!("fragment {f} ({rel} in layer {layerid})")),
                ),
            }
        }
        Ok(())
    }

    /// `check_and_set_builtin_fragment`: `<prefix>/<value>` sets the variable the prefix
    /// names, as a plain assignment made where the fragments variable got the word.
    fn builtin_fragment(
        &mut self,
        st: &StatementRow,
        via: &[Loc],
        frag_var: &str,
        fragment: &str,
        var: &str,
        value: &str,
    ) {
        let pinned = self.pins.iter().any(|(n, _)| *n == var);
        // getVar(var, noweakdefault=True) != None: a value other than a `??=` default.
        let assigned = self.ds.vars.get(var).is_some_and(|v| {
            v.content.is_some() || !v.appends.is_empty() || !v.prepends.is_empty()
        });
        if assigned && !pinned {
            self.issue(
                Some(&st.loc),
                via,
                "addfragments",
                fragment,
                RefProblem::Fatal(format!(
                    "a builtin fragment '{fragment}' is used while {var} has already got an assignment"
                )),
            );
            return;
        }
        // The history entry names the last `=` / `+=` of the fragments variable that holds
        // the word (BitBake: `fh['op'] in ("set", "append") and fragment in fh["detail"]`).
        let origin = self
            .ds
            .history(frag_var)
            .into_iter()
            .map(|(op, _)| op)
            .rfind(|op| {
                op.var.overrides.is_empty()
                    && op.var.flag.is_none()
                    && matches!(op.op, AssignOp::Set | AssignOp::Append)
                    && op.value.contains(fragment)
            });
        let (loc, mut op_via) = match origin {
            Some(op) => (op.loc, op.via),
            None => (st.loc.clone(), via.to_vec()),
        };
        op_via.push(st.loc.clone());
        self.ds.apply(VarOp {
            var: VarRef {
                base: var.to_string(),
                overrides: Vec::new(),
                flag: None,
                legacy_syntax: false,
            },
            op: AssignOp::Set,
            value: value.to_string(),
            loc,
            via: op_via,
            kind: OpKind::Statement,
        });
        self.after_write(var, &[], None, true);
    }

    /// `find_fragment`: the first `BBLAYERS` entry holding `<entry>/<rel>` whose collection
    /// (`bb.utils.get_file_layer`) is `layerid`.
    fn find_fragment(&self, layerid: &str, rel: &str) -> Option<PathBuf> {
        for layer in &self.ws.layers {
            let cand = normalize(&layer.root.join(rel));
            let exists = self.exists(&cand) || cand.is_file();
            if exists && self.file_collection(&cand, &layer.collections).as_deref() == Some(layerid)
            {
                return Some(cand);
            }
        }
        None
    }

    /// `get_file_layer` for a file that no `BBFILES` entry matches (a `.conf`): the
    /// collection with the longest `BBFILE_PATTERN` that matches the path (`re.match`).
    /// Falls back to the first of `own` (the collections of the layer the file is in) when
    /// no pattern can be evaluated.
    fn file_collection(&self, path: &Path, own: &[String]) -> Option<String> {
        let text = path.to_string_lossy();
        let mut best: Option<(usize, String)> = None;
        let mut any = false;
        let mut seen = HashSet::new();
        for l in &self.ws.layers {
            for c in &l.collections {
                if !seen.insert(c.clone()) {
                    continue;
                }
                let Some(v) = self.ds.value(&format!("BBFILE_PATTERN_{c}")) else {
                    continue;
                };
                if v.unevaluated_python || !v.unresolved.is_empty() {
                    continue;
                }
                any = true;
                let longer = best.as_ref().is_none_or(|(n, _)| v.text.len() > *n);
                if longer
                    && let Ok(re) = Regex::new(&format!("^(?:{})", v.text))
                    && re.is_match(&text)
                {
                    best = Some((v.text.len(), c.clone()));
                }
            }
        }
        match best {
            Some((_, c)) => Some(c),
            None if !any => own.first().cloned(),
            None => None,
        }
    }

    /// True (and an issue) if `t` is already being parsed.
    fn cycle(&mut self, t: &Path, loc: &Loc, via: &[Loc], kind: &str, text: &str) -> bool {
        if !self.stack.iter().any(|s| s == t) {
            return false;
        }
        let at = self
            .files
            .by_abs
            .get(t)
            .map(|f| Loc {
                layer: f.layer.clone(),
                path: f.path.clone(),
                lines: LineSpan::line(1),
            })
            .unwrap_or_else(|| loc.clone());
        self.issue(Some(loc), via, kind, text, RefProblem::Cycle(at));
        true
    }

    /// `BBHandler.inherit`: each word of the expanded text; words in `BB_DEFER_BBCLASSES`
    /// wait for the end of the recipe unless `deferred`; a class already in
    /// `__inherit_cache` is skipped.
    fn inherit(
        &mut self,
        text: &str,
        loc: Option<&Loc>,
        via: &[Loc],
        deferred: bool,
        kind: &str,
    ) -> Result<()> {
        let Some((expanded, taint)) = self.expand_ref(text, loc, via, kind) else {
            return Ok(());
        };
        self.under_taint(taint, |r| {
            r.inherit_words(text, loc, via, deferred, kind, &expanded)
        })
    }

    fn inherit_words(
        &mut self,
        text: &str,
        loc: Option<&Loc>,
        via: &[Loc],
        deferred: bool,
        kind: &str,
        expanded: &str,
    ) -> Result<()> {
        let mut child_via = via.to_vec();
        if let Some(l) = loc {
            child_via.push(l.clone());
        }
        for word in expanded.split_whitespace() {
            let defer = self
                .ds
                .value("BB_DEFER_BBCLASSES")
                .map(|v| pwords::words(&v.text).contains(&word))
                .unwrap_or(false);
            if !deferred && defer {
                Arc::make_mut(&mut self.ds.notes).deferred.push(Deferred {
                    text: word.to_string(),
                    loc: loc.cloned(),
                    via: via.to_vec(),
                });
                continue;
            }
            let ws = self.ws;
            let global = self.global;
            let Some(f) = self.resolver().find_class(ws, word, global) else {
                self.issue(loc, via, kind, text, RefProblem::NotFound(word.to_string()));
                continue;
            };
            if self.ds.notes.inherit_cache.contains(&f) {
                continue;
            }
            Arc::make_mut(&mut self.ds.notes)
                .inherit_cache
                .push(f.clone());
            self.visit(&f, FileKind::Class, child_via.clone(), true)?;
        }
        Ok(())
    }

    /// The `via` chain for something a variable's value pulled in (`BBLAYERS` for the layer
    /// confs, `INHERIT` for a global class): the chain of the first applied operation whose
    /// value contains `word` (any applied operation when `word` is `None`, the last one),
    /// plus that operation.
    fn via_of_word(&self, var: &str, word: Option<&str>) -> Vec<Loc> {
        let mut applied = self
            .ds
            .history(var)
            .into_iter()
            .filter(|(_, s)| *s == OpStatus::Applied)
            .map(|(op, _)| op);
        let op = match word {
            None => applied.next_back(),
            Some(w) => applied
                .into_iter()
                .find(|op| pwords::words(&self.ds.expand(&op.value).text).contains(&w)),
        };
        match op {
            Some(op) => {
                let mut v = op.via.clone();
                v.push(op.loc.clone());
                v
            }
            None => Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Finalisation and class-extended variants
// ---------------------------------------------------------------------------------------

/// Variables OE-core's `native_virtclass_handler` (`classes-recipe/native.bbclass`, Wrynose
/// `33a6bd6c`) filters with `d.setVarFilter` (`native_filter`) when `BBCLASSEXTEND` contains
/// `native`; it also renames `<VAR>:<pkg>` package variables of `RDEPENDS` and the others.
const NATIVE_FILTERED: &[&str] = &[
    "DEPENDS",
    "RDEPENDS",
    "RRECOMMENDS",
    "RSUGGESTS",
    "RPROVIDES",
    "RREPLACES",
    "PACKAGES",
    "PACKAGES_DYNAMIC",
    "PROVIDES",
];

/// `oe.classextend.ClassExtender.map_packagevars` (OE-core Wrynose `lib/oe/classextend.py`):
/// a `suffix_filter_deps` filter on each.
const MAP_PACKAGEVARS: &[&str] = &[
    "RDEPENDS",
    "RRECOMMENDS",
    "RSUGGESTS",
    "RPROVIDES",
    "RREPLACES",
    "RCONFLICTS",
    "PKG",
];

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

impl Replay<'_> {
    /// `ast.finalize` up to the task processing: OE-core's `RecipePreDeferredInherits`
    /// handler ([`Feature::DeferredInheritsClassOverride`]), the deferred inherits (first in
    /// first out, more may be added meanwhile), the `RecipePreFinalise` handlers of the
    /// class-extension classes ([`Replay::class_extend_handlers`]), `expandKeys`, the
    /// `RecipePostKeyExpansion` handler of `multilib.bbclass`, (anonymous python, not run),
    /// `handleVirtRecipeProviders`; then the skip notes.
    fn finalize(&mut self, recipe: &Loc) -> Result<()> {
        // OE-core base.bbclass `base_eventhandler` on RecipePreDeferredInherits: the first of
        // native, nativesdk, crosssdk, cross that is among the deferred inherits (as written)
        // sets CLASSOVERRIDE.
        if self
            .ws
            .release
            .release
            .has(Feature::DeferredInheritsClassOverride)
            && self.handles("base_eventhandler", "RecipePreDeferredInherits")
        {
            let inherits: Vec<String> = self
                .ds
                .notes
                .deferred
                .iter()
                .map(|d| d.text.clone())
                .collect();
            if let Some(c) = ["native", "nativesdk", "crosssdk", "cross"]
                .into_iter()
                .find(|c| inherits.iter().any(|i| i == c))
            {
                let loc = self.handler_loc("base_eventhandler", recipe);
                self.ds.rewrite(
                    "CLASSOVERRIDE",
                    format!("class-{c}"),
                    loc,
                    OpKind::EventHandler,
                );
            }
        }
        loop {
            let next = {
                let notes = Arc::make_mut(&mut self.ds.notes);
                if notes.deferred.is_empty() {
                    None
                } else {
                    Some(notes.deferred.remove(0))
                }
            };
            let Some(d) = next else { break };
            self.inherit(&d.text, d.loc.as_ref(), &d.via, true, "inherit_defer")?;
        }
        self.class_extend_handlers(recipe);
        let incomplete = self.ds.expand_keys();
        Arc::make_mut(&mut self.ds.notes).incomplete_keys = incomplete;
        self.multilib_postkeyexp(recipe);
        // (BitBake runs anonymous python here, which the replay does not.)
        if self.ws.release.release.has(Feature::RecipeVirtualProviders) {
            virtual_recipe_providers(&mut self.ds, recipe);
        }
        self.apply_native_provides_filter();
        let mut skip = skip_reasons(&self.ds);
        skip.extend(self.features_check_notes());
        skip.extend(self.virtual_provider_notes());
        if !skip.is_empty() {
            Arc::make_mut(&mut self.ds.notes).target_notes.extend(skip);
        }
        Ok(())
    }

    /// `extendfunc` of `ast.multi_finalize` on the copy of the datastore before
    /// finalisation: `BBEXTENDCURR` / `BBEXTENDVARIANT` for a word with an argument, else
    /// `PN = "<pn>-<word>"`; then the class, deferred ([`Feature::ClassExtendDeferred`]) or
    /// at once.
    fn extend_variant(&mut self, ext: &ClassExtend, pn: &str, recipe: &Loc) -> Result<()> {
        match ext.variant() {
            Some(v) => {
                self.ds.rewrite(
                    "BBEXTENDCURR",
                    ext.class().to_string(),
                    recipe.clone(),
                    OpKind::ClassExtend,
                );
                self.ds.rewrite(
                    "BBEXTENDVARIANT",
                    v.to_string(),
                    recipe.clone(),
                    OpKind::ClassExtend,
                );
            }
            None => self.set_pn(
                format!("{pn}-{}", ext.word),
                recipe.clone(),
                OpKind::ClassExtend,
            ),
        }
        let name = ext
            .variant_name(pn)
            .unwrap_or_else(|| format!("{pn}-{}", ext.word));
        let set = match ext.variant() {
            Some(v) => format!(
                "BBEXTENDCURR = \"{}\", BBEXTENDVARIANT = \"{v}\"",
                ext.class()
            ),
            None => format!("PN = \"{pn}-{}\"", ext.word),
        };
        let mut notes = vec![format!(
            "note: {name} is {} of {pn} (BBCLASSEXTEND `{}`), evaluated as BitBake makes it: the base recipe and its bbappends, then {set}, `inherit {}` and finalisation; FILE is the base recipe; what OE-core's class-extension handlers and anonymous python change is flagged, not guessed",
            ext.describe(),
            ext.word,
            ext.class()
        )];
        let deferred_before = !self.ds.notes.deferred.is_empty();
        if self.ws.release.release.has(Feature::ClassExtendDeferred) {
            Arc::make_mut(&mut self.ds.notes).deferred.push(Deferred {
                text: ext.class().to_string(),
                loc: None,
                via: Vec::new(),
            });
        } else {
            if deferred_before && self.ws.release.release.has(Feature::InheritDefer) {
                notes.push(format!(
                    "note: on this release BitBake inherits `{}` at once, before the recipe's deferred inherits (BitBake before 2.16); the Scarthgap point releases from 5.0.11 (BitBake 2.8 with 139f61fe9) defer it after them, as 2.16 does, which this evaluation does not follow",
                    ext.class()
                ));
            }
            self.inherit(ext.class(), None, &[], false, "BBCLASSEXTEND")?;
        }
        let n = Arc::make_mut(&mut self.ds.notes);
        n.variant = Some(ext.clone());
        n.target_notes.extend(notes);
        Ok(())
    }

    /// A plain `setVar("PN", ...)` from code: the value is no longer computed from `FILE`,
    /// and the values computed from `PN` (`BPN`) follow it.
    fn set_pn(&mut self, pn: String, loc: Loc, kind: OpKind) {
        self.ds.rewrite("PN", pn, loc, kind);
        Arc::make_mut(&mut self.ds.notes).computed.remove("PN");
        self.refresh_computed();
    }

    /// Whether `name` is a function whose `eventmask` flag names `bb.event.<event>`: an
    /// event handler BitBake runs for that event (registered by `addhandler`).
    fn handles(&self, name: &str, event: &str) -> bool {
        self.ds.is_function(name)
            && self.ds.flag(name, "eventmask").is_some_and(|m| {
                m.text
                    .split_whitespace()
                    .any(|w| w == format!("bb.event.{event}"))
            })
    }

    /// Where a handler function is defined (its first piece), else `fallback`.
    fn handler_loc(&self, name: &str, fallback: &Loc) -> Loc {
        self.ds
            .function(name)
            .and_then(|p| p.first().map(|p| p.loc.clone()))
            .unwrap_or_else(|| fallback.clone())
    }

    /// `bb.data.inherits_class`: a class file named `<klass>.bbclass` in `__inherit_cache`.
    fn inherits_class(&self, klass: &str) -> bool {
        let needle = format!("{klass}.bbclass");
        self.ds
            .notes
            .inherit_cache
            .iter()
            .any(|p| p.file_name().is_some_and(|f| f.to_string_lossy() == needle))
    }

    /// `native_virtclass_handler`'s `d.setVarFilter("PROVIDES", "native_filter(val, pn,
    /// bpn)")`: BitBake applies the filter to the expanded value on every `getVar` (BitBake
    /// 2.18.0 `data_smart.getVarFlag`, `bb.filter.apply_filters`, only for a non-empty
    /// value), so the final `PROVIDES`, the one `bb.cache.CoreRecipeInfo` registers for the
    /// variant, is `native_filter(<final expanded value>)`. Computed here when that value is
    /// static and no anonymous python sets `PROVIDES` by name; then recorded as a handler
    /// operation with a note. Otherwise `PROVIDES` is flagged, as before w11.
    fn apply_native_provides_filter(&mut self) {
        const NATIVE: &str = "native_virtclass_handler";
        let Some((pn, bpn, loc)) = self.native_provides.take() else {
            return;
        };
        let v = self.ds.value("PROVIDES").unwrap_or_default();
        let by_python = self.ds.notes.python_vars.contains("PROVIDES");
        let computed = if by_python || v.unevaluated_python || !v.unresolved.is_empty() {
            None
        } else {
            match super::variant::native_filter(&v.text, &pn, bpn.as_deref()) {
                None => return,
                Some(Ok(t)) => Some(t),
                Some(Err(())) => None,
            }
        };
        match computed {
            Some(t) => {
                let note = format!(
                    "note: {NATIVE} (an OE-core event handler at {loc}) filters PROVIDES with native_filter (OE-core lib/oe/classextend.py), which BitBake applies when the value is read: \"{}\" becomes \"{t}\" (computed from the static value)",
                    v.text
                );
                self.ds
                    .rewrite("PROVIDES", t, loc.clone(), OpKind::EventHandler);
                let notes = Arc::make_mut(&mut self.ds.notes);
                notes
                    .var_notes
                    .entry("PROVIDES".into())
                    .or_default()
                    .push(note);
            }
            None => self.handler_flags(NATIVE, &loc, &strings(&["PROVIDES"]), "filters"),
        }
    }

    /// The text of the anonymous python function defined in class `<class>.bbclass`.
    fn class_anonymous_body(&self, class: &str) -> Option<String> {
        let file = format!("{class}.bbclass");
        let anon = self.ds.notes.anonymous.iter().find(|a| {
            a.loc
                .path
                .file_name()
                .is_some_and(|f| f.to_string_lossy() == file)
        })?;
        self.body_text(&anon.loc)
    }

    /// OE-core `features_check.bbclass` (see [`super::skips`]), when the recipe inherits
    /// it: "would skip" when a check fails on known values, "may skip" naming what is not
    /// known, nothing when every check passes. What the class compares and how it reads
    /// `PARSE_ALL_RECIPES` are read from the workspace's own class (its `for kind in [...]`
    /// list; `bb.utils.to_boolean` or a plain string; the `DISTRO` wording).
    fn features_check_notes(&self) -> Vec<String> {
        const TAIL: &str = "(features_check.bbclass; decided from static values)";
        const MAY: &str = "note: BitBake may skip this recipe:";
        if !self.inherits_class("features_check") {
            return Vec::new();
        }
        let Some(body) = self.class_anonymous_body("features_check") else {
            return vec![format!(
                "{MAY} it inherits features_check, whose anonymous python could not be read, so its REQUIRED / CONFLICT / ANY_OF feature checks were not made"
            )];
        };
        static KINDS: LazyLock<Option<Regex>> =
            LazyLock::new(|| Regex::new(r"for\s+kind\s+in\s+\[([^\]]*)\]").ok());
        let kinds: Vec<String> = KINDS
            .as_ref()
            .and_then(|re| re.captures(&body))
            .and_then(|c| c.get(1))
            .map(|m| {
                m.as_str()
                    .split(',')
                    .map(|k| k.trim().trim_matches(['\'', '"']).to_string())
                    .filter(|k| !k.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if kinds.is_empty() {
            return vec![format!(
                "{MAY} it inherits features_check, whose anonymous python is not the one sous-chef knows (no `for kind in [...]` list), so its feature checks were not made"
            )];
        }
        let parse_all = if body.contains("to_boolean(d.getVar('PARSE_ALL_RECIPES'") {
            static_bool(&self.ds, "PARSE_ALL_RECIPES")
        } else {
            static_value(&self.ds, "PARSE_ALL_RECIPES").map(|v| v.is_some_and(|t| !t.is_empty()))
        };
        if parse_all == Some(true) {
            return Vec::new();
        }
        let distro_message = body.contains("which is missing required");
        let mut unknown = Vec::new();
        for kind in &kinds {
            let lists = skips::KindLists {
                any_of: words_of(&self.ds, &format!("ANY_OF_{kind}_FEATURES")),
                required: words_of(&self.ds, &format!("REQUIRED_{kind}_FEATURES")),
                conflict: words_of(&self.ds, &format!("CONFLICT_{kind}_FEATURES")),
                features: words_of(&self.ds, &format!("{kind}_FEATURES")),
                kind_value: self.ds.value(kind).map(|v| v.text).unwrap_or_default(),
            };
            for c in skips::features_kind(kind, &lists, distro_message) {
                match c {
                    skips::Check::Pass => {}
                    skips::Check::Skip(msg) if parse_all == Some(false) => {
                        // Certain; but a check BitBake makes first may raise first, with
                        // its own message.
                        let earlier = if unknown.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "; BitBake may give an earlier reason instead ({})",
                                unknown.join("; ")
                            )
                        };
                        return vec![format!(
                            "note: BitBake would skip this recipe: {msg} {TAIL}{earlier}"
                        )];
                    }
                    skips::Check::Skip(msg) => unknown.push(format!(
                        "features_check: {msg}, unless PARSE_ALL_RECIPES is true (not known statically)"
                    )),
                    skips::Check::Unknown(why) => unknown.push(why),
                }
            }
        }
        unknown.into_iter().map(|u| format!("{MAY} {u}")).collect()
    }

    /// OE-core `base_eventhandler` on `RecipeParsed` (see [`super::skips`]): a recipe that
    /// provides a `virtual/` name whose `PREFERRED_PROVIDER_` (the recipe's own value)
    /// names another `PN` is skipped. Only when the workspace's handler handles
    /// `RecipeParsed` with that code (`"PREFERRED_PROVIDER_" + p` in its body).
    fn virtual_provider_notes(&self) -> Vec<String> {
        const TAIL: &str = "(base_eventhandler on RecipeParsed; decided from static values)";
        const MAY: &str = "note: BitBake may skip this recipe:";
        const H: &str = "base_eventhandler";
        if !self.handles(H, "RecipeParsed") {
            return Vec::new();
        }
        let Some(body) = self
            .ds
            .function(H)
            .and_then(|p| p.first().map(|p| p.loc.clone()))
            .and_then(|loc| self.body_text(&loc))
        else {
            return Vec::new();
        };
        if !body.contains("\"PREFERRED_PROVIDER_\" + p") {
            return Vec::new();
        }
        let mirror = if body.contains("to_boolean(d.getVar('SOURCE_MIRROR_FETCH'") {
            static_bool(&self.ds, "SOURCE_MIRROR_FETCH")
        } else {
            static_value(&self.ds, "SOURCE_MIRROR_FETCH").map(|v| v.is_some_and(|t| !t.is_empty()))
        };
        if mirror == Some(true) {
            return Vec::new();
        }
        let provides = words_of(&self.ds, "PROVIDES");
        let allowed = words_of(&self.ds, "BB_MULTI_PROVIDER_ALLOWED");
        let pn = static_value(&self.ds, "PN").flatten();
        let mut unknown = Vec::new();
        for p in provides.known.iter().filter(|p| p.starts_with("virtual/")) {
            if allowed.known.contains(p) {
                continue;
            }
            let pref_name = format!("PREFERRED_PROVIDER_{p}");
            let pref = match static_value(&self.ds, &pref_name) {
                Some(v) => v,
                None => {
                    unknown.push(format!(
                        "it provides {p}, and {pref_name} is not known statically ({})",
                        unknown_why(&self.ds, &pref_name)
                    ));
                    continue;
                }
            };
            let Some(x) = pref.filter(|x| !x.is_empty()) else {
                continue;
            };
            let Some(pn) = pn.as_deref() else {
                unknown.push(format!(
                    "it provides {p} and {pref_name} = \"{x}\", but its PN is not known statically"
                ));
                continue;
            };
            let Some(msg) = skips::virtual_provider_skip(p, Some(&x), pn) else {
                continue;
            };
            let mut why = Vec::new();
            if !allowed.complete {
                why.push(format!(
                    "BB_MULTI_PROVIDER_ALLOWED is not known statically ({}), and could list {p}",
                    allowed.why.as_deref().unwrap_or("not evaluated")
                ));
            }
            if mirror.is_none() {
                why.push("SOURCE_MIRROR_FETCH is not known statically".into());
            }
            if why.is_empty() {
                return vec![format!(
                    "note: BitBake would skip this recipe: {msg} {TAIL}"
                )];
            }
            unknown.push(format!("{msg}, unless {}", why.join("; ")));
        }
        // PROVIDES words python makes are not seen; say so only when they may be virtual.
        if !provides.complete {
            let raw = self
                .ds
                .value("PROVIDES")
                .map(|v| v.text)
                .unwrap_or_default();
            let maybe_virtual = pwords::words(&raw)
                .iter()
                .any(|w| pwords::is_unevaluated(w) && w.contains("virtual/"))
                || (self.ds.notes.python_vars.contains("PROVIDES")
                    && pwords::known_words(&raw)
                        .iter()
                        .any(|w| w.starts_with("virtual/")));
            if maybe_virtual {
                unknown.push(format!(
                    "PROVIDES is not known statically ({}), and a virtual/ name it provides may have a PREFERRED_PROVIDER_ naming another recipe",
                    provides.why.as_deref().unwrap_or("not evaluated")
                ));
            }
        }
        unknown
            .into_iter()
            .map(|u| format!("{MAY} {u} (base_eventhandler on RecipeParsed)"))
            .collect()
    }

    /// Flags `vars` as changed by event handler `handler` (not run): their values carry
    /// `unevaluated_python`, and each gets a note.
    fn handler_flags(&mut self, handler: &str, loc: &Loc, vars: &[String], how: &str) {
        let notes = Arc::make_mut(&mut self.ds.notes);
        for v in vars {
            notes.python_vars.insert(v.clone());
            let line = format!(
                "note: {handler} (an OE-core event handler at {loc}, run before finalisation) {how} {v}; it is not run, so the value shown is the one before it"
            );
            let e = notes.var_notes.entry(v.clone()).or_default();
            if !e.contains(&line) {
                e.push(line);
            }
        }
        self.ds.ovr_cache = OnceLock::new();
    }

    /// The `RecipePreFinalise` handlers of OE-core's class-extension classes (Wrynose
    /// `33a6bd6c`), when the class defines them: `native_virtclass_handler`
    /// (`classes-recipe/native.bbclass`), `nativesdk_virtclass_handler`
    /// (`classes-recipe/nativesdk.bbclass`), `multilib_virtclass_handler`
    /// (`classes/multilib.bbclass`). What is plain code on known values is reproduced (the
    /// `PN` and `MLPREFIX` renames, the `OVERRIDES` additions); filters
    /// (`d.setVarFilter`, `oe.utils.set_class_filter`) and renames of package variables
    /// are flagged.
    fn class_extend_handlers(&mut self, recipe: &Loc) {
        // native.bbclass: PN must end with -native; DISTRO_FEATURES goes through
        // set_class_filter; with "native" in BBCLASSEXTEND (a substring test) the filters.
        const NATIVE: &str = "native_virtclass_handler";
        if self.handles(NATIVE, "RecipePreFinalise") {
            let loc = self.handler_loc(NATIVE, recipe);
            match static_value(&self.ds, "PN") {
                Some(Some(pn)) if !pn.ends_with("-native") => {}
                pn => {
                    let mut vars = strings(&["DISTRO_FEATURES"]);
                    let ce = self.ds.value("BBCLASSEXTEND").unwrap_or_default();
                    let unsure = ce.unevaluated_python || !ce.unresolved.is_empty();
                    if ce.text.contains("native") || unsure || pn.is_none() {
                        // The PROVIDES filter is computed at the end of finalisation
                        // ([`Replay::apply_native_provides_filter`]) when PN is known and
                        // the filter certainly applies; otherwise flagged with the rest.
                        let certain = !unsure && ce.text.contains("native");
                        match (&pn, certain) {
                            (Some(Some(p)), true) => {
                                let bpn = static_value(&self.ds, "BPN").flatten();
                                vars.extend(
                                    NATIVE_FILTERED
                                        .iter()
                                        .filter(|v| **v != "PROVIDES")
                                        .map(|v| v.to_string()),
                                );
                                self.native_provides = Some((p.clone(), bpn, loc.clone()));
                            }
                            _ => vars.extend(strings(NATIVE_FILTERED)),
                        }
                    }
                    self.handler_flags(NATIVE, &loc, &vars, "filters");
                }
            }
        }
        const NATIVESDK: &str = "nativesdk_virtclass_handler";
        if self.handles(NATIVESDK, "RecipePreFinalise") {
            let loc = self.handler_loc(NATIVESDK, recipe);
            match static_value(&self.ds, "PN") {
                Some(Some(pn)) if pn.ends_with("-nativesdk") || pn.starts_with("nativesdk-") => {
                    self.handler_flags(NATIVESDK, &loc, &strings(&["DISTRO_FEATURES"]), "filters");
                    self.ds.rewrite(
                        "MLPREFIX",
                        "nativesdk-".into(),
                        loc.clone(),
                        OpKind::EventHandler,
                    );
                    let new = format!(
                        "nativesdk-{}",
                        pn.replace("-nativesdk", "").replace("nativesdk-", "")
                    );
                    self.set_pn(new, loc, OpKind::EventHandler);
                }
                Some(_) => {}
                None => self.handler_flags(
                    NATIVESDK,
                    &loc,
                    &strings(&["DISTRO_FEATURES", "MLPREFIX", "PN"]),
                    "may set",
                ),
            }
        }
        self.multilib_handler(recipe);
    }

    /// `BBEXTENDCURR` and `BBEXTENDVARIANT` when they are `multilib` and a variant name:
    /// `Ok(Some(variant))`; `Ok(None)` when they are known and not; `Err(())` when not known.
    fn multilib_variant(&self) -> std::result::Result<Option<String>, ()> {
        let cls = static_value(&self.ds, "BBEXTENDCURR").ok_or(())?;
        let var = static_value(&self.ds, "BBEXTENDVARIANT").ok_or(())?;
        Ok(match (cls, var) {
            (Some(c), Some(v)) if c == "multilib" && !v.is_empty() => Some(v),
            _ => None,
        })
    }

    /// `OVERRIDES = OVERRIDES (raw) + ":virtclass-multilib-<variant>"`.
    fn add_override(&mut self, name: &str, loc: &Loc) {
        let ov = self.ds.raw("OVERRIDES").unwrap_or_default();
        self.ds.rewrite(
            "OVERRIDES",
            format!("{ov}:{name}"),
            loc.clone(),
            OpKind::EventHandler,
        );
    }

    /// `MLPREFIX = "<variant>-"` and `PN = "<variant>-" + PN (raw)`.
    fn multilib_rename(&mut self, variant: &str, raw_pn: &str, loc: &Loc) {
        self.ds.rewrite(
            "MLPREFIX",
            format!("{variant}-"),
            loc.clone(),
            OpKind::EventHandler,
        );
        self.set_pn(
            format!("{variant}-{raw_pn}"),
            loc.clone(),
            OpKind::EventHandler,
        );
    }

    /// `multilib_virtclass_handler` (OE-core Wrynose `classes/multilib.bbclass`), on
    /// `RecipePreFinalise`.
    fn multilib_handler(&mut self, recipe: &Loc) {
        const H: &str = "multilib_virtclass_handler";
        if !self.handles(H, "RecipePreFinalise") {
            return;
        }
        let loc = self.handler_loc(H, recipe);
        let variant = match self.multilib_variant() {
            Ok(Some(v)) => v,
            Ok(None) => return,
            Err(()) => {
                self.handler_flags(
                    H,
                    &loc,
                    &strings(&["PN", "MLPREFIX", "OVERRIDES"]),
                    "may set (BBEXTENDCURR or BBEXTENDVARIANT is not known statically)",
                );
                return;
            }
        };
        // Values copied from others that change later, or computed by python.
        let mut flagged = strings(&[
            "STAGING_KERNEL_DIR",
            "SSTATE_ARCHS_TUNEPKG",
            "ALL_MULTILIB_PACKAGE_ARCHS",
        ]);
        if let Some(v) = self.ds.value("MULTILIB_SAVE_VARNAME") {
            flagged.extend(
                pwords::known_words(&v.text)
                    .into_iter()
                    .map(|n| format!("{n}_MULTILIB_ORIGINAL")),
            );
        }
        self.handler_flags(H, &loc, &flagged, "sets");

        // The skips, in the handler's order.
        let mut skip: Option<String> = None;
        let mut unsure: Vec<String> = Vec::new();
        let bpn = self.ds.value("BPN").map(|v| v.text).unwrap_or_default();
        let provides = self.ds.value("PROVIDES").unwrap_or_default();
        let non_ml = static_value(&self.ds, "NON_MULTILIB_RECIPES");
        if pwords::known_words(&provides.text).contains(&"virtual/kernel")
            || self.inherits_class("module-base")
            || self.inherits_class("kernel-fit-image")
            || matches!(&non_ml, Some(Some(n)) if pwords::words(n).contains(&bpn.as_str()))
        {
            skip = Some(format!("We shouldn't have multilib variants for {bpn}"));
        } else {
            if provides.unevaluated_python || self.ds.notes.python_vars.contains("PROVIDES") {
                unsure.push("PROVIDES depends on python".into());
            }
            if non_ml.is_none() {
                unsure.push("NON_MULTILIB_RECIPES is not known statically".into());
            }
        }
        let override_name = format!("virtclass-multilib-{variant}");
        let raw_pn = self.ds.raw("PN").unwrap_or_default();
        if skip.is_none() {
            // `OVERRIDES.replace("pn-${PN}", "pn-${PN}:pn-" + pn)` on the raw values.
            let ov = self.ds.raw("OVERRIDES").unwrap_or_default();
            self.ds.rewrite(
                "OVERRIDES",
                ov.replace("pn-${PN}", &format!("pn-${{PN}}:pn-{raw_pn}")),
                loc.clone(),
                OpKind::EventHandler,
            );
        }
        let image = self.inherits_class("image");
        let canadian = self.inherits_class("cross-canadian");
        if skip.is_none() && !image && !canadian {
            if self.inherits_class("native") {
                skip = Some("We can't extend native recipes".into());
            } else if self.inherits_class("nativesdk") || self.inherits_class("crosssdk") {
                skip = Some("We can't extend nativesdk recipes".into());
            } else if self.inherits_class("allarch") && !self.inherits_class("packagegroup") {
                match static_value(&self.ds, "MULTILIB_VARIANTS") {
                    Some(v) if v.as_deref().unwrap_or("").is_empty() => {
                        skip =
                            Some("Don't extend allarch recipes which are not packagegroups".into())
                    }
                    Some(_) => {}
                    None => unsure.push("MULTILIB_VARIANTS is not known statically".into()),
                }
            }
        }
        if let Some(msg) = skip {
            Arc::make_mut(&mut self.ds.notes).target_notes.push(format!(
                "note: BitBake would skip this recipe: {msg} (multilib_virtclass_handler at {loc}, decided from static values)"
            ));
            return;
        }
        if !unsure.is_empty() {
            Arc::make_mut(&mut self.ds.notes).target_notes.push(format!(
                "note: BitBake may skip this recipe: multilib_virtclass_handler at {loc} skips some recipes, and {}",
                unsure.join("; ")
            ));
        }
        if image {
            self.multilib_rename(&variant, &raw_pn, &loc);
            self.handler_flags(H, &loc, &strings(&["SDKTARGETSYSROOT"]), "sets");
            self.add_override(&override_name, &loc);
            if let Some(tv) = self.ds.raw(&format!("TARGET_VENDOR:{override_name}"))
                && !tv.is_empty()
            {
                self.ds
                    .rewrite("TARGET_VENDOR", tv, loc.clone(), OpKind::EventHandler);
            }
            return;
        }
        if canadian {
            for v in ["RECIPE_SYSROOT", "STAGING_DIR_TARGET", "STAGING_DIR_HOST"] {
                self.ds.rewrite(
                    v,
                    "${WORKDIR}/recipe-sysroot".into(),
                    loc.clone(),
                    OpKind::EventHandler,
                );
            }
            self.ds.rewrite(
                "RECIPE_SYSROOT_MANIFEST_SUBDIR",
                format!("nativesdk-{variant}"),
                loc.clone(),
                OpKind::EventHandler,
            );
            self.ds.rewrite(
                "MLPREFIX",
                format!("{variant}-"),
                loc.clone(),
                OpKind::EventHandler,
            );
            self.add_override(&override_name, &loc);
            return;
        }
        // SKIP_RECIPE[pn] is copied to SKIP_RECIPE[<variant>-pn] when that is unset.
        if let Some(Some(pn)) = static_value(&self.ds, "PN")
            && let Some(msg) = self.ds.raw_flag("SKIP_RECIPE", &pn)
            && !msg.is_empty()
        {
            let new = format!("{variant}-{pn}");
            if self.ds.raw_flag("SKIP_RECIPE", &new).is_none() {
                put_flag(&mut self.ds, "SKIP_RECIPE", &new, &msg);
            }
        }
        self.multilib_rename(&variant, &raw_pn, &loc);
        self.add_override(&override_name, &loc);
        match static_value(&self.ds, "INCOMPATIBLE_LICENSE_EXCEPTIONS") {
            Some(Some(pkgs)) if !pkgs.is_empty() => {
                let mut out = pkgs.clone();
                for p in pkgs.split_whitespace() {
                    out.push_str(&format!(" {variant}-{p}"));
                }
                self.ds.rewrite(
                    "INCOMPATIBLE_LICENSE_EXCEPTIONS",
                    out,
                    loc.clone(),
                    OpKind::EventHandler,
                );
            }
            Some(_) => {}
            None => self.handler_flags(
                H,
                &loc,
                &strings(&["INCOMPATIBLE_LICENSE_EXCEPTIONS"]),
                "extends",
            ),
        }
        if let Some(t) = self.ds.raw(&format!("DEFAULTTUNE:{override_name}"))
            && !t.is_empty()
        {
            self.ds
                .rewrite("DEFAULTTUNE", t, loc.clone(), OpKind::EventHandler);
        }
    }

    /// `multilib_virtclass_handler_postkeyexp` (OE-core Wrynose `classes/multilib.bbclass`),
    /// on `RecipePostKeyExpansion`: `ClassExtender` filters and package variable renames, all
    /// flagged.
    fn multilib_postkeyexp(&mut self, recipe: &Loc) {
        const H: &str = "multilib_virtclass_handler_postkeyexp";
        if !self.handles(H, "RecipePostKeyExpansion") {
            return;
        }
        if matches!(self.multilib_variant(), Ok(None)) || self.inherits_class("image") {
            return;
        }
        let loc = self.handler_loc(H, recipe);
        let mut vars = strings(&["DEPENDS", "PACKAGE_WRITE_DEPS", "PROVIDES"]);
        if !self.inherits_class("cross-canadian") {
            vars.extend(strings(&["PACKAGES", "PACKAGES_DYNAMIC"]));
            if let Some(v) = self.ds.value("PACKAGEVARS") {
                vars.extend(pwords::known_words(&v.text).into_iter().map(str::to_string));
            }
            vars.extend(strings(MAP_PACKAGEVARS));
            vars.extend(strings(&[
                "INITSCRIPT_PACKAGES",
                "USERADD_PACKAGES",
                "SYSTEMD_PACKAGES",
                "UPDATERCPN",
            ]));
            if self.inherits_class("update-alternatives") {
                vars.push("ALTERNATIVE_PRIORITY".into());
                if let Some(p) = self.ds.value("PACKAGES") {
                    vars.extend(
                        pwords::known_words(&p.text)
                            .into_iter()
                            .map(|p| format!("ALTERNATIVE_PRIORITY_{p}")),
                    );
                }
            }
        }
        vars.sort();
        vars.dedup();
        self.handler_flags(H, &loc, &vars, "filters or renames");
    }
}

/// `bb.build.ensure_task_prefix`.
fn task_name(name: &str) -> String {
    if name.starts_with("do_") {
        name.to_string()
    } else {
        format!("do_{name}")
    }
}

/// Whether an assignment was written `export VAR = ...` (the index keeps the statement's
/// first line in `snippet`; the assignment regex allows `export` only at the start).
fn is_exported(st: &StatementRow) -> bool {
    let Some(rest) = st.snippet.strip_prefix("export") else {
        return false;
    };
    if !rest.starts_with(char::is_whitespace) {
        return false;
    }
    st.name.as_deref() != Some("export") || rest.trim_start().starts_with("export")
}

/// Variables a python body changes: literal first arguments of the `d.setVar` family, and
/// whether it also writes through a computed name or hands `d` to other code.
fn python_writes(body: &str) -> (Vec<String>, bool) {
    static WRITE: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(
            r#"\bd\.(?:setVar|setVarFilter|appendVar|prependVar|delVar|renameVar|setVarFlag|setVarFlags|appendVarFlag|prependVarFlag|delVarFlag|delVarFlags)\s*\(\s*(?:'([^'\\]*)'|"([^"\\]*)")?"#,
        )
        .ok()
    });
    static PASS_D: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r"[\w.\]]\s*\((?:[^()]*,)?\s*d\s*[,)]").ok());
    let (Some(write), Some(pass_d)) = (WRITE.as_ref(), PASS_D.as_ref()) else {
        return (Vec::new(), true);
    };
    let mut sets = Vec::new();
    let mut opaque = false;
    for c in write.captures_iter(body) {
        match c.get(1).or_else(|| c.get(2)) {
            Some(m) => {
                let n = m.as_str().to_string();
                if !sets.contains(&n) {
                    sets.push(n);
                }
            }
            None => opaque = true,
        }
    }
    if pass_d.is_match(body) {
        opaque = true;
    }
    // OE-core's `oe.classextend.ClassExtender` (Wrynose `lib/oe/classextend.py`):
    // `set_filter(var, ...)` is `d.setVarFilter(var, ...)`; `map_packagevars()` filters the
    // runtime package variables; `rename_package_variables(...)` filters `PACKAGES` and
    // `PACKAGES_DYNAMIC` (and renames per-package variables, which the caller adds).
    static SET_FILTER: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r#"\.set_filter\s*\(\s*(?:'([^'\\]*)'|"([^"\\]*)")"#).ok());
    let mut add = |n: &str| {
        if !sets.iter().any(|s| s == n) {
            sets.push(n.to_string());
        }
    };
    if let Some(re) = SET_FILTER.as_ref() {
        for c in re.captures_iter(body) {
            if let Some(m) = c.get(1).or_else(|| c.get(2)) {
                add(m.as_str());
            }
        }
    }
    if body.contains(".map_packagevars(") {
        for n in MAP_PACKAGEVARS {
            add(n);
        }
    }
    if body.contains(".rename_package_variables(") {
        add("PACKAGES");
        add("PACKAGES_DYNAMIC");
    }
    (sets, opaque)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vars_from_file_like_bitbake() {
        assert_eq!(
            vars_from_file("/a/hello_1.1.bb"),
            (Some("hello".into()), Some("1.1".into()))
        );
        assert_eq!(
            vars_from_file("/a/hello_%.bbappend"),
            (Some("hello".into()), Some("%".into()))
        );
        assert_eq!(vars_from_file("/a/app.bb"), (Some("app".into()), None));
        assert_eq!(vars_from_file("/a/hello.inc"), (None, None));
        assert_eq!(vars_from_file("/a/a_b_c_d.bb"), (None, None));
    }

    #[test]
    fn re_escape_like_python() {
        assert_eq!(re_escape("/a/meta-x.y"), "/a/meta\\-x\\.y");
        assert_eq!(re_escape("a_b/c"), "a_b/c");
    }

    #[test]
    fn python_writes_finds_literal_names() {
        let (sets, opaque) = python_writes(
            "python () {\n    if d.getVar('X'):\n        d.appendVar('EXTRA_OEMAKE', ' B=1')\n        d.setVar(\"RDEPENDS:${PN}\", 'x')\n}",
        );
        assert_eq!(sets, vec!["EXTRA_OEMAKE", "RDEPENDS:${PN}"]);
        assert!(!opaque);
        let (_, opaque) = python_writes("    d.setVar(name, 'x')");
        assert!(opaque);
        let (_, opaque) = python_writes("    oe.utils.something(d)");
        assert!(opaque);
        let (_, opaque) = python_writes("    x = d.getVar('A')");
        assert!(!opaque);
    }

    #[test]
    fn python_writes_sees_class_extender_filters() {
        // nativesdk.bbclass's anonymous python (OE-core Wrynose).
        let (sets, opaque) = python_writes(
            "    clsextend = oe.classextend.ClassExtender(\"nativesdk\", [], d)\n    clsextend.rename_package_variables((d.getVar(\"PACKAGEVARS\") or \"\").split())\n    clsextend.set_filter(\"DEPENDS\", deps=True)\n    clsextend.set_filter('PROVIDES', deps=False)\n    clsextend.map_packagevars()\n    d.setVarFilter(\"X\", \"f(val)\")\n",
        );
        assert!(opaque);
        for n in ["DEPENDS", "PROVIDES", "RDEPENDS", "PKG", "PACKAGES", "X"] {
            assert!(sets.iter().any(|s| s == n), "{n}: {sets:?}");
        }
    }

    #[test]
    fn pieces_parse() {
        let p = parse_piece("meta-core:classes/base.bbclass:54 -> base_do_install").unwrap();
        assert_eq!(p.calls.as_deref(), Some("base_do_install"));
        assert_eq!(p.loc.to_string(), "meta-core:classes/base.bbclass:54");
        assert!(parse_piece("not a pointer").is_none());
    }
}

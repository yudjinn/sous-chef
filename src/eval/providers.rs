//! `PROVIDES` / `PREFERRED_PROVIDER` / `PREFERRED_VERSION` selection (T3.7).
//!
//! A port of BitBake's `lib/bb/providers.py` (`master`, 2026-11; see
//! `docs/bitbake-semantics.md`, "Providers and versions"):
//!
//! - **Which recipe file of a `PN`** ([`Providers::best_version`], `findBestProvider`): the
//!   files are bucketed by layer priority (highest first) and, inside a bucket, ordered by
//!   `DEFAULT_PREFERENCE` (lowest first), then file order. `REQUIRED_VERSION` and then
//!   `PREFERRED_VERSION` are looked up as `<K>_VERSION:pn-<pn>`, `<K>_VERSION_<pn>`, then
//!   `<K>_VERSION` (`versionVariableMatch`) in the configuration; the first file, bucket by
//!   bucket, whose version matches wins (`%` at the end matches any rest; a leading `N:`
//!   is the epoch, matched against `PE`). Otherwise, within the highest priority bucket
//!   only, a higher `DEFAULT_PREFERENCE` wins, then the higher `(PE, PV, PR)` (`vercmp`).
//! - **Which recipe provides a name** ([`Providers::providers`], `filterProviders`): every
//!   recipe that provides it (its `PN` or a `PROVIDES` word), grouped by `PN`; per `PN` the
//!   file is chosen as above among the files that provide the name. The eligible list is the
//!   `PN`s whose preferred version was found (sorted by `PN`), then the others (sorted by
//!   `PN`); a `PN` equal to the name goes first; then `PREFERRED_PROVIDER_<name>` moves its
//!   `PN` to the front and makes the choice unique. BitBake uses the first entry.
//!
//! The candidate files come from the index (`provides` table: `PN` and static `PROVIDES`
//! words, also in includes, classes and bbappends, mapped to the recipes that use them),
//! and are confirmed by evaluating each candidate (`PROVIDES` expanded). A recipe whose
//! `PROVIDES` needs expansion to name the item, or comes from a global (`INHERIT`) class,
//! is not found. `DEFAULT_PREFERENCE`, `PE` and `PR` are read from an evaluation of each
//! candidate only when some non-`.conf` file, or a `.conf` statement with an override, sets
//! one of them; otherwise every candidate has the configuration's values and no recipe pass
//! is needed. Anonymous python that sets them is not seen.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet, VecDeque};

use serde::Serialize;

use super::replay::{ConfigCache, evaluate_with, global_cache};
use super::{DataStore, OpStatus, Target};
use crate::error::{Error, Result};
use crate::index::Index;
use crate::index::query::{self, FileRow, RecipeRow, StatementFilter};
use crate::types::{FileKind, Loc};
use crate::workspace::Workspace;

/// A configuration variable that took part in a selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Setting {
    /// The name looked up, e.g. `PREFERRED_PROVIDER_virtual/foo`.
    pub name: String,
    /// Its expanded value.
    pub value: String,
    /// The assignment that gave the value (the last applied one).
    pub loc: Option<Loc>,
    /// The value contains unevaluated python.
    pub python: bool,
}

/// Why a recipe file was chosen for its `PN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionReason {
    /// The caller asked for this version.
    Requested,
    /// `REQUIRED_VERSION...` matched.
    Required,
    /// `PREFERRED_VERSION...` matched.
    Preferred,
    /// Highest `DEFAULT_PREFERENCE`, then highest version, in the highest priority layer.
    Latest,
}

/// The recipe file chosen for one `PN`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Selection {
    pub recipe: RecipeRow,
    pub reason: VersionReason,
    /// The `REQUIRED_VERSION` / `PREFERRED_VERSION` setting that applied, or that was set
    /// and matched nothing (then `reason` is `Latest` and a note says so).
    pub setting: Option<Setting>,
    /// `DEFAULT_PREFERENCE` of the chosen file (0 when unset).
    pub default_preference: i64,
    pub notes: Vec<String>,
}

impl Selection {
    /// `preferred by layer priority and version`, `PREFERRED_VERSION_x = "1.%"`, ...
    pub fn describe(&self) -> String {
        match (self.reason, &self.setting) {
            (VersionReason::Requested, _) => "selected by the version argument".into(),
            (VersionReason::Required | VersionReason::Preferred, Some(s)) => {
                format!("{} = \"{}\"", s.name, s.value)
            }
            _ if self.default_preference != 0 => format!(
                "preferred by layer priority, DEFAULT_PREFERENCE {} and version",
                self.default_preference
            ),
            _ => "preferred by layer priority and version".into(),
        }
    }
}

/// One recipe that provides a name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Provider {
    pub pn: String,
    /// The file chosen for this `PN`.
    pub selection: Selection,
    /// The `PROVIDES` assignment that names the item, or `None` when the `PN` is the item.
    pub provides_loc: Option<Loc>,
    /// The candidate's `PROVIDES` depends on python, so whether it provides is uncertain.
    pub python: bool,
}

/// Why the first provider is the selected one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderReason {
    /// No recipe provides the name.
    None,
    /// One recipe provides it.
    Only,
    /// `PREFERRED_PROVIDER_<name>` names it.
    PreferredProvider,
    /// The recipe's `PN` is the name.
    SameName,
    /// Several providers and no `PREFERRED_PROVIDER_<name>`: BitBake reports "multiple
    /// providers" and uses the first in its order.
    FirstOfSeveral,
}

/// The providers of a name, in BitBake's order (the selected one first).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderSet {
    pub name: String,
    pub providers: Vec<Provider>,
    pub reason: ProviderReason,
    /// `PREFERRED_PROVIDER_<name>`, if set.
    pub preferred_provider: Option<Setting>,
    /// Candidates from the index that do not provide the name once evaluated (`PN`).
    pub excluded: Vec<RecipeRow>,
    pub notes: Vec<String>,
}

impl ProviderSet {
    /// The selected provider (BitBake's first eligible one).
    pub fn selected(&self) -> Option<&Provider> {
        self.providers.first()
    }
}

/// `(PE, PV, PR, DEFAULT_PREFERENCE)` of a candidate file.
#[derive(Debug, Clone)]
struct Info {
    recipe: RecipeRow,
    pe: Option<String>,
    pr: Option<String>,
    dp: i64,
}

/// Provider and version selection against one configuration (machine, distro).
pub struct Providers<'a> {
    cache: &'a ConfigCache,
    index: &'a Index,
    ws: &'a Workspace,
    machine: Option<String>,
    distro: Option<String>,
    cfg: DataStore,
    per_recipe: bool,
}

impl<'a> Providers<'a> {
    /// Evaluates the configuration for `machine` / `distro` (through the process-wide
    /// cache). The index must be refreshed by the caller.
    pub fn new(
        index: &'a Index,
        ws: &'a Workspace,
        machine: Option<&str>,
        distro: Option<&str>,
    ) -> Result<Providers<'a>> {
        let target = Target {
            recipe: None,
            version: None,
            machine: machine.map(str::to_string),
            distro: distro.map(str::to_string),
        };
        let cfg = evaluate_with(global_cache(), index, ws, &target)?;
        Providers::with_config(global_cache(), index, ws, &target, cfg)
    }

    /// With a configuration already evaluated for `target.machine` / `target.distro`
    /// (`target.recipe` is ignored). Candidate recipes are evaluated through `cache`.
    pub fn with_config(
        cache: &'a ConfigCache,
        index: &'a Index,
        ws: &'a Workspace,
        target: &Target,
        cfg: DataStore,
    ) -> Result<Providers<'a>> {
        Ok(Providers {
            cache,
            index,
            ws,
            machine: target.machine.clone(),
            distro: target.distro.clone(),
            per_recipe: per_recipe_values(index)?,
            cfg,
        })
    }

    /// The configuration datastore the selection reads.
    pub fn config(&self) -> &DataStore {
        &self.cfg
    }

    /// `layer:path` target of a recipe file, with this machine and distro.
    pub fn target_for(&self, recipe: &RecipeRow) -> Target {
        Target {
            recipe: Some(format!(
                "{}:{}",
                recipe.file.layer,
                recipe.file.path.display()
            )),
            version: None,
            machine: self.machine.clone(),
            distro: self.distro.clone(),
        }
    }

    /// Evaluates one recipe file with this machine and distro.
    pub fn evaluate(&self, recipe: &RecipeRow) -> Result<DataStore> {
        evaluate_with(self.cache, self.index, self.ws, &self.target_for(recipe))
    }

    /// The recipe file BitBake uses for `pn` (`findBestProvider`), or the first file whose
    /// version matches `version` (`%` allowed, a leading `N:` is the epoch) in layer
    /// priority order. `None` when no file matches. A `REQUIRED_VERSION` that matches no
    /// file is an error, as in BitBake.
    pub fn best_version(&self, pn: &str, version: Option<&str>) -> Result<Option<Selection>> {
        let files = query::find_recipes(self.index, pn)?;
        self.best_among(pn, files, version)
    }

    fn best_among(
        &self,
        pn: &str,
        files: Vec<RecipeRow>,
        version: Option<&str>,
    ) -> Result<Option<Selection>> {
        if files.is_empty() {
            return Ok(None);
        }
        let mut notes = Vec::new();
        let required = self.version_setting("REQUIRED", pn);
        let preferred = self.version_setting("PREFERRED", pn);
        let epoch_wanted = [version, required.as_ref().map(|s| s.value.as_str())]
            .into_iter()
            .chain([preferred.as_ref().map(|s| s.value.as_str())])
            .flatten()
            .any(|v| split_epoch(v).0.is_some());
        let infos = self.ordered(files, epoch_wanted, &mut notes)?;
        if let Some(v) = version {
            return Ok(
                find_match(&infos, v).map(|i| selection(i, VersionReason::Requested, None, notes))
            );
        }
        if let Some(s) = required {
            return match find_match(&infos, &s.value) {
                Some(i) => Ok(Some(selection(i, VersionReason::Required, Some(s), notes))),
                None => Err(Error::NotFound {
                    what: format!(
                        "a version of {pn} matching {} = \"{}\"{}; versions: {}",
                        s.name,
                        s.value,
                        s.loc.as_ref().map_or(String::new(), |l| format!(" ({l})")),
                        versions(&infos)
                    ),
                }),
            };
        }
        let mut missed = None;
        if let Some(s) = preferred {
            match find_match(&infos, &s.value) {
                Some(i) => {
                    return Ok(Some(selection(i, VersionReason::Preferred, Some(s), notes)));
                }
                None => {
                    notes.push(format!(
                        "{} = \"{}\"{} matches no version of {pn} (versions: {}); BitBake uses the latest",
                        s.name,
                        s.value,
                        s.loc.as_ref().map_or(String::new(), |l| format!(" at {l}")),
                        versions(&infos)
                    ));
                    missed = Some(s);
                }
            }
        }
        Ok(latest(&infos).map(|i| selection(i, VersionReason::Latest, missed, notes)))
    }

    /// Candidate files with their `(PE, PR, DEFAULT_PREFERENCE)`, in `sortPriorities` order:
    /// layer priority (highest first), then `DEFAULT_PREFERENCE` (lowest first), then file
    /// order (layer, path).
    ///
    /// A file is evaluated for its own values only when they can matter: recipes can differ
    /// in them ([`per_recipe_values`]) and the file shares its layer priority with another
    /// file, or an epoch is asked for. A file alone in its priority is ranked by the
    /// priority alone (and its `PE` only matters for an epoch).
    fn ordered(
        &self,
        files: Vec<RecipeRow>,
        epoch_wanted: bool,
        notes: &mut Vec<String>,
    ) -> Result<Vec<Info>> {
        let mut per_priority: BTreeMap<i32, usize> = BTreeMap::new();
        for r in &files {
            *per_priority.entry(r.file.priority).or_default() += 1;
        }
        let mut out = Vec::with_capacity(files.len());
        for r in files {
            let shared = per_priority.get(&r.file.priority).copied().unwrap_or(0) > 1;
            let info = if self.per_recipe && (shared || epoch_wanted) {
                let ds = self.evaluate(&r)?;
                self.info(r, &ds, notes)?
            } else {
                self.info(r, &self.cfg, notes)?
            };
            out.push(info);
        }
        sort_infos(&mut out);
        Ok(out)
    }

    /// `(PE, PR, DEFAULT_PREFERENCE)` of `recipe` read from `ds` (its own evaluation, or
    /// the configuration when recipes cannot differ).
    fn info(&self, recipe: RecipeRow, ds: &DataStore, notes: &mut Vec<String>) -> Result<Info> {
        let get = |name: &str| {
            ds.value(name)
                .filter(|v| !v.text.trim().is_empty())
                .map(|v| (v.text.trim().to_string(), v.unevaluated_python))
        };
        let pe = get("PE").map(|(t, _)| t);
        let pr = get("PR").map(|(t, _)| t);
        let dp = match get("DEFAULT_PREFERENCE") {
            None => 0,
            Some((t, python)) => match t.parse::<i64>() {
                Ok(n) if !python => n,
                _ => {
                    notes.push(format!(
                        "DEFAULT_PREFERENCE of {} is \"{t}\" (not evaluated); taken as 0",
                        file_label(&recipe.file)
                    ));
                    0
                }
            },
        };
        Ok(Info { recipe, pe, pr, dp })
    }

    /// `versionVariableMatch(cfgData, keyword, pn)`.
    fn version_setting(&self, keyword: &str, pn: &str) -> Option<Setting> {
        [
            format!("{keyword}_VERSION:pn-{pn}"),
            format!("{keyword}_VERSION_{pn}"),
            format!("{keyword}_VERSION"),
        ]
        .into_iter()
        .find_map(|n| setting(&self.cfg, &n))
    }

    /// The configuration variable `name` (expanded), when set and not empty, with the
    /// pointer of its last applied assignment.
    pub fn setting(&self, name: &str) -> Option<Setting> {
        setting(&self.cfg, name)
    }

    /// The providers of `name`, the selected one first (`filterProviders`).
    pub fn providers(&self, name: &str) -> Result<ProviderSet> {
        let mut notes = Vec::new();
        let mut candidates: Vec<RecipeRow> = Vec::new();
        let mut seen = HashSet::new();
        for f in query::files_providing(self.index, name)? {
            for r in recipes_using(self.index, &f)? {
                if seen.insert(r.file.id) {
                    candidates.push(r);
                }
            }
        }
        // Confirm each candidate by evaluating it; group by PN.
        let mut by_pn: BTreeMap<String, Vec<(Info, Option<Loc>, bool)>> = BTreeMap::new();
        let mut excluded = Vec::new();
        for r in candidates {
            let ds = self.evaluate(&r)?;
            let (provides, loc, python) = provides_name(&ds, &r.pn, name);
            if !provides {
                excluded.push(r);
                continue;
            }
            let info = self.info(r, &ds, &mut notes)?;
            by_pn
                .entry(info.recipe.pn.clone())
                .or_default()
                .push((info, loc, python));
        }
        let mut preferred: Vec<Provider> = Vec::new();
        let mut rest: Vec<Provider> = Vec::new();
        for (pn, mut list) in by_pn {
            sort_entries(&mut list);
            let infos: Vec<Info> = list.iter().map(|(i, _, _)| i.clone()).collect();
            let mut sel_notes = Vec::new();
            let mut sel: Option<Selection> = None;
            let mut is_preferred = false;
            if let Some(s) = self.version_setting("REQUIRED", &pn) {
                match find_match(&infos, &s.value) {
                    Some(i) => {
                        sel = Some(selection(i, VersionReason::Required, Some(s), Vec::new()));
                        is_preferred = true;
                    }
                    None => {
                        notes.push(format!(
                            "{} = \"{}\" matches no version of {pn} that provides {name}; BitBake fails (no provider)",
                            s.name, s.value
                        ));
                        continue;
                    }
                }
            }
            let mut missed = None;
            if sel.is_none()
                && let Some(s) = self.version_setting("PREFERRED", &pn)
            {
                match find_match(&infos, &s.value) {
                    Some(i) => {
                        sel = Some(selection(i, VersionReason::Preferred, Some(s), Vec::new()));
                        is_preferred = true;
                    }
                    None => {
                        sel_notes.push(format!(
                            "{} = \"{}\" matches no version of {pn} (versions: {}); BitBake uses the latest",
                            s.name,
                            s.value,
                            versions(&infos)
                        ));
                        missed = Some(s);
                    }
                }
            }
            let sel = match sel {
                Some(s) => s,
                None => match latest(&infos) {
                    Some(i) => selection(i, VersionReason::Latest, missed, sel_notes),
                    None => continue,
                },
            };
            let (loc, python) = list
                .iter()
                .find(|(i, _, _)| i.recipe.file.id == sel.recipe.file.id)
                .map(|(_, l, p)| (l.clone(), *p))
                .unwrap_or((None, false));
            let p = Provider {
                pn: pn.clone(),
                selection: sel,
                provides_loc: loc,
                python,
            };
            if is_preferred {
                preferred.push(p);
            } else {
                rest.push(p);
            }
        }
        let mut eligible = preferred;
        eligible.extend(rest);
        let mut reason = match eligible.len() {
            0 => ProviderReason::None,
            1 => ProviderReason::Only,
            _ => ProviderReason::FirstOfSeveral,
        };
        if let Some(i) = eligible.iter().position(|p| p.pn == name) {
            let p = eligible.remove(i);
            eligible.insert(0, p);
            if eligible.len() > 1 {
                reason = ProviderReason::SameName;
            }
        }
        let preferred_provider = self.setting(&format!("PREFERRED_PROVIDER_{name}"));
        if let Some(pp) = &preferred_provider {
            match eligible.iter().position(|p| p.pn == pp.value) {
                Some(i) => {
                    let p = eligible.remove(i);
                    eligible.insert(0, p);
                    reason = ProviderReason::PreferredProvider;
                }
                None => notes.push(format!(
                    "{} = \"{}\"{} names no recipe that provides {name}",
                    pp.name,
                    pp.value,
                    pp.loc.as_ref().map_or(String::new(), |l| format!(" ({l})"))
                )),
            }
        }
        if eligible.iter().any(|p| p.python) {
            notes.push(format!(
                "some PROVIDES values depend on python; whether those recipes provide {name} is not certain"
            ));
        }
        Ok(ProviderSet {
            name: name.to_string(),
            providers: eligible,
            reason,
            preferred_provider,
            excluded,
            notes,
        })
    }
}

/// Resolves a recipe name to the file BitBake uses, against the configuration for
/// `machine` / `distro`: [`Providers::best_version`] after [`Providers::new`].
pub fn preferred_recipe(
    index: &Index,
    ws: &Workspace,
    pn: &str,
    version: Option<&str>,
    machine: Option<&str>,
    distro: Option<&str>,
) -> Result<Option<Selection>> {
    Providers::new(index, ws, machine, distro)?.best_version(pn, version)
}

fn selection(
    i: &Info,
    reason: VersionReason,
    setting: Option<Setting>,
    notes: Vec<String>,
) -> Selection {
    Selection {
        recipe: i.recipe.clone(),
        reason,
        setting,
        default_preference: i.dp,
        notes,
    }
}

fn setting(cfg: &DataStore, name: &str) -> Option<Setting> {
    let v = cfg.value(name)?;
    let value = v.text.trim().to_string();
    if value.is_empty() {
        return None;
    }
    let loc = cfg
        .history(name)
        .into_iter()
        .rev()
        .find(|(_, st)| *st == OpStatus::Applied)
        .map(|(op, _)| op.loc);
    Some(Setting {
        name: name.to_string(),
        value,
        loc,
        python: v.unevaluated_python,
    })
}

fn sort_infos(v: &mut [Info]) {
    v.sort_by(order);
}

fn sort_entries(v: &mut [(Info, Option<Loc>, bool)]) {
    v.sort_by(|a, b| order(&a.0, &b.0));
}

fn order(a: &Info, b: &Info) -> Ordering {
    b.recipe
        .file
        .priority
        .cmp(&a.recipe.file.priority)
        .then_with(|| a.dp.cmp(&b.dp))
        .then_with(|| a.recipe.file.layer_id.cmp(&b.recipe.file.layer_id))
        .then_with(|| a.recipe.file.path.cmp(&b.recipe.file.path))
}

fn versions(infos: &[Info]) -> String {
    let mut v: Vec<&str> = infos.iter().map(|i| i.recipe.pv.as_str()).collect();
    v.dedup();
    v.join(", ")
}

/// `findPreferredProvider`: the first file, in [`order`], matching the preferred version.
/// The value is split like BitBake's `(\d+:)*(.*)(_.*)*`: a leading `N:` is the epoch; the
/// greedy middle group keeps any `_rN` suffix in the version, so the revision never takes
/// part.
fn find_match<'i>(infos: &'i [Info], preferred: &str) -> Option<&'i Info> {
    let (epoch, version) = split_epoch(preferred);
    infos.iter().find(|i| {
        let pe_ok = epoch.is_none_or(|e| i.pe.as_deref() == Some(e));
        pe_ok && query::version_matches(&i.recipe.pv, version)
    })
}

fn split_epoch(v: &str) -> (Option<&str>, &str) {
    let mut epoch = None;
    let mut rest = v;
    while let Some((e, r)) = rest.split_once(':') {
        if e.is_empty() || !e.chars().all(|c| c.is_ascii_digit()) {
            break;
        }
        epoch = Some(e);
        rest = r;
    }
    (epoch, rest)
}

/// `findLatestProvider` over the highest priority bucket.
fn latest(infos: &[Info]) -> Option<&Info> {
    let top = infos.first()?.recipe.file.priority;
    let mut best: Option<&Info> = None;
    for i in infos.iter().filter(|i| i.recipe.file.priority == top) {
        let replace = match best {
            None => true,
            Some(b) => (b.dp == i.dp && vercmp3(b, i) == Ordering::Less) || i.dp > b.dp,
        };
        if replace {
            best = Some(i);
        }
    }
    best
}

/// `bb.utils.vercmp((pe, pv, pr), ...)`: epoch as an integer (empty is 0), then `PV`, then
/// `PR` with `vercmp_part`.
fn vercmp3(a: &Info, b: &Info) -> Ordering {
    let e = |i: &Info| {
        i.pe.as_deref()
            .and_then(|p| p.parse::<i64>().ok())
            .unwrap_or(0)
    };
    e(a).cmp(&e(b))
        .then_with(|| query::vercmp(&a.recipe.pv, &b.recipe.pv))
        .then_with(|| query::vercmp(a.pr.as_deref().unwrap_or(""), b.pr.as_deref().unwrap_or("")))
}

/// Whether an evaluated recipe provides `name`: its `PN` is the name, or the expanded
/// `PROVIDES` contains it. Also the pointer of the `PROVIDES` assignment that names it and
/// whether `PROVIDES` depends on python.
fn provides_name(ds: &DataStore, pn: &str, name: &str) -> (bool, Option<Loc>, bool) {
    let evaluated_pn = ds.value("PN").map(|v| v.text);
    if pn == name || evaluated_pn.as_deref() == Some(name) {
        return (true, None, false);
    }
    let Some(v) = ds.value("PROVIDES") else {
        return (false, None, false);
    };
    if !v.text.split_whitespace().any(|w| w == name) {
        return (v.unevaluated_python, None, v.unevaluated_python);
    }
    let loc = ds
        .history("PROVIDES")
        .into_iter()
        .filter(|(_, st)| *st == OpStatus::Applied)
        .find(|(op, _)| {
            ds.expand(&op.value)
                .text
                .split_whitespace()
                .any(|w| w == name)
        })
        .map(|(op, _)| op.loc);
    (true, loc, v.unevaluated_python)
}

/// Whether recipes can differ in `DEFAULT_PREFERENCE`, `PE` or `PR`: some non-`.conf` file
/// sets one of them, or a `.conf` statement does so with an override.
fn per_recipe_values(index: &Index) -> Result<bool> {
    for base in ["DEFAULT_PREFERENCE", "PE", "PR"] {
        for s in query::statements_by_base(index, base, &StatementFilter::default())? {
            let conf = s.loc.path.extension().is_some_and(|e| e == "conf");
            if !conf || !s.overrides.is_empty() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn file_label(f: &FileRow) -> String {
    format!("{}:{}", f.layer, f.path.display())
}

/// The recipe files that use `file`: the file itself for a `.bb`, the recipes a bbappend
/// applies to, and for an include or class every recipe that pulls it in (through other
/// includes, classes and bbappends), following the index edges.
pub fn recipes_using(index: &Index, file: &FileRow) -> Result<Vec<RecipeRow>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([file.clone()]);
    let mut done = HashSet::new();
    while let Some(f) = queue.pop_front() {
        if !done.insert(f.id) {
            continue;
        }
        match f.kind {
            FileKind::Recipe => {
                if let (Some(pn), Some(pv)) = (f.pn.clone(), f.pv.clone())
                    && seen.insert(f.id)
                {
                    out.push(RecipeRow { file: f, pn, pv });
                }
            }
            FileKind::Append => {
                for e in query::edges_from(index, f.id)? {
                    if e.kind == "bbappend"
                        && let Some(t) = e.target
                    {
                        queue.push_back(t);
                    }
                }
            }
            FileKind::Include | FileKind::Class => {
                for e in query::edges_to(index, f.id)? {
                    if e.kind != "bbappend" {
                        queue.push_back(e.src);
                    }
                }
            }
            FileKind::Conf => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_split() {
        assert_eq!(split_epoch("1.%"), (None, "1.%"));
        assert_eq!(split_epoch("2:1.0"), (Some("2"), "1.0"));
        assert_eq!(split_epoch("1:2:1.0"), (Some("2"), "1.0"));
        assert_eq!(split_epoch("git:1.0"), (None, "git:1.0"));
        assert_eq!(split_epoch("1.0_r1"), (None, "1.0_r1"));
    }
}

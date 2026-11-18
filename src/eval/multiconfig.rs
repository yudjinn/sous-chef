//! Multiconfig: several configurations in one build directory (`BBMULTICONFIG`).
//!
//! What BitBake does (BitBake 2.18.0 `lib/bb/cookerdata.py`; the rules, with the Kirkstone and
//! Scarthgap differences, are in `docs/bitbake-semantics.md`, "Multiconfig"):
//!
//! - `CookerDataBuilder.parseBaseConfiguration` parses the default configuration
//!   (`parseConfigurationFiles(prefiles, postfiles)`, `mc = ""`), then reads
//!   `BBMULTICONFIG` from **that** datastore and, for each word, parses the whole
//!   configuration again from the start (`parseConfigurationFiles(..., config)`: a fresh
//!   copy of the base environment datastore, not a fork of the finished default one), with
//!   `BB_CURRENT_MC` set to the name before anything is read. A name starting with a digit
//!   is fatal ("Multiconfig name '%s' is invalid as multiconfigs cannot start with a
//!   digit").
//! - BitBake does not include `conf/multiconfig/<name>.conf` itself: OE-core's
//!   `conf/bitbake.conf` does, right after `conf/local.conf` and the `addfragments` line and
//!   before `conf/machine/${MACHINE}.conf` (Wrynose: `require ${@"conf/multiconfig/${BB_CURRENT_MC}.conf"
//!   if "${BB_CURRENT_MC}" != "" else ""}`; Kirkstone and Scarthgap: `require
//!   conf/multiconfig/${BB_CURRENT_MC}.conf`, with `BB_CURRENT_MC` `"default"` for the
//!   default configuration and OE-core's empty `conf/multiconfig/default.conf`). So the
//!   file is found like any `require` (the including file's directory, then `BBPATH`), a
//!   missing one stops the parse ("Could not require file ..."), and its `MACHINE` decides
//!   which machine conf is read.
//! - Every recipe is parsed once per configuration, on a copy of that configuration's
//!   datastore (`parseRecipeVariants`), so provider selection (`PREFERRED_PROVIDER_*`,
//!   `PREFERRED_VERSION_*`) and `BBMASK` are per configuration.
//!
//! What sous-chef does: [`super::Target::mc`] names the configuration; the configuration pass
//! is replayed from the start with `BB_CURRENT_MC` set (`replay::config_pass`), the replay
//! follows the `require` where `bitbake.conf` has it (the Wrynose python form is recognised by
//! [`oe_require_words`], the older form expands by itself), and the cache is keyed by the
//! name. The default configuration is replayed exactly as before (no `BB_CURRENT_MC`).
//! [`list`] says which configurations the workspace has; [`mcdepends`] reads the
//! cross-configuration task dependencies of an evaluated recipe.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::replay::{ConfigCache, evaluate_with, global_cache, normalize};
use super::{DataStore, OpStatus, Target};
use crate::error::{Error, Result};
use crate::index::Index;
use crate::index::query::{self, FileSet};
use crate::parse::words;
use crate::types::{FileKind, LineSpan, Loc};
use crate::workspace::Workspace;
use crate::workspace::resolve;

/// The prefix of a target in a multiconfig (`mc:<name>:<target>`).
pub const PREFIX: &str = "mc:";
/// The old spelling, which BitBake still rewrites to `mc:` on the command line (BitBake
/// 2.0 to 2.18 `cooker.py` `checkPackages`).
pub const OLD_PREFIX: &str = "multiconfig:";

/// `mc:<name>:<rest>` (or `multiconfig:<name>:<rest>`) as `(name, rest)`; `None` for any
/// other text. Like BitBake's `split_mc`, there must be a second `:`; the name may be empty
/// (`mc::<target>` is the default configuration).
pub fn split_mc(s: &str) -> Option<(&str, &str)> {
    let rest = s
        .strip_prefix(PREFIX)
        .or_else(|| s.strip_prefix(OLD_PREFIX))?;
    rest.split_once(':')
}

/// `target` with the multiconfig made explicit: an empty `mc` is `None` (the default
/// configuration), and a recipe written `mc:<name>:<recipe>` gives `mc` and the recipe. A
/// recipe prefix that names another configuration than `mc` is an error.
pub fn normalize_target(target: &Target) -> Result<Target> {
    let mut t = target.clone();
    t.mc =
        t.mc.as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string);
    if let Some(r) = t.recipe.clone()
        && let Some((name, rest)) = split_mc(r.trim())
    {
        let name = name.trim();
        if let Some(m) = &t.mc
            && m != name
        {
            return Err(Error::Invalid {
                msg: format!(
                    "the recipe {r:?} names multiconfig {name:?} but mc is {m:?}; give one"
                ),
            });
        }
        t.mc = (!name.is_empty()).then(|| name.to_string());
        t.recipe = Some(rest.trim().to_string());
    }
    Ok(t)
}

/// One configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Multiconfig {
    pub name: String,
    /// `conf/multiconfig/<name>.conf` as the `require` in `bitbake.conf` finds it.
    pub file: Option<Loc>,
}

/// Where the list of names came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// `BBMULTICONFIG` of the evaluated default configuration.
    Configuration,
    /// A kas project without a build directory: the `mc:` / `multiconfig:` targets, which
    /// kas writes into `local.conf` as `BBMULTICONFIG ?= "..."` (kas 5.5 `config.py`
    /// `get_multiconfig`, `libcmds.py` `WriteBBConfig._write_local_conf`).
    Kas,
}

/// The workspace's configurations besides the default one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Multiconfigs {
    pub source: Source,
    /// The value the names were read from (`BBMULTICONFIG`, or the kas targets joined).
    pub value: Option<String>,
    /// The assignment that gave `BBMULTICONFIG` its value (the last applied one).
    pub setting: Option<Loc>,
    /// Why the list may be incomplete or wrong (the value depends on python or on
    /// something not evaluated); `None` when it is static.
    pub uncertain: Option<String>,
    /// The names, in order, each with its file.
    pub enabled: Vec<Multiconfig>,
    /// `conf/multiconfig/*.conf` files along `BBPATH` that `enabled` does not list (with
    /// `uncertain` set: present, not confirmed enabled).
    pub present: Vec<Multiconfig>,
    /// What BitBake would stop on: a name without a file, a name starting with a digit.
    pub problems: Vec<String>,
}

impl Multiconfigs {
    /// Nothing to say: no name, no file, no problem.
    pub fn is_empty(&self) -> bool {
        self.enabled.is_empty() && self.present.is_empty() && self.problems.is_empty()
    }

    /// The names an evaluation accepts: the enabled ones, and with an uncertain value also
    /// the present ones.
    pub fn known(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.enabled.iter().map(|m| m.name.as_str()).collect();
        if self.uncertain.is_some() {
            out.extend(self.present.iter().map(|m| m.name.as_str()));
        }
        out
    }
}

/// [`list_with`] for the default machine and distro, through the process-wide cache.
pub fn list(index: &Index, ws: &Workspace) -> Result<Multiconfigs> {
    list_with(global_cache(), index, ws, None, None)
}

/// The workspace's multiconfigs, as BitBake reads them: `BBMULTICONFIG` of the default
/// configuration evaluated for `machine` / `distro` (as given to BitBake's environment),
/// each name with its `conf/multiconfig/<name>.conf` along that configuration's `BBPATH`.
/// In a kas project without a build directory the names are those kas would write. A
/// listed name without a file, and a name BitBake rejects, are [`Multiconfigs::problems`];
/// nothing here fails on the metadata. The index must be refreshed by the caller.
pub fn list_with(
    cache: &ConfigCache,
    index: &Index,
    ws: &Workspace,
    machine: Option<&str>,
    distro: Option<&str>,
) -> Result<Multiconfigs> {
    let base = evaluate_with(
        cache,
        index,
        ws,
        &Target {
            machine: machine.map(str::to_string),
            distro: distro.map(str::to_string),
            ..Target::default()
        },
    )?;
    let kas = ws
        .kas
        .as_ref()
        .filter(|k| !k.files.is_empty() && ws.build_dir.is_none());
    let mut out = match kas {
        Some(k) => {
            let names = k.multiconfigs();
            let headers = k.multiconfig_headers();
            Multiconfigs {
                source: Source::Kas,
                value: Some(names.join(" ")),
                setting: None,
                uncertain: (!headers.is_empty()).then(|| {
                    format!(
                        "a local_conf_header entry of the kas files mentions BBMULTICONFIG ({}), which is written before kas's `BBMULTICONFIG ?=` line and is not evaluated without a build directory",
                        headers.join(", ")
                    )
                }),
                enabled: names
                    .into_iter()
                    .map(|name| Multiconfig { name, file: None })
                    .collect(),
                present: Vec::new(),
                problems: Vec::new(),
            }
        }
        None => from_configuration(&base),
    };
    let found = mc_files(index, ws)?;
    let dirs = search_dirs(ws, &base);
    for m in &mut out.enabled {
        m.file = find_file(ws, &dirs, &found, &m.name);
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut problems = Vec::new();
    for m in &out.enabled {
        if !seen.insert(m.name.clone()) {
            continue;
        }
        if m.name.starts_with(|c: char| c.is_ascii_digit()) {
            problems.push(format!(
                "multiconfig name {:?} starts with a digit: BitBake stops with \"Multiconfig name '{}' is invalid as multiconfigs cannot start with a digit\"",
                m.name, m.name
            ));
        }
        if m.file.is_none() {
            problems.push(format!(
                "BBMULTICONFIG lists {:?} but no conf/multiconfig/{}.conf is found along BBPATH: BitBake stops parsing with \"Could not require file conf/multiconfig/{}.conf\"",
                m.name, m.name, m.name
            ));
        }
    }
    out.enabled.retain({
        let mut once = BTreeSet::new();
        move |m| once.insert(m.name.clone())
    });
    out.problems = problems;
    // The files along BBPATH that no enabled name uses.
    let mut present: Vec<Multiconfig> = Vec::new();
    for (name, _) in &found {
        if seen.contains(name) || present.iter().any(|p| &p.name == name) {
            continue;
        }
        if let Some(file) = find_file(ws, &dirs, &found, name) {
            present.push(Multiconfig {
                name: name.clone(),
                file: Some(file),
            });
        }
    }
    out.present = present;
    Ok(out)
}

/// The names from `BBMULTICONFIG` of the default configuration (`.split()`).
fn from_configuration(base: &DataStore) -> Multiconfigs {
    let value = base.value("BBMULTICONFIG");
    let setting = base
        .history("BBMULTICONFIG")
        .into_iter()
        .rev()
        .find(|(op, st)| *st == OpStatus::Applied && op.var.flag.is_none())
        .map(|(op, _)| op.loc);
    let notes = base.replay();
    let (names, uncertain) = match &value {
        None => (Vec::new(), None),
        Some(v) => {
            let mut why = Vec::new();
            if v.unevaluated_python {
                why.push("it depends on python that is not evaluated".to_string());
            }
            if !v.unresolved.is_empty() {
                why.push(format!(
                    "it depends on unresolved {}",
                    v.unresolved.join(", ")
                ));
            }
            if notes.python_vars.contains("BBMULTICONFIG")
                || notes.uncertain_vars.contains("BBMULTICONFIG")
            {
                why.push("python or an uncertain include may change it".to_string());
            }
            let names: Vec<String> = words::known_words(&v.text)
                .into_iter()
                .map(str::to_string)
                .collect();
            (names, (!why.is_empty()).then(|| why.join("; ")))
        }
    };
    Multiconfigs {
        source: Source::Configuration,
        value: value.map(|v| v.text),
        setting,
        uncertain,
        enabled: names
            .into_iter()
            .map(|name| Multiconfig { name, file: None })
            .collect(),
        present: Vec::new(),
        problems: Vec::new(),
    }
}

/// Every indexed `conf/multiconfig/<name>.conf` (any layer, the build directory), by name.
fn mc_files(index: &Index, ws: &Workspace) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    for f in query::files_by_kind_in(index, &[FileKind::Conf], FileSet::ALL)? {
        let comps: Vec<String> = f
            .path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();
        let [conf, dir, file] = comps.as_slice() else {
            continue;
        };
        let Some(name) = file.strip_suffix(".conf") else {
            continue;
        };
        if conf != "conf" || dir != "multiconfig" || name.is_empty() {
            continue;
        }
        if let Some(abs) = ws.abs_path(&f.loc(LineSpan::line(1))) {
            out.push((name.to_string(), canon(&abs)));
        }
    }
    Ok(out)
}

fn canon(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| normalize(p))
}

/// Where `require conf/multiconfig/<name>.conf` in `conf/bitbake.conf` looks, in order
/// (`ConfHandler.include_single_file`: the including file's directory, then `BBPATH`).
fn search_dirs(ws: &Workspace, base: &DataStore) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(v) = base
        .files()
        .iter()
        .find(|v| v.kind == FileKind::Conf && v.loc.path == Path::new("conf/bitbake.conf"))
        && let Some(abs) = ws.abs_path(&v.loc)
        && let Some(parent) = abs.parent()
    {
        dirs.push(parent.to_path_buf());
    }
    let mut bbpath: Vec<PathBuf> = Vec::new();
    if let Some(v) = base.value("BBPATH")
        && !v.unevaluated_python
    {
        for e in v
            .text
            .split(':')
            .filter(|e| !e.is_empty() && !e.contains("${"))
        {
            let p = Path::new(e);
            if p.is_absolute() {
                bbpath.push(canon(p));
            }
        }
    }
    if bbpath.is_empty() {
        bbpath = resolve::bbpath(ws);
    }
    for d in bbpath {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    dirs
}

/// The first `<dir>/conf/multiconfig/<name>.conf` that is an indexed file.
fn find_file(
    ws: &Workspace,
    dirs: &[PathBuf],
    found: &[(String, PathBuf)],
    name: &str,
) -> Option<Loc> {
    for d in dirs {
        let cand = canon(&d.join("conf/multiconfig").join(format!("{name}.conf")));
        if found.iter().any(|(n, p)| n == name && *p == cand) {
            return ws.loc_for(&cand, LineSpan::line(1));
        }
    }
    None
}

/// Checks that `mc` is a configuration of the workspace before an evaluation in it
/// (`target` gives the machine and distro of the default configuration that lists it). An
/// unknown name is `NotFound` listing the known ones (BitBake: "Multiconfig dependency ...
/// depends on nonexistent multiconfig configuration named ..."); a listed name without a
/// file is `Invalid` (BitBake stops parsing). `Ok(Some(note))` when the name is accepted
/// without certainty (the list depends on python, or comes from kas files).
pub fn check(
    cache: &ConfigCache,
    index: &Index,
    ws: &Workspace,
    target: &Target,
    mc: &str,
) -> Result<Option<String>> {
    let all = list_with(
        cache,
        index,
        ws,
        target.machine.as_deref(),
        target.distro.as_deref(),
    )?;
    if let Some(m) = all.enabled.iter().find(|m| m.name == mc) {
        if m.file.is_none() {
            return Err(Error::Invalid {
                msg: format!(
                    "multiconfig {mc} is listed in BBMULTICONFIG but conf/multiconfig/{mc}.conf is not found along BBPATH; BitBake cannot parse it either (\"Could not require file conf/multiconfig/{mc}.conf\")"
                ),
            });
        }
        return Ok(all.uncertain.as_ref().map(|why| {
            format!("note: multiconfig {mc} is taken from a list that may be wrong: {why}")
        }));
    }
    if all.uncertain.is_some()
        && let Some(p) = all.present.iter().find(|p| p.name == mc)
    {
        return Ok(Some(format!(
            "note: multiconfig {mc} ({}) is present but not confirmed enabled: BBMULTICONFIG {}",
            p.file.as_ref().map_or("-".into(), Loc::to_string),
            all.uncertain.as_deref().unwrap_or("")
        )));
    }
    let known = all.known();
    let mut what = format!(
        "multiconfig {mc}; known: {}",
        if known.is_empty() {
            "none (BBMULTICONFIG is empty or unset)".to_string()
        } else {
            known.join(", ")
        }
    );
    if let Some(p) = all.present.iter().find(|p| p.name == mc) {
        what.push_str(&format!(
            "; {} exists but BBMULTICONFIG does not list {mc}",
            p.file.as_ref().map_or("-".into(), Loc::to_string)
        ));
    }
    Err(Error::NotFound { what })
}

/// OE-core Wrynose `conf/bitbake.conf:824` (from OE-core `ff469ab2e8`, "bitbake.conf: Handle
/// empty BB_CURRENT_MC", for BitBake 2.12's empty default name).
const OE_REQUIRE: &str =
    r#"${@"conf/multiconfig/${BB_CURRENT_MC}.conf" if "${BB_CURRENT_MC}" != "" else ""}"#;

/// For the argument of an `include` / `require`: when it is OE-core's multiconfig `require`
/// in its python form ([`OE_REQUIRE`], whitespace ignored) and `BB_CURRENT_MC` has a static
/// value (a multiconfig pass), the words it expands to (`conf/multiconfig/<name>.conf`, or
/// nothing for an empty name). `None` otherwise: the default configuration does not set
/// `BB_CURRENT_MC`, and its replay is left as it was. The Kirkstone / Scarthgap form
/// (`conf/multiconfig/${BB_CURRENT_MC}.conf`) needs no help: it expands with the variable.
pub fn oe_require_words(text: &str, ds: &DataStore) -> Option<String> {
    let squash = |s: &str| s.split_whitespace().collect::<String>();
    if squash(text) != squash(OE_REQUIRE) {
        return None;
    }
    let v = ds.value("BB_CURRENT_MC")?;
    if v.unevaluated_python || !v.unresolved.is_empty() || v.text.contains("${") {
        return None;
    }
    let mc = v.text.trim();
    Some(if mc.is_empty() {
        String::new()
    } else {
        format!("conf/multiconfig/{mc}.conf")
    })
}

/// The target note of a configuration pass in multiconfig `mc`: which file was read for it.
pub fn pass_note(ds: &DataStore, mc: &str) -> String {
    let rel = format!("conf/multiconfig/{mc}.conf");
    match ds.files().iter().find(|v| v.loc.path == Path::new(&rel)) {
        Some(v) => format!(
            "note: multiconfig {mc}: the configuration parsed again with BB_CURRENT_MC = \"{mc}\", {rel} ({}:{}) read where conf/bitbake.conf requires it",
            v.loc.layer,
            v.loc.path.display()
        ),
        None => format!(
            "note: multiconfig {mc}: the configuration parsed again with BB_CURRENT_MC = \"{mc}\", but conf/bitbake.conf never read {rel} (OE-core's requires it after conf/local.conf), so nothing of it is applied"
        ),
    }
}

/// One entry of a task's `mcdepends` flag (`mc:<from>:<to>:<recipe>:<task>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McDepend {
    /// The task whose flag it is (`do_install`).
    pub task: String,
    /// The entry as expanded.
    pub entry: String,
    /// `None` when the entry does not have exactly five `:` fields (BitBake stops: "does not
    /// contain exactly four ':' characters").
    pub from: Option<String>,
    pub to: Option<String>,
    pub recipe: Option<String>,
    pub dep_task: Option<String>,
    /// `from` is the configuration evaluated (BitBake adds the dependency only then,
    /// `runqueue.py` `add_mc_dependencies`: `if mc == frommc`).
    pub applies: bool,
    /// The value depends on python or unresolved variables.
    pub uncertain: bool,
    /// The assignments to the flag that are applied.
    pub locs: Vec<Loc>,
}

/// The `mcdepends` entries of every task of an evaluated recipe (`__BBTASKS`; BitBake
/// 2.18.0 `build.py` `add_tasks` reads the expanded flag of each task, `taskdata.py`
/// `add_mcdepends` splits it on whitespace and requires five `:` fields). `applies` is
/// decided against `BB_CURRENT_MC` of `ds` (empty for the default configuration).
pub fn mcdepends(ds: &DataStore) -> Vec<McDepend> {
    let current = ds
        .value("BB_CURRENT_MC")
        .map(|v| v.text)
        .unwrap_or_default();
    let mut out = Vec::new();
    let mut tasks: Vec<String> = ds.tasks().to_vec();
    tasks.dedup();
    for task in tasks {
        let Some(v) = ds.flag(&task, "mcdepends") else {
            continue;
        };
        let locs: Vec<Loc> = ds
            .flag_history(&task, "mcdepends")
            .into_iter()
            .filter(|(_, st)| *st == OpStatus::Applied)
            .map(|(op, _)| op.loc)
            .collect();
        let uncertain = v.unevaluated_python || !v.unresolved.is_empty();
        for entry in words::words(&v.text) {
            let parts: Vec<&str> = entry.split(':').collect();
            let five = parts.len() == 5 && !words::is_unevaluated(entry);
            let field = |i: usize| five.then(|| parts[i].to_string());
            out.push(McDepend {
                task: task.clone(),
                entry: entry.to_string(),
                from: field(1),
                to: field(2),
                recipe: field(3),
                dep_task: field(4),
                applies: five && parts[1] == current,
                uncertain: uncertain || words::is_unevaluated(entry),
                locs: locs.clone(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_like_bitbake() {
        assert_eq!(split_mc("mc:a:hello"), Some(("a", "hello")));
        assert_eq!(split_mc("mc::hello"), Some(("", "hello")));
        assert_eq!(split_mc("multiconfig:a:hello"), Some(("a", "hello")));
        assert_eq!(
            split_mc("mc:a:virtual:native:x"),
            Some(("a", "virtual:native:x"))
        );
        assert_eq!(split_mc("mc:hello"), None);
        assert_eq!(split_mc("meta:recipes/x.bb"), None);
        assert_eq!(split_mc("hello"), None);
    }

    #[test]
    fn targets_are_normalised() {
        let t = normalize_target(&Target {
            recipe: Some("mc:musl:hello".into()),
            ..Target::default()
        })
        .unwrap();
        assert_eq!(t.mc.as_deref(), Some("musl"));
        assert_eq!(t.recipe.as_deref(), Some("hello"));
        let t = normalize_target(&Target {
            mc: Some(" ".into()),
            ..Target::default()
        })
        .unwrap();
        assert_eq!(t, Target::default());
        assert!(
            normalize_target(&Target {
                recipe: Some("mc:a:hello".into()),
                mc: Some("b".into()),
                ..Target::default()
            })
            .is_err()
        );
    }
}

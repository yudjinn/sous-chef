//! Lint engine, `Rule` trait and registry. Rules live in `rules/`, one file each.
//!
//! # Flow of [`run`]
//!
//! 1. **Target** ([`resolve_target`]): the whole workspace, a layer, a recipe, or files (a
//!    pointer `layer:path[:A-B]`, a path to a file, or a directory inside a layer).
//! 2. **Rules** ([`Options::rules`], `[lint] disable`): with no selection every rule runs
//!    except those disabled in `sous-chef.toml` and those off by default
//!    ([`Rule::default_enabled`]). A selection names ids or groups; a rule named by its exact
//!    id runs even when it is disabled or off by default, a group does not override either.
//!    `style/variable-order` (and the group `style`) is an alias of `hygiene/variable-order`.
//! 3. **Units.** File scope: every file of the target. Recipe scope: every `.bb` of the
//!    target, with the [`DataStore`] from the evaluator ([`Evaluate`]); when the evaluator is
//!    not available the recipe-scope rules are skipped with a note. Layer scope: every layer
//!    of the target (for a recipe or file target, the layers the files are in, and only the
//!    findings located in those files are kept).
//! 4. **Releases.** Each unit is checked against the releases of its layer
//!    ([`Layer::lint_releases`]); a file outside every layer (the build directory's
//!    `conf/*.conf`) against the workspace release alone. A rule runs for a unit only when its
//!    [`Rule::releases`] range contains one of them.
//! 5. **Post-processing**: `[lint.severity]` overrides (by id, or by group; an id wins over
//!    its group), inline suppression, the minimum severity, then sorting by file (layer name,
//!    path), line, rule id and message, with exact duplicates removed.
//!
//! Inline suppression: a comment `# sous-chef: allow <id-or-group>[, <id-or-group>...]` on
//! the line before a finding's line, or before the statement that contains it. Several such
//! comment lines may be stacked directly above.
//!
//! Upstream layers (not writable) are skipped when linting the whole workspace unless
//! [`Options::all`] is set; if no layer is writable at all, every layer is linted (the same
//! convention as release detection). Naming a layer, recipe or file lints it whatever its
//! layer.
//!
//! Unknown ids in `sous-chef.toml` (`[lint] disable`, `[lint.severity]`) do not stop lint:
//! each one becomes a note in the report, so a typo is visible without making every lint run
//! fail. Unknown ids in [`Options::rules`] are an error (the caller typed them just now).

pub mod helpers;
pub mod rules;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::eval::DataStore;
use crate::index::{Index, query};
use crate::parse::{ParsedFile, lexer};
use crate::release::Release;
use crate::render::{self, Format, Table};
use crate::types::{FileKind, LineSpan, Loc};
use crate::workspace::{BUILD_LAYER, Layer, Workspace};

/// What the engine hands a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Scope {
    /// One parsed file.
    File,
    /// The evaluated recipe tree.
    Recipe,
    /// Index queries over a layer.
    Layer,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::File => "file",
            Scope::Recipe => "recipe",
            Scope::Layer => "layer",
        }
    }
}

/// Ordered: `Info < Warning < Error`.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    /// `info`, `warning`, `error`.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub rule: &'static str,
    pub severity: Severity,
    pub loc: Loc,
    /// One line: what is wrong, then what to write instead.
    pub message: String,
}

/// Input to `Rule::check`. Which fields are set depends on the rule's scope:
///
/// - `File`: `file`, `source`, `kind`, and `loc` covering the whole file.
/// - `Recipe`: as `File` for the recipe's `.bb`, plus `data`.
/// - `Layer`: `layer`; `loc` points at the layer's `conf/layer.conf`, line 1.
///
/// `index`, `ws` and `releases` are always set. `releases` (oldest first, never empty) are
/// the releases the unit is checked against: [`Layer::lint_releases`] of its layer, or the
/// workspace release alone for a file outside every layer. A rule that holds for only some
/// of them names them in the message (see [`helpers::release_list`]).
pub struct LintContext<'a> {
    pub file: Option<&'a ParsedFile>,
    /// The file's text, for rules that look at raw lines (tabs, trailing whitespace).
    pub source: Option<&'a str>,
    pub kind: Option<FileKind>,
    pub data: Option<&'a DataStore>,
    /// Layer name, for layer scope.
    pub layer: Option<&'a str>,
    pub index: &'a Index,
    pub ws: &'a Workspace,
    pub loc: Loc,
    pub releases: &'a [Release],
}

pub trait Rule: Sync {
    /// The rule id, e.g. `"override/legacy-syntax"`.
    fn id(&self) -> &'static str;
    fn scope(&self) -> Scope;
    fn default_severity(&self) -> Severity;
    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>);

    /// The releases whose layers this rule applies to. The engine skips the rule for a unit
    /// when none of [`LintContext::releases`] is in the range. Default: every supported
    /// release. Derive a range from a `Feature` (`Feature::first()..=Feature::last()`)
    /// rather than naming codenames.
    fn releases(&self) -> RangeInclusive<Release> {
        Release::FLOOR..=Release::NEWEST
    }

    /// False for a rule that runs only when selected by its exact id (for example
    /// `hygiene/variable-order`). Default: true.
    fn default_enabled(&self) -> bool {
        true
    }
}

/// Every rule in the catalogue.
pub fn all_rules() -> &'static [&'static dyn Rule] {
    rules::ALL
}

/// The rule groups.
pub const GROUPS: [&str; 3] = ["override", "org", "hygiene"];

/// Alternative names accepted wherever a rule id is (`[lint] disable`, `[lint.severity]`,
/// selections, suppression comments): `(alias, id)`. An alias group (`style`) stands for
/// the ids its aliases name.
pub const ALIASES: &[(&str, &str)] = &[("style/variable-order", "hygiene/variable-order")];

/// The group of a rule id (`override/dead` -> `override`).
pub fn group_of(id: &str) -> &str {
    id.split('/').next().unwrap_or(id)
}

fn canonical(entry: &str) -> &str {
    let e = entry.trim().trim_end_matches('/');
    ALIASES
        .iter()
        .find(|(a, _)| *a == e)
        .map_or(e, |(_, id)| id)
}

/// Whether a selection, `disable` or suppression entry names rule `id` exactly (or through
/// an alias).
fn names_exactly(entry: &str, id: &str) -> bool {
    canonical(entry) == id
}

/// Whether an entry names `id`, its group, or an alias group containing it.
pub fn entry_matches(entry: &str, id: &str) -> bool {
    let e = canonical(entry);
    e == id
        || e == group_of(id)
        || ALIASES
            .iter()
            .any(|(alias, target)| *target == id && group_of(alias) == e)
}

/// Whether `[lint] disable` disables rule `id` (by id, group or alias).
pub fn is_disabled(config: &Config, id: &str) -> bool {
    config.lint.disable.iter().any(|d| entry_matches(d, id))
}

/// Whether an entry names a known rule, group or alias among `rules`.
fn entry_known(entry: &str, rules: &[&dyn Rule]) -> bool {
    rules.iter().any(|r| entry_matches(entry, r.id()))
}

// ---------------------------------------------------------------------------------------
// Targets

/// What to lint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Every writable layer (every layer with [`Options::all`], or when none is writable),
    /// plus the build directory's configuration files.
    Workspace,
    /// One layer by name; [`BUILD_LAYER`] is the build directory.
    Layer(String),
    /// A recipe by `PN` (and `PV`): its `.bb` as BitBake would pick it from layer priority
    /// and version, the `.inc` files it requires or includes, and its bbappends.
    Recipe {
        name: String,
        version: Option<String>,
    },
    /// Files by absolute path; `lines` restricts the reported findings to that range (only
    /// meaningful with a single file).
    Files {
        files: Vec<PathBuf>,
        lines: Option<LineSpan>,
    },
}

/// Resolves a target as the CLI and the `lint` tool take it, in this order: nothing (the
/// workspace); a layer name (`build` for the build directory); a pointer `layer:path` or
/// `layer:path:A-B`; a path to a file or directory (relative to `work_dir`), which must pass
/// [`Workspace::confine`] (a layer root is that layer); a recipe name `pn`, or `pn_pv`.
pub fn resolve_target(
    ws: &Workspace,
    index: &Index,
    text: Option<&str>,
    work_dir: &Path,
) -> Result<Target> {
    let Some(t) = text.map(str::trim).filter(|t| !t.is_empty()) else {
        return Ok(Target::Workspace);
    };
    if ws.layer(t).is_some() || (t == BUILD_LAYER && ws.build_dir.is_some()) {
        return Ok(Target::Layer(t.to_string()));
    }
    if let Some((layer, rest)) = t.split_once(':')
        && (ws.layer(layer).is_some() || (layer == BUILD_LAYER && ws.build_dir.is_some()))
    {
        let (path, lines) = match rest.rsplit_once(':') {
            Some((p, l)) => match l.parse::<LineSpan>() {
                Ok(span) => (p, Some(span)),
                Err(_) => (rest, None),
            },
            None => (rest, None),
        };
        let loc = Loc {
            layer: layer.to_string(),
            path: PathBuf::from(path),
            lines: LineSpan::line(1),
        };
        let abs = ws.abs_path(&loc).ok_or_else(|| Error::Invalid {
            msg: format!("bad pointer {t:?}: the path must be relative to the layer root"),
        })?;
        let abs = ws.confine(&abs)?;
        if !abs.is_file() {
            return Err(Error::NotFound {
                what: format!("file {layer}:{path}"),
            });
        }
        return Ok(Target::Files {
            files: vec![abs],
            lines,
        });
    }
    let candidate = work_dir.join(t);
    if candidate.exists() {
        let abs = ws.confine(&candidate)?;
        if abs.is_file() {
            return Ok(Target::Files {
                files: vec![abs],
                lines: None,
            });
        }
        if let Some(l) = ws.layers.iter().find(|l| l.root == abs) {
            return Ok(Target::Layer(l.name.clone()));
        }
        let mut files: Vec<PathBuf> = Vec::new();
        for layer in target_layers_all(ws) {
            files.extend(
                layer_files(ws, index, &layer)?
                    .into_iter()
                    .filter(|p| p.starts_with(&abs)),
            );
        }
        if files.is_empty() {
            return Err(Error::NotFound {
                what: format!("indexed BitBake files under {}", abs.display()),
            });
        }
        return Ok(Target::Files { files, lines: None });
    }
    if query::preferred_recipe(index, t, None)?.is_some() {
        return Ok(Target::Recipe {
            name: t.to_string(),
            version: None,
        });
    }
    if let Some((pn, pv)) = t.rsplit_once('_')
        && query::preferred_recipe(index, pn, Some(pv))?.is_some()
    {
        return Ok(Target::Recipe {
            name: pn.to_string(),
            version: Some(pv.to_string()),
        });
    }
    Err(Error::NotFound {
        what: format!(
            "lint target {t:?}: not a layer, file pointer (layer:path), path or recipe name"
        ),
    })
}

/// Every layer name plus the build pseudo-layer when there is a build directory.
fn target_layers_all(ws: &Workspace) -> Vec<String> {
    let mut v: Vec<String> = ws.layers.iter().map(|l| l.name.clone()).collect();
    if ws.build_dir.is_some() {
        v.push(BUILD_LAYER.to_string());
    }
    v
}

/// Absolute paths of the indexed files of a layer, by path.
fn layer_files(ws: &Workspace, index: &Index, layer: &str) -> Result<Vec<PathBuf>> {
    // query.rs has no "files of a layer" reader yet; this is the one read the engine makes
    // itself.
    let mut st = index.conn().prepare(
        "SELECT f.path FROM files f JOIN layers l ON l.id = f.layer_id \
         WHERE l.name = ?1 ORDER BY f.path",
    )?;
    let rows = st.query_map([layer], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for rel in rows {
        let loc = Loc {
            layer: layer.to_string(),
            path: PathBuf::from(rel?),
            lines: LineSpan::line(1),
        };
        if let Some(abs) = ws.abs_path(&loc) {
            out.push(abs);
        }
    }
    Ok(out)
}

/// The files of a recipe target: the `.bb`, the files its `require` / `include` /
/// `include_all` statements pull in (not classes or what classes pull in), the bbappends and
/// what they include. Returns the `.bb` first.
fn recipe_files(ws: &Workspace, index: &Index, recipe: &query::RecipeRow) -> Result<Vec<PathBuf>> {
    let mut ids = vec![recipe.file.id];
    ids.extend(
        query::appends_for(index, recipe.file.id)?
            .iter()
            .map(|f| f.id),
    );
    let mut out: Vec<PathBuf> = Vec::new();
    let mut seen = HashSet::new();
    for id in ids {
        let mut skip_below: Option<u32> = None;
        for node in query::tree(index, id)? {
            if let Some(d) = skip_below {
                if node.depth > d {
                    continue;
                }
                skip_below = None;
            }
            if node.kind == "inherit" {
                skip_below = Some(node.depth);
                continue;
            }
            let Some(f) = node.file else { continue };
            if f.kind == FileKind::Class || !seen.insert(f.id) {
                continue;
            }
            if let Some(abs) = ws.abs_path(&f.loc(LineSpan::line(1))) {
                out.push(abs);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Options, evaluation seam, report

/// How to lint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// Rule ids or groups to run; empty: every enabled rule.
    pub rules: Vec<String>,
    /// Drop findings below this severity.
    pub min_severity: Option<Severity>,
    /// Include layers that are not writable when linting the whole workspace.
    pub all: bool,
}

/// Produces the [`DataStore`] for recipe-scope rules. A seam: tests supply their own.
pub trait Evaluate: Sync {
    /// `Ok(None)`: no evaluator is available; recipe-scope rules are skipped with a note.
    fn evaluate(
        &self,
        index: &Index,
        ws: &Workspace,
        target: &crate::eval::Target,
    ) -> Result<Option<DataStore>>;
}

/// The real evaluator, [`crate::eval::evaluate`]. While that is the M0 stub (it returns
/// `Error::not_implemented()`) this reports "not available", so recipe-scope rules start
/// running as soon as T3.4 is merged, with no change here.
pub struct Replay;

impl Evaluate for Replay {
    fn evaluate(
        &self,
        index: &Index,
        ws: &Workspace,
        target: &crate::eval::Target,
    ) -> Result<Option<DataStore>> {
        match crate::eval::evaluate(index, ws, target) {
            Ok(d) => Ok(Some(d)),
            Err(Error::Invalid { msg }) if msg == "not implemented" => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// What [`run`] returns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Sorted by file, line, rule; de-duplicated.
    pub findings: Vec<Finding>,
    /// One line each: skipped layers or rules, configuration problems, evaluation failures.
    pub notes: Vec<String>,
    /// Number of files checked.
    pub files: usize,
    /// Layers checked (the build pseudo-layer included), in workspace order.
    pub layers: Vec<String>,
}

/// Lints `target` with every rule of the catalogue and the real evaluator.
pub fn run(
    ws: &Workspace,
    index: &Index,
    config: &Config,
    target: &Target,
    options: &Options,
) -> Result<Report> {
    run_with(ws, index, config, target, options, all_rules(), &Replay)
}

/// [`run`] with the rule set and the evaluator given (tests).
pub fn run_with(
    ws: &Workspace,
    index: &Index,
    config: &Config,
    target: &Target,
    options: &Options,
    rules: &[&dyn Rule],
    eval: &dyn Evaluate,
) -> Result<Report> {
    let mut report = Report::default();
    let selected = select_rules(config, options, rules, &mut report.notes)?;
    let overrides = severity_overrides(config, rules, &mut report.notes);
    let plan = plan(ws, index, target, options, &mut report.notes)?;
    report.files = plan.files.len();
    report.layers = plan.layers_checked.clone();

    let releases_of = ReleaseCache::new(ws);
    let mut raw: Vec<Finding> = Vec::new();
    let mut sources = SourceCache::default();
    let mut eval_unavailable = false;
    let recipe_rules: Vec<&dyn Rule> = selected
        .iter()
        .copied()
        .filter(|r| r.scope() == Scope::Recipe)
        .collect();

    for abs in &plan.files {
        let Some(loc0) = ws.loc_for(abs, LineSpan::line(1)) else {
            report.notes.push(format!(
                "skipped {}: not inside a layer or the build directory",
                abs.display()
            ));
            continue;
        };
        let key = (loc0.layer.clone(), loc0.path.clone());
        let entry = match sources.load(ws, &key, Some(abs)) {
            Ok(e) => e,
            Err(e) => {
                report
                    .notes
                    .push(format!("skipped {}:{}: {e}", key.0, key.1.display()));
                continue;
            }
        };
        let releases = releases_of.get(&key.0);
        let kind = FileKind::from_path(abs);
        let loc = Loc {
            lines: LineSpan::new(1, entry.lines),
            ..loc0
        };
        let cx = LintContext {
            file: Some(&entry.parsed),
            source: Some(&entry.text),
            kind,
            data: None,
            layer: None,
            index,
            ws,
            loc: loc.clone(),
            releases: &releases,
        };
        for r in selected.iter().filter(|r| r.scope() == Scope::File) {
            if applies(*r, &releases) {
                r.check(&cx, &mut raw);
            }
        }
        if kind != Some(FileKind::Recipe) || recipe_rules.is_empty() {
            continue;
        }
        let rules_here: Vec<&dyn Rule> = recipe_rules
            .iter()
            .copied()
            .filter(|r| applies(*r, &releases))
            .collect();
        if rules_here.is_empty() || eval_unavailable {
            continue;
        }
        let file_name = abs.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let (pn, pv) = crate::index::scan::pn_pv(FileKind::Recipe, file_name);
        let target = crate::eval::Target {
            recipe: pn.clone(),
            version: pv,
            machine: None,
            distro: None,
        };
        match eval.evaluate(index, ws, &target) {
            Ok(Some(data)) => {
                let cx = LintContext {
                    data: Some(&data),
                    ..cx
                };
                for r in &rules_here {
                    r.check(&cx, &mut raw);
                }
            }
            Ok(None) => eval_unavailable = true,
            Err(e) => report.notes.push(format!(
                "recipe {} ({}): evaluation failed, recipe-scope rules skipped: {e}",
                pn.unwrap_or_default(),
                loc
            )),
        }
    }
    if eval_unavailable {
        let ids: Vec<&str> = recipe_rules.iter().map(|r| r.id()).collect();
        report.notes.push(format!(
            "recipe-scope rules skipped, the evaluator is not available in this build: {}",
            ids.join(", ")
        ));
    }

    for layer in &plan.layer_units {
        let releases = releases_of.get(layer);
        let cx = LintContext {
            file: None,
            source: None,
            kind: None,
            data: None,
            layer: Some(layer),
            index,
            ws,
            loc: Loc {
                layer: layer.clone(),
                path: PathBuf::from("conf/layer.conf"),
                lines: LineSpan::line(1),
            },
            releases: &releases,
        };
        for r in selected.iter().filter(|r| r.scope() == Scope::Layer) {
            if applies(*r, &releases) {
                r.check(&cx, &mut raw);
            }
        }
    }

    let mut findings = Vec::with_capacity(raw.len());
    for mut f in raw {
        if let Some(only) = &plan.only_files
            && !only.contains(&(f.loc.layer.clone(), f.loc.path.clone()))
        {
            continue;
        }
        if let Some(span) = plan.lines
            && (f.loc.lines.end < span.start || f.loc.lines.start > span.end)
        {
            continue;
        }
        if let Some(s) = overrides.get(f.rule) {
            f.severity = *s;
        }
        if options.min_severity.is_some_and(|m| f.severity < m) {
            continue;
        }
        if suppressed(ws, &mut sources, &f) {
            continue;
        }
        findings.push(f);
    }
    sort_findings(&mut findings);
    report.findings = findings;
    Ok(report)
}

/// Whether a rule's release range contains one of the unit's releases.
pub fn applies(rule: &dyn Rule, releases: &[Release]) -> bool {
    let range = rule.releases();
    releases.iter().any(|r| range.contains(r))
}

/// Runs one file-scope rule on `text` as the engine would for a file at `loc` checked
/// against `releases` (empty: the workspace release), and returns its findings sorted. No
/// configuration, suppression or severity filter applies. For rule fixture tests: the file
/// need not be inside the workspace.
#[allow(clippy::too_many_arguments)]
pub fn check_text(
    rule: &dyn Rule,
    ws: &Workspace,
    index: &Index,
    loc: Loc,
    text: &str,
    kind: Option<FileKind>,
    releases: &[Release],
    data: Option<&DataStore>,
) -> Vec<Finding> {
    let default = [ws.release.release];
    let releases = if releases.is_empty() {
        &default[..]
    } else {
        releases
    };
    let mut out = Vec::new();
    if !applies(rule, releases) {
        return out;
    }
    let parsed = crate::parse::parse(text);
    let lines = u32::try_from(text.lines().count().max(1)).unwrap_or(u32::MAX);
    let cx = LintContext {
        file: Some(&parsed),
        source: Some(text),
        kind,
        data,
        layer: None,
        index,
        ws,
        loc: Loc {
            lines: LineSpan::new(1, lines),
            ..loc
        },
        releases,
    };
    rule.check(&cx, &mut out);
    sort_findings(&mut out);
    out
}

/// Sorts by file (layer, path), line, rule, message, highest severity first, and drops
/// exact duplicates (same rule, pointer and message).
pub fn sort_findings(findings: &mut Vec<Finding>) {
    findings.sort_by(|a, b| {
        (
            &a.loc.layer,
            &a.loc.path,
            a.loc.lines.start,
            a.loc.lines.end,
        )
            .cmp(&(
                &b.loc.layer,
                &b.loc.path,
                b.loc.lines.start,
                b.loc.lines.end,
            ))
            .then_with(|| a.rule.cmp(b.rule))
            .then_with(|| a.message.cmp(&b.message))
            .then_with(|| b.severity.cmp(&a.severity))
    });
    findings.dedup_by(|b, a| a.rule == b.rule && a.loc == b.loc && a.message == b.message);
}

fn select_rules<'r>(
    config: &Config,
    options: &Options,
    rules: &[&'r dyn Rule],
    notes: &mut Vec<String>,
) -> Result<Vec<&'r dyn Rule>> {
    for d in &config.lint.disable {
        if !entry_known(d, rules) {
            notes.push(format!(
                "sous-chef.toml: [lint] disable names {d:?}, which is not a rule id or group; \
                 ignored (groups: {})",
                GROUPS.join(", ")
            ));
        }
    }
    let unknown: Vec<&String> = options
        .rules
        .iter()
        .filter(|e| !entry_known(e, rules))
        .collect();
    if !unknown.is_empty() {
        let names: Vec<String> = unknown.iter().map(|u| format!("{u:?}")).collect();
        return Err(Error::Invalid {
            msg: format!(
                "unknown rule id or group {}; groups: {}; ids are listed in sous-chef://rules",
                names.join(", "),
                GROUPS.join(", ")
            ),
        });
    }
    Ok(rules
        .iter()
        .copied()
        .filter(|r| {
            let id = r.id();
            if options.rules.is_empty() {
                return r.default_enabled() && !is_disabled(config, id);
            }
            if options.rules.iter().any(|e| names_exactly(e, id)) {
                return true;
            }
            options.rules.iter().any(|e| entry_matches(e, id))
                && r.default_enabled()
                && !is_disabled(config, id)
        })
        .collect())
}

/// `[lint.severity]` resolved per rule id: an entry for the id (or its alias) wins over one
/// for its group.
fn severity_overrides(
    config: &Config,
    rules: &[&dyn Rule],
    notes: &mut Vec<String>,
) -> HashMap<&'static str, Severity> {
    for k in config.lint.severity.keys() {
        if !entry_known(k, rules) {
            notes.push(format!(
                "sous-chef.toml: [lint.severity] names {k:?}, which is not a rule id or group; \
                 ignored"
            ));
        }
    }
    let mut out = HashMap::new();
    for r in rules {
        let id = r.id();
        let exact = config
            .lint
            .severity
            .iter()
            .find(|(k, _)| names_exactly(k, id));
        let any = exact.or_else(|| {
            config
                .lint
                .severity
                .iter()
                .find(|(k, _)| entry_matches(k, id))
        });
        if let Some((_, s)) = any {
            out.insert(id, *s);
        }
    }
    out
}

/// The effective severity of a rule under `config` (`[lint.severity]`, else its default).
pub fn effective_severity(config: &Config, rule: &dyn Rule) -> Severity {
    let mut ignored = Vec::new();
    severity_overrides(config, &[rule], &mut ignored)
        .get(rule.id())
        .copied()
        .unwrap_or(rule.default_severity())
}

struct Plan {
    files: Vec<PathBuf>,
    layer_units: Vec<String>,
    layers_checked: Vec<String>,
    /// Keep only findings in these files (recipe and file targets).
    only_files: Option<HashSet<(String, PathBuf)>>,
    lines: Option<LineSpan>,
}

fn plan(
    ws: &Workspace,
    index: &Index,
    target: &Target,
    options: &Options,
    notes: &mut Vec<String>,
) -> Result<Plan> {
    let real_layer = |name: &str| ws.layer(name).is_some();
    match target {
        Target::Workspace => {
            let any_writable = ws.layers.iter().any(|l| l.writable);
            let (take, skip): (Vec<&Layer>, Vec<&Layer>) = ws
                .layers
                .iter()
                .partition(|l| options.all || !any_writable || l.writable);
            if !skip.is_empty() {
                let names: Vec<&str> = skip.iter().map(|l| l.name.as_str()).collect();
                notes.push(format!(
                    "not checked: {} {} that {} not writable ({}); name one as the target or \
                     pass --all",
                    names.len(),
                    if names.len() == 1 { "layer" } else { "layers" },
                    if names.len() == 1 { "is" } else { "are" },
                    names.join(", ")
                ));
            }
            let mut layers: Vec<String> = take.iter().map(|l| l.name.clone()).collect();
            let layer_units = layers.clone();
            if ws.build_dir.is_some() {
                layers.push(BUILD_LAYER.to_string());
            }
            let mut files = Vec::new();
            for l in &layers {
                files.extend(layer_files(ws, index, l)?);
            }
            Ok(Plan {
                files,
                layer_units,
                layers_checked: layers,
                only_files: None,
                lines: None,
            })
        }
        Target::Layer(name) => {
            let is_build = name == BUILD_LAYER && ws.build_dir.is_some();
            if !real_layer(name) && !is_build {
                return Err(Error::NotFound {
                    what: format!("layer {name}"),
                });
            }
            Ok(Plan {
                files: layer_files(ws, index, name)?,
                layer_units: if is_build && !real_layer(name) {
                    Vec::new()
                } else {
                    vec![name.clone()]
                },
                layers_checked: vec![name.clone()],
                only_files: None,
                lines: None,
            })
        }
        Target::Recipe { name, version } => {
            let Some(recipe) = query::preferred_recipe(index, name, version.as_deref())? else {
                return Err(Error::NotFound {
                    what: match version {
                        Some(v) => format!("recipe {name} version {v}"),
                        None => format!("recipe {name}"),
                    },
                });
            };
            let files = recipe_files(ws, index, &recipe)?;
            Ok(files_plan(ws, files, None))
        }
        Target::Files { files, lines } => Ok(files_plan(ws, files.clone(), *lines)),
    }
}

fn files_plan(ws: &Workspace, files: Vec<PathBuf>, lines: Option<LineSpan>) -> Plan {
    let mut only = HashSet::new();
    let mut layers: Vec<String> = Vec::new();
    for f in &files {
        if let Some(loc) = ws.loc_for(f, LineSpan::line(1)) {
            if !layers.contains(&loc.layer) {
                layers.push(loc.layer.clone());
            }
            only.insert((loc.layer, loc.path));
        }
    }
    // Workspace order.
    let order = target_layers_all(ws);
    layers.sort_by_key(|l| order.iter().position(|o| o == l).unwrap_or(usize::MAX));
    let layer_units = layers
        .iter()
        .filter(|l| ws.layer(l).is_some())
        .cloned()
        .collect();
    Plan {
        files,
        layer_units,
        layers_checked: layers,
        only_files: Some(only),
        lines,
    }
}

struct ReleaseCache<'a> {
    ws: &'a Workspace,
}

impl<'a> ReleaseCache<'a> {
    fn new(ws: &'a Workspace) -> Self {
        ReleaseCache { ws }
    }

    /// The releases a layer is linted against; the workspace release alone for the build
    /// directory or anything that is not a layer.
    fn get(&self, layer: &str) -> Vec<Release> {
        match self.ws.layer(layer) {
            Some(l) => l.lint_releases(self.ws),
            None => vec![self.ws.release.release],
        }
    }
}

/// The releases a pointer's layer is checked against (see [`LintContext::releases`]).
pub fn releases_for(ws: &Workspace, layer: &str) -> Vec<Release> {
    ReleaseCache::new(ws).get(layer)
}

struct Source {
    text: String,
    parsed: ParsedFile,
    lines: u32,
}

#[derive(Default)]
struct SourceCache {
    files: HashMap<(String, PathBuf), std::result::Result<Source, String>>,
}

impl SourceCache {
    /// The text and parse of a file, read once. `abs` is used when given, else the pointer is
    /// resolved. Every read goes through [`Workspace::confine`].
    fn load(
        &mut self,
        ws: &Workspace,
        key: &(String, PathBuf),
        abs: Option<&Path>,
    ) -> std::result::Result<&Source, String> {
        if !self.files.contains_key(key) {
            let loaded = read_source(ws, key, abs).map_err(|e| e.to_string());
            self.files.insert(key.clone(), loaded);
        }
        match self.files.get(key) {
            Some(Ok(s)) => Ok(s),
            Some(Err(e)) => Err(e.clone()),
            None => Err("not loaded".into()),
        }
    }
}

fn read_source(ws: &Workspace, key: &(String, PathBuf), abs: Option<&Path>) -> Result<Source> {
    let path = match abs {
        Some(p) => p.to_path_buf(),
        None => ws
            .abs_path(&Loc {
                layer: key.0.clone(),
                path: key.1.clone(),
                lines: LineSpan::line(1),
            })
            .ok_or_else(|| Error::NotFound {
                what: format!("file {}:{}", key.0, key.1.display()),
            })?,
    };
    let path = ws.confine(&path)?;
    let bytes = fs::read(&path)?;
    let text = lexer::decode(&bytes).into_owned();
    let parsed = crate::parse::parse(&text);
    let lines = u32::try_from(text.lines().count().max(1)).unwrap_or(u32::MAX);
    Ok(Source {
        text,
        parsed,
        lines,
    })
}

/// The ids and groups a `# sous-chef: allow ...` comment line names; `None` if the line is
/// not such a comment.
pub fn allow_comment(line: &str) -> Option<Vec<&str>> {
    let rest = line.trim().strip_prefix('#')?.trim_start();
    let rest = rest.strip_prefix("sous-chef:")?.trim_start();
    let rest = rest.strip_prefix("allow")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some(
        rest.split(|c: char| c.is_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

fn suppressed(ws: &Workspace, sources: &mut SourceCache, f: &Finding) -> bool {
    let key = (f.loc.layer.clone(), f.loc.path.clone());
    let Ok(src) = sources.load(ws, &key, None) else {
        return false;
    };
    let lines: Vec<&str> = src.text.lines().collect();
    let line = f.loc.lines.start;
    let mut anchors = vec![line];
    if let Some(stmt) = src
        .parsed
        .statements
        .iter()
        .find(|s| s.span.start <= line && line <= s.span.end)
    {
        anchors.push(stmt.span.start);
    }
    anchors.iter().any(|&a| {
        // Walk up the block of comment lines directly above line `a` (1-based).
        let mut n = a as usize;
        while n >= 2 {
            let Some(text) = lines.get(n - 2) else { break };
            if !text.trim_start().starts_with('#') {
                break;
            }
            if allow_comment(text).is_some_and(|ids| ids.iter().any(|e| entry_matches(e, f.rule))) {
                return true;
            }
            n -= 1;
        }
        false
    })
}

// ---------------------------------------------------------------------------------------
// Rendering, exit code, CLI

/// Exit status for findings: 1 if any is at or above `fail_on`, else 0.
pub fn exit_code(findings: &[Finding], fail_on: Severity) -> i32 {
    i32::from(findings.iter().any(|f| f.severity >= fail_on))
}

/// The summary line: counts by severity and by rule (most findings first).
pub fn summary_line(report: &Report) -> String {
    let n = report.findings.len();
    let files = format!(
        "{} {}",
        report.files,
        if report.files == 1 { "file" } else { "files" }
    );
    if n == 0 {
        return format!("no findings in {files}");
    }
    let count = |s: Severity| report.findings.iter().filter(|f| f.severity == s).count();
    let mut by_rule: BTreeMap<&str, usize> = BTreeMap::new();
    for f in &report.findings {
        *by_rule.entry(f.rule).or_default() += 1;
    }
    let mut rules: Vec<(&str, usize)> = by_rule.into_iter().collect();
    rules.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let rules: Vec<String> = rules.iter().map(|(r, c)| format!("{r} {c}")).collect();
    format!(
        "{n} {} in {files}: {} error, {} warning, {} info; by rule: {}",
        if n == 1 { "finding" } else { "findings" },
        count(Severity::Error),
        count(Severity::Warning),
        count(Severity::Info),
        rules.join(", ")
    )
}

/// JSON output of a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonReport {
    pub summary: String,
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
    /// Every finding counted by rule, also those cut by the budget.
    pub by_rule: BTreeMap<String, usize>,
    pub findings: render::JsonTable,
}

/// Renders a report: the summary line first, then the table `severity rule pointer message`
/// within what is left of the budget, then the notes. JSON carries the same table (the rows
/// the text layout keeps) with the counts.
pub fn render_report(report: &Report, format: Format, budget_tokens: usize) -> String {
    let summary = summary_line(report);
    let mut table = Table::new(&["severity", "rule", "pointer", "message"]);
    for f in &report.findings {
        table.row(&[
            f.severity.as_str(),
            f.rule,
            &f.loc.to_string(),
            &render::escape_cell(&f.message),
        ]);
    }
    for n in &report.notes {
        table.note(&format!("note: {n}"));
    }
    let budget = budget_tokens.saturating_sub(render::estimate_tokens(&summary) + 1);
    match format {
        Format::Text => format!("{summary}\n{}", table.render(budget)),
        Format::Json => {
            let count = |s: Severity| report.findings.iter().filter(|f| f.severity == s).count();
            let mut by_rule = BTreeMap::new();
            for f in &report.findings {
                *by_rule.entry(f.rule.to_string()).or_default() += 1;
            }
            let out = JsonReport {
                summary,
                errors: count(Severity::Error),
                warnings: count(Severity::Warning),
                infos: count(Severity::Info),
                by_rule,
                findings: table.to_json(budget),
            };
            serde_json::to_string(&out).unwrap_or_else(|_| "{}".into())
        }
    }
}

/// `sous-chef lint` arguments. Flattened into the CLI by `main.rs`; T4.1 may add flags here
/// without touching `main.rs`.
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct LintCliArgs {
    /// A recipe, a file pointer, a layer name or a path; default: the whole workspace.
    pub target: Option<String>,
    /// Rule ids or groups to run (repeatable).
    #[arg(long = "rule", value_name = "ID")]
    pub rules: Vec<String>,
    /// Minimum severity to report.
    #[arg(long, value_enum)]
    pub severity: Option<Severity>,
    /// Output format.
    #[arg(long, value_enum)]
    pub format: Option<Format>,
    /// Exit with status 1 if any finding is at or above this severity (default: error).
    #[arg(long, value_enum)]
    pub fail_on: Option<Severity>,
    /// Also lint layers that are not writable.
    #[arg(long)]
    pub all: bool,
}

/// Runs `sous-chef lint`, printing the report to stdout. Returns the process exit code: 1 if
/// a reported finding is at or above `--fail-on` (default `error`), else 0. An error (bad
/// target, unknown rule id, unreadable workspace) is returned; `main.rs` exits 2 on it.
pub fn cli(cfg: &Config, args: &LintCliArgs) -> Result<i32> {
    let ws = Workspace::discover(cfg)?;
    if let Err(e) = cfg.validate(&ws) {
        eprintln!("sous-chef: configuration problem: {e}");
    }
    let mut index = Index::open(&ws)?;
    index.refresh(&ws)?;
    let mut stdout = std::io::stdout().lock();
    let code = cli_with(&ws, &index, cfg, args, &mut stdout)?;
    stdout.flush()?;
    Ok(code)
}

/// [`cli`] on an open, refreshed index, writing to `out` (tests).
pub fn cli_with(
    ws: &Workspace,
    index: &Index,
    cfg: &Config,
    args: &LintCliArgs,
    out: &mut dyn Write,
) -> Result<i32> {
    let target = resolve_target(ws, index, args.target.as_deref(), &cfg.work_dir)?;
    let options = Options {
        rules: args.rules.clone(),
        min_severity: args.severity,
        all: args.all,
    };
    let report = run(ws, index, cfg, &target, &options)?;
    let text = render_report(&report, args.format.unwrap_or_default(), usize::MAX);
    out.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        out.write_all(b"\n")?;
    }
    Ok(exit_code(
        &report.findings,
        args.fail_on.unwrap_or(Severity::Error),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_ids_are_unique_and_grouped() {
        let mut ids: Vec<_> = all_rules().iter().map(|r| r.id()).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate rule id");
        for id in ids {
            let group = id.split('/').next().unwrap_or("");
            assert!(GROUPS.contains(&group), "{id} has unknown group");
        }
    }

    #[test]
    fn entries_match_ids_groups_and_aliases() {
        assert!(entry_matches("org", "org/recipe-path"));
        assert!(entry_matches("org/", "org/recipe-path"));
        assert!(entry_matches("org/recipe-path", "org/recipe-path"));
        assert!(!entry_matches("org/recipe", "org/recipe-path"));
        assert!(!entry_matches("hygiene", "org/recipe-path"));
        assert!(entry_matches(
            "style/variable-order",
            "hygiene/variable-order"
        ));
        assert!(entry_matches("style", "hygiene/variable-order"));
        assert!(!entry_matches("style", "hygiene/tabs"));
        assert!(names_exactly(
            "style/variable-order",
            "hygiene/variable-order"
        ));
        assert!(!names_exactly("hygiene", "hygiene/tabs"));
    }

    #[test]
    fn allow_comments() {
        assert_eq!(
            allow_comment("# sous-chef: allow hygiene/tabs"),
            Some(vec!["hygiene/tabs"])
        );
        assert_eq!(
            allow_comment("  #sous-chef: allow org, hygiene/tabs"),
            Some(vec!["org", "hygiene/tabs"])
        );
        assert_eq!(allow_comment("# sous-chef: allowed x"), None);
        assert_eq!(allow_comment("# nothing"), None);
        assert_eq!(allow_comment("FOO = \"# sous-chef: allow x\""), None);
    }

    #[test]
    fn exit_codes() {
        let f = |s| Finding {
            rule: "hygiene/parse-error",
            severity: s,
            loc: "l:p:1".parse().unwrap(),
            message: String::new(),
        };
        assert_eq!(exit_code(&[], Severity::Error), 0);
        assert_eq!(exit_code(&[f(Severity::Warning)], Severity::Error), 0);
        assert_eq!(exit_code(&[f(Severity::Warning)], Severity::Warning), 1);
        assert_eq!(exit_code(&[f(Severity::Error)], Severity::Info), 1);
    }
}

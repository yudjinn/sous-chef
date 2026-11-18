//! Tool `overrides`: the override chain and override-conditional statements. See docs/mcp-tools.md.
//!
//! - `chain`: the entries of `OVERRIDES`, lowest priority first, each with the variables it
//!   came through (`OVERRIDES <- MACHINEOVERRIDES <- MACHINE`) and the pointer of the
//!   deepest assignment that contributes it (`MACHINEOVERRIDES =. "socx:"` in the machine
//!   conf for `socx`). Entries are attributed by expanding the applied assignments of each
//!   variable piece by piece; a piece that is python is unknown and said so.
//! - `conditional`: every operation whose name carries an override condition, in parse
//!   order, `active` or `inactive` for this machine and distro. With a recipe: the
//!   operations from the recipe's own files (recipe, includes, classes, bbappends) plus
//!   `.conf` operations conditional on `pn-<pn>`; without: the configuration's operations.
//!   Package-name overrides (`FILES:${PN}-dev`, `RDEPENDS:libfoo-dev`) are not conditions
//!   and are left out.
//! - `dead`: the conditional operations with an override element that matches no known
//!   override, by the definition of the lint rule `override/dead` (`Known` here mirrors the
//!   rule's: the evaluated `OVERRIDES`, `lint::helpers::KnownOverrides`, class stems, words of
//!   every `*OVERRIDES` assignment, python included), and not a package of the recipe. Each is
//!   reported with the closest known name by edit distance. Operations that are only
//!   inactive for this machine, on variables BitBake reads by name (`TUNE_FEATURES:tune-*`,
//!   [`ReadByName`]) or with a possible package in the package position are counted in
//!   notes, not listed.
//!
//! `OVERRIDES` and `PACKAGES` are split with `parse::words`: an entry that holds `${@...}` is
//! one opaque entry, never an override name or a package.

use std::collections::{BTreeSet, HashMap};

use schemars::JsonSchema;
use serde::Deserialize;

use super::var::{mc_notes, op_snippet, recipe_target_in, replay_notes};
use crate::error::Result;
use crate::eval::{self, DataStore, OpStatus, Target, VarOp};
use crate::index::{Index, query};
use crate::lint::helpers::{self, KnownOverrides};
use crate::mcp::Ctx;
use crate::parse::words;
use crate::render::{self, Format, Table};
use crate::types::{FileKind, Loc};

/// What to show.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Show {
    /// `OVERRIDES` entries, lowest to highest priority, with where each comes from.
    #[default]
    Chain,
    /// Every override-conditional statement in the recipe tree, active or inactive.
    Conditional,
    /// Statements whose override matches nothing known (likely typos).
    Dead,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Recipe; omit for global configuration.
    pub recipe: Option<String>,
    /// Default: `MACHINE` from local.conf.
    pub machine: Option<String>,
    /// Default: `DISTRO` from local.conf.
    pub distro: Option<String>,
    /// Multiconfig to evaluate in (default: the default configuration); a recipe written
    /// `mc:<name>:<recipe>` also selects it.
    pub mc: Option<String>,
    /// `chain` (default), `conditional` or `dead`.
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
    let target = recipe_target_in(
        &index,
        args.recipe.as_deref(),
        args.machine.as_deref(),
        args.distro.as_deref(),
        args.mc.as_deref(),
    )?;
    let ds = eval::evaluate(&index, &ctx.workspace, &target)?;
    let mut t = match args.show {
        Show::Chain => chain(&ds, &target),
        Show::Conditional => conditional(&ds, &target),
        Show::Dead => dead(&index, &ds, &target)?,
    };
    drop(index);
    if let Some(v) = ds.overrides_value()
        && (v.unevaluated_python || !v.unresolved.is_empty())
    {
        let mut what = Vec::new();
        if v.unevaluated_python {
            what.push("python".to_string());
        }
        if !v.unresolved.is_empty() {
            what.push(format!("unresolved {}", v.unresolved.join(", ")));
        }
        t.note(&format!(
            "warning: OVERRIDES contains {}; its real entries are not all known, so active/inactive may be wrong; use authoritative=true with var",
            what.join(" and ")
        ));
    }
    for n in super::var::python_eval_notes(&ds.python_log("OVERRIDES")) {
        t.note(&n.replacen("note: python", "note: OVERRIDES: python", 1));
    }
    for n in replay_notes(&ds, "OVERRIDES") {
        if n.starts_with("warning: anonymous python") && n.contains("computed names") {
            t.note(&n);
        }
    }
    for n in mc_notes(&ds) {
        t.note(&n);
    }
    if target.recipe.is_none() {
        t.note("note: global configuration only (no recipe)");
    }
    if let Some(line) = render::freshness_line(&report) {
        t.note(&line);
    }
    let budget = args
        .budget_tokens
        .unwrap_or(ctx.config.output.budget_tokens);
    Ok(t.render_as(args.format.unwrap_or_default(), budget))
}

// ---- chain ----

/// Where one entry came from: the variables it went through, outermost first, each with the
/// assignment that contributed it (`None` for a value the evaluator set itself, such as
/// `MACHINE` from the `machine` argument or `PN` from the file name).
type Chain = Vec<(String, Option<Loc>)>;

fn chain(ds: &DataStore, target: &Target) -> Table {
    let mut t = Table::new(&["priority", "entry", "from", "loc"]);
    let mut map: HashMap<String, Chain> = HashMap::new();
    let mut python = false;
    attribute_var(ds, "OVERRIDES", &mut Vec::new(), &mut map, &mut python);
    let mut unevaluated = 0;
    for (i, (e, opaque)) in ds.override_entries().iter().enumerate() {
        if *opaque {
            // One entry holding python (or an undefined variable): shown once, as what it
            // is, never split into fragments and never an override name.
            unevaluated += 1;
            let what = if eval::expand::has_python(e) {
                "${@...} (python, not evaluated): stands for entries that are not known"
            } else {
                "${...} (undefined variable, left as written)"
            };
            t.row(&[&i.to_string(), &words::abbreviate(e), what, "-"]);
            continue;
        }
        let (from, loc) = match map.get(e) {
            Some(c) => {
                let names: Vec<&str> = c.iter().map(|(n, _)| n.as_str()).collect();
                let loc = c
                    .iter()
                    .rev()
                    .find_map(|(_, l)| l.clone())
                    .map_or("-".to_string(), |l| l.to_string());
                let mut from = names.join(" <- ");
                if let Some((last, None)) = c.last() {
                    from.push_str(&format!(" ({})", set_by_evaluator(last, target)));
                }
                (from, loc)
            }
            None => ("?".into(), "-".into()),
        };
        t.row(&[&i.to_string(), e, &from, &loc]);
    }
    if python || unevaluated > 0 {
        t.note(&format!(
            "note: part of OVERRIDES comes from python, which is not evaluated; {unevaluated} {} it, and the override names it stands for are not known",
            if unevaluated == 1 {
                "entry holds"
            } else {
                "entries hold"
            }
        ));
    }
    t.note("note: entries to the right (higher priority) win; loc is the deepest assignment that contributes the entry");
    t
}

fn set_by_evaluator(name: &str, target: &Target) -> &'static str {
    match name {
        "MACHINE" if target.machine.is_some() => "machine argument",
        "DISTRO" if target.distro.is_some() => "distro argument",
        _ => "set by the evaluator",
    }
}

/// Attributes the entries of `var`'s value: each applied assignment is split into pieces
/// (`${X}` references and literal text), expanded piece by piece, and every `:`-separated
/// entry is credited to the piece its first character came from. A reference is followed
/// into the referenced variable's own assignments.
fn attribute_var(
    ds: &DataStore,
    var: &str,
    stack: &mut Vec<(String, Option<Loc>)>,
    map: &mut HashMap<String, Chain>,
    python: &mut bool,
) {
    if stack.iter().any(|(n, _)| n == var) || stack.len() > 16 {
        return;
    }
    let applied: Vec<VarOp> = ds
        .history(var)
        .into_iter()
        .filter(|(op, st)| *st == OpStatus::Applied && op.var.flag.is_none())
        .map(|(op, _)| op)
        .collect();
    if applied.is_empty() {
        // Set without an assignment of its own (target machine, PN from the file name).
        if let Some(v) = ds.value(var) {
            stack.push((var.to_string(), None));
            for e in words::colon_parts(&v.text)
                .into_iter()
                .filter(|e| !e.is_empty() && !words::is_unevaluated(e))
            {
                map.entry(e.to_string()).or_insert_with(|| stack.clone());
            }
            stack.pop();
        }
        return;
    }
    // Latest assignment first: it is the one whose text survives for a plain `=`.
    for op in applied.iter().rev() {
        stack.push((var.to_string(), Some(op.loc.clone())));
        attribute_text(ds, &op.value, stack, map, python);
        stack.pop();
    }
}

fn attribute_text(
    ds: &DataStore,
    text: &str,
    stack: &mut Vec<(String, Option<Loc>)>,
    map: &mut HashMap<String, Chain>,
    python: &mut bool,
) {
    // Expanded pieces with their origin: `Some(var)` for a `${var}` reference.
    let mut expanded = String::new();
    let mut origin: Vec<Option<String>> = Vec::new();
    for piece in pieces(text) {
        let (out, from) = match &piece {
            Piece::Lit(s) => (s.clone(), None),
            Piece::Ref(n) if n.starts_with('@') => {
                // Python the evaluator computes statically (a pure helper reading certain
                // values) contributes its text, credited to the assignment.
                let v = ds.expand(&format!("${{{n}}}"));
                if v.unevaluated_python || !v.unresolved.is_empty() {
                    *python = true;
                    (String::new(), None)
                } else {
                    (v.text, None)
                }
            }
            Piece::Ref(n) => {
                let v = ds.expand(&format!("${{{n}}}"));
                if v.unevaluated_python {
                    *python = true;
                }
                (v.text, Some(n.clone()))
            }
        };
        for _ in out.bytes() {
            origin.push(from.clone());
        }
        expanded.push_str(&out);
    }
    // Entries split as OVERRIDES is split, a `${...}` group kept whole; an entry holding
    // one is not an override name and is not attributed.
    let base = expanded.as_ptr() as usize;
    for part in words::colon_parts(&expanded) {
        let start = part.as_ptr() as usize - base;
        if !part.is_empty() && !words::is_unevaluated(part) {
            let entry = part.to_string();
            match origin.get(start).cloned().flatten() {
                Some(var) if !map.contains_key(&entry) => {
                    let mut sub = HashMap::new();
                    attribute_var(ds, &var, stack, &mut sub, python);
                    match sub.remove(&entry) {
                        Some(c) => {
                            map.insert(entry.clone(), c);
                        }
                        None => {
                            // A reference inside a larger word (`pn-${PN}`) or a value the
                            // evaluator computed: credit the reference itself.
                            let mut c = stack.clone();
                            c.push((var.clone(), None));
                            map.insert(entry.clone(), c);
                        }
                    }
                    // Keep what else the referenced variable contributes.
                    for (k, v) in sub {
                        map.entry(k).or_insert(v);
                    }
                }
                Some(_) => {}
                None => {
                    map.entry(entry).or_insert_with(|| stack.clone());
                }
            }
        }
    }
}

enum Piece {
    Lit(String),
    /// The text between `${` and the matching `}`.
    Ref(String),
}

fn pieces(text: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let b: Vec<char> = text.chars().collect();
    let mut lit = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '$' && b.get(i + 1) == Some(&'{') {
            let mut depth = 1;
            let mut j = i + 2;
            while j < b.len() && depth > 0 {
                match b[j] {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            if depth == 0 {
                if !lit.is_empty() {
                    out.push(Piece::Lit(std::mem::take(&mut lit)));
                }
                out.push(Piece::Ref(b[i + 2..j - 1].iter().collect()));
                i = j;
                continue;
            }
        }
        lit.push(b[i]);
        i += 1;
    }
    if !lit.is_empty() {
        out.push(Piece::Lit(lit));
    }
    out
}

// ---- conditional and dead ----

/// An operation with an override condition, in parse order.
struct Cond {
    op: VarOp,
    status: OpStatus,
    /// The condition elements (overrides minus `append` / `prepend` / `remove` and a
    /// leading package name), expanded.
    elements: Vec<String>,
    /// The first element stands where a package name would (`RDEPENDS:<x>`), and
    /// `PACKAGES` holds python, so it may be a package python adds.
    maybe_package: Option<String>,
}

const KEYWORDS: [&str; 3] = ["append", "prepend", "remove"];

/// Every conditional operation in scope (see the module documentation).
fn conditionals(ds: &DataStore, target: &Target) -> Vec<Cond> {
    let pn = ds.value("PN").map(|v| v.text);
    let packages: BTreeSet<String> = packages(ds);
    let pkgs_uncertain = packages_uncertain(ds);
    // The recipe's own tree: the files read from the recipe file on (the recipe pass runs
    // on a fork of the configuration, so the configuration's visits come first; an `.inc`
    // the configuration includes, such as a tune file, is not the recipe's).
    let first_recipe = ds
        .files()
        .iter()
        .position(|v| v.kind == FileKind::Recipe)
        .unwrap_or(usize::MAX);
    let recipe_files: BTreeSet<(String, std::path::PathBuf)> = ds
        .files()
        .iter()
        .enumerate()
        .filter(|(i, v)| *i >= first_recipe && v.kind != FileKind::Conf)
        .map(|(_, v)| (v.loc.layer.clone(), v.loc.path.clone()))
        .collect();
    let mut bases: BTreeSet<String> = BTreeSet::new();
    for n in ds.names() {
        bases.insert(n.split(':').next().unwrap_or(&n).to_string());
    }
    let mut out: Vec<(usize, u32, Cond)> = Vec::new();
    for base in bases {
        for (op, status) in ds.history(&base) {
            if op.var.legacy_syntax || op.var.overrides.is_empty() {
                continue;
            }
            let full = ds.expanded_name(&op);
            let mut parts: Vec<String> = full.split(':').skip(1).map(str::to_string).collect();
            // A leading package name is not a condition (`FILES:hello-dev`).
            let lead_removed = parts
                .first()
                .is_some_and(|p| packages.contains(p) || p.contains("${"))
                && !KEYWORDS.contains(&parts[0].as_str());
            if lead_removed {
                parts.remove(0);
            }
            // Still in the package position: a package python adds to PACKAGES (or one at
            // build time, for a package-scoped variable) cannot be ruled out.
            let maybe_package = parts
                .first()
                .filter(|p| {
                    !lead_removed
                        && !KEYWORDS.contains(&p.as_str())
                        && (pkgs_uncertain || helpers::is_package_var(&op.var.base))
                })
                .cloned();
            let elements: Vec<String> = parts
                .into_iter()
                .filter(|p| !p.is_empty() && !KEYWORDS.contains(&p.as_str()))
                .flat_map(|p| p.split(':').map(str::to_string).collect::<Vec<_>>())
                .collect();
            if elements.is_empty() {
                continue;
            }
            let in_tree = recipe_files.contains(&(op.loc.layer.clone(), op.loc.path.clone()));
            let keep = match (&target.recipe, &pn) {
                (None, _) => true,
                (Some(_), Some(pn)) => in_tree || elements.iter().any(|e| *e == format!("pn-{pn}")),
                (Some(_), None) => in_tree,
            };
            if !keep {
                continue;
            }
            let (file, line) = position(ds, &op.loc);
            out.push((
                file,
                line,
                Cond {
                    op,
                    status,
                    elements,
                    maybe_package,
                },
            ));
        }
    }
    out.sort_by_key(|(f, l, _)| (*f, *l));
    out.into_iter().map(|(_, _, c)| c).collect()
}

fn position(ds: &DataStore, loc: &Loc) -> (usize, u32) {
    let file = ds
        .files()
        .iter()
        .position(|v| v.loc.layer == loc.layer && v.loc.path == loc.path)
        .unwrap_or(usize::MAX);
    (file, loc.lines.start)
}

/// Whether `PACKAGES` holds an unevaluated word (`${@bb.utils.contains('PTEST_ENABLED', '1',
/// '${PN}-ptest', '', d)}`): then some package names are not known.
fn packages_uncertain(ds: &DataStore) -> bool {
    ds.value("PACKAGES").is_some_and(|v| {
        words::words(&v.text)
            .iter()
            .any(|w| words::is_unevaluated(w))
    })
}

/// Package names of the recipe: the known words of `PACKAGES` plus `PN`.
fn packages(ds: &DataStore) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = ds
        .value("PACKAGES")
        .map(|v| {
            words::known_words(&v.text)
                .into_iter()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if let Some(pn) = ds.value("PN") {
        out.insert(pn.text);
    }
    out
}

fn conditional(ds: &DataStore, target: &Target) -> Table {
    let mut t = Table::new(&["state", "condition", "name", "loc", "snippet"]);
    let ovr: BTreeSet<String> = ds.overrides().into_iter().collect();
    let conds = conditionals(ds, target);
    let (mut active, mut inactive) = (0, 0);
    for c in &conds {
        let state = if c.status == OpStatus::InactiveOverride {
            inactive += 1;
            "inactive"
        } else {
            active += 1;
            "active"
        };
        let missing: Vec<&str> = c
            .elements
            .iter()
            .filter(|e| !ovr.contains(*e))
            .map(String::as_str)
            .collect();
        // For a chained condition, say which elements are missing.
        let cond = if c.elements.len() < 2 || missing.is_empty() || state == "active" {
            c.elements.join(":")
        } else {
            format!(
                "{} (not in OVERRIDES: {})",
                c.elements.join(":"),
                missing.join(", ")
            )
        };
        t.row(&[
            state,
            &cond,
            &eval::datastore::full_name(&c.op.var),
            &c.op.loc.to_string(),
            &op_snippet(ds, &c.op),
        ]);
    }
    t.note(&format!(
        "note: {active} active, {inactive} inactive; scope: {}",
        match &target.recipe {
            Some(_) => "the recipe's files, and .conf statements conditional on pn-<recipe>",
            None => "the configuration files",
        }
    ));
    t
}

/// Variables whose values feed `OVERRIDES` in OE-core: the same list as the lint rule
/// `override/dead` (`src/lint/rules/override_dead.rs`, `OVERRIDE_VARS`).
const OVERRIDE_VARS: &[&str] = &[
    "OVERRIDES",
    "MACHINEOVERRIDES",
    "MACHINEOVERRIDES_EXTENDER",
    "DISTROOVERRIDES",
    "CLASSOVERRIDE",
    "LIBCOVERRIDE",
    "TCOVERRIDE",
    "TUNEOVERRIDES",
    "FILESOVERRIDES",
];

/// Words of an `OVERRIDES`-like value, python included: runs of `[a-z0-9_+.-]` (the lint
/// rule's `tokens`; OE-core's tune files add `armv7a` to `MACHINEOVERRIDES` from python).
fn override_tokens(value: &str) -> impl Iterator<Item = String> + '_ {
    crate::lint::helpers::override_words(value).into_iter()
}

/// What an override element may legitimately be: the definition of the lint rule
/// `override/dead` (its `Known`): an entry of the evaluated `OVERRIDES`, a name
/// [`KnownOverrides::contains`] accepts, a class stem, or a word of any `*OVERRIDES`
/// assignment in the workspace (python included).
struct Known {
    index: KnownOverrides,
    active: BTreeSet<String>,
    words: BTreeSet<String>,
}

impl Known {
    fn load(index: &Index, ds: &DataStore) -> Result<Known> {
        let known = KnownOverrides::from_index(index)?;
        let active: BTreeSet<String> = ds.overrides().into_iter().collect();
        let mut words = BTreeSet::new();
        let filter = query::StatementFilter {
            kinds: vec!["assign".into()],
            ..query::StatementFilter::default()
        };
        for base in OVERRIDE_VARS {
            for s in query::statements_by_base(index, base, &filter)? {
                words.extend(override_tokens(s.value.as_deref().unwrap_or("")));
            }
            for (op, _) in ds.history(base) {
                words.extend(override_tokens(&op.value));
            }
        }
        Ok(Known {
            index: known,
            active,
            words,
        })
    }

    fn contains(&self, o: &str) -> bool {
        self.active.contains(o)
            || self.index.contains(o)
            || self.index.classes.contains(o)
            || self.words.contains(o)
    }

    fn names(&self) -> BTreeSet<String> {
        let mut n: BTreeSet<String> = BTreeSet::new();
        n.extend(self.index.machines.iter().cloned());
        n.extend(self.index.distros.iter().cloned());
        n.extend(self.index.declared.iter().cloned());
        n.extend(self.active.iter().cloned());
        n
    }
}

/// How BitBake reads `VAR:<o>` by name rather than as a condition: a value somewhere that
/// holds `VAR:` followed by literal text and then `${`, a quote, `%`, `)` or `+` (`${TUNE_FEATURES:tune-${DEFAULTTUNE}}`,
/// `d.getVar('BASE_LIB:tune-' + ...)`, `"QEMU_EXTRAOPTIONS:tune-%s"`): every name starting
/// with that text is a variable of its own (`getVar` finalises the variant's name; it is
/// never active through `OVERRIDES`). `${VAR:x}` reads exactly `VAR:x`.
#[derive(Default)]
struct ReadByName {
    /// `(base, prefix, exact)`.
    patterns: Vec<(String, String, bool)>,
}

impl ReadByName {
    fn load(index: &Index, bases: &BTreeSet<String>) -> Result<ReadByName> {
        let mut out = ReadByName::default();
        for base in bases {
            let needle = format!("{base}:");
            for v in query::values_containing(index, &needle)? {
                let mut rest = v.as_str();
                while let Some(i) = rest.find(&needle) {
                    let before = rest[..i].chars().next_back();
                    let after = &rest[i + needle.len()..];
                    rest = after;
                    // `FOO_BASE:` is not `BASE:`.
                    if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                        continue;
                    }
                    let lit: String = after
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || "-_+.".contains(*c))
                        .collect();
                    if lit.is_empty() {
                        continue;
                    }
                    let next = after[lit.len()..].chars().next();
                    match next {
                        Some('}') => out.patterns.push((base.clone(), lit, true)),
                        Some('$' | '\'' | '"' | '%' | ')') => {
                            out.patterns.push((base.clone(), lit, false))
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(out)
    }

    /// The text that reads `base:<first override>` by name, if any.
    fn reads(&self, base: &str, first: &str) -> Option<String> {
        self.patterns
            .iter()
            .find(|(b, p, exact)| {
                b == base
                    && if *exact {
                        first == p
                    } else {
                        first.starts_with(p.as_str())
                    }
            })
            .map(|(b, p, exact)| {
                if *exact {
                    format!("{b}:{p}")
                } else {
                    format!("{b}:{p}*")
                }
            })
    }
}

fn dead(index: &Index, ds: &DataStore, target: &Target) -> Result<Table> {
    let known = Known::load(index, ds)?;
    let names = known.names();
    let pkgs = packages(ds);
    let conds = conditionals(ds, target);
    let bases: BTreeSet<String> = conds.iter().map(|c| c.op.var.base.clone()).collect();
    let by_name = ReadByName::load(index, &bases)?;

    let mut t = Table::new(&["override", "closest", "name", "loc", "snippet"]);
    let (mut inactive_known, mut read_by_name, mut maybe_pkg) = (0usize, 0usize, 0usize);
    for c in &conds {
        let first = c.op.var.overrides.first().map(String::as_str).unwrap_or("");
        if by_name.reads(&c.op.var.base, first).is_some() {
            read_by_name += 1;
            continue;
        }
        let mut is_dead = false;
        for e in &c.elements {
            if pkgs.contains(e) || known.contains(e) {
                continue;
            }
            // BitBake does not treat a word that does not start with [a-z0-9] as an
            // override (the lint rule's test too).
            if !e.starts_with(|ch: char| ch.is_ascii_lowercase() || ch.is_ascii_digit()) {
                continue;
            }
            if c.maybe_package.as_deref() == Some(e.as_str()) {
                maybe_pkg += 1;
                continue;
            }
            is_dead = true;
            let closest = closest(e, &names).unwrap_or_else(|| "-".into());
            t.row(&[
                e,
                &closest,
                &eval::datastore::full_name(&c.op.var),
                &c.op.loc.to_string(),
                &op_snippet(ds, &c.op),
            ]);
        }
        if !is_dead && c.status == OpStatus::InactiveOverride {
            inactive_known += 1;
        }
    }
    t.note("note: dead = an override element that matches no known override: not in OVERRIDES, not a machine, distro or class name, not a declared or built-in override, not a word of any *OVERRIDES assignment in the workspace (python included), not a package of the recipe; the same definition as the lint rule override/dead");
    if inactive_known > 0 {
        t.note(&format!(
            "note: {inactive_known} inactive conditional {} known overrides: inactive for this machine and distro, not dead (show=conditional lists them)",
            if inactive_known == 1 { "operation uses" } else { "operations use" }
        ));
    }
    if read_by_name > 0 {
        t.note(&format!(
            "note: {read_by_name} {} variables read by name (such as ${{TUNE_FEATURES:tune-${{DEFAULTTUNE}}}}), not override conditions; not listed",
            if read_by_name == 1 { "operation is on" } else { "operations are on" }
        ));
    }
    if maybe_pkg > 0 {
        t.note(&format!(
            "note: {maybe_pkg} unknown {} in the package position of a package variable, or PACKAGES holds python: may be a package; not listed",
            if maybe_pkg == 1 { "name stands" } else { "names stand" }
        ));
    }
    Ok(t)
}

/// The known name with the smallest edit distance to `word` (ties: alphabetical).
fn closest(word: &str, names: &BTreeSet<String>) -> Option<String> {
    names
        .iter()
        .map(|n| (levenshtein(word, n), n))
        .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)))
        .map(|(d, n)| format!("{n} (distance {d})"))
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance() {
        assert_eq!(levenshtein("boradx", "boardx"), 2);
        assert_eq!(levenshtein("", "ab"), 2);
        assert_eq!(levenshtein("same", "same"), 0);
    }

    #[test]
    fn split_pieces() {
        let p = pieces("a:${B}${C}:pn-${PN}");
        let kinds: Vec<String> = p
            .iter()
            .map(|p| match p {
                Piece::Lit(s) => format!("L{s}"),
                Piece::Ref(s) => format!("R{s}"),
            })
            .collect();
        assert_eq!(kinds, ["La:", "RB", "RC", "L:pn-", "RPN"]);
    }
}

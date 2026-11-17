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
//! - `dead`: the conditional operations with an override element that is known nowhere:
//!   not in `OVERRIDES` for any machine or distro conf in the workspace (each evaluated), not
//!   a machine, distro or declared override (`lint::helpers::KnownOverrides`), not a class,
//!   `pn-` or `layer-` form, not a package of the recipe. Each is reported with the closest
//!   known name by edit distance.

use std::collections::{BTreeSet, HashMap};

use schemars::JsonSchema;
use serde::Deserialize;

use super::var::{op_snippet, recipe_target, replay_notes};
use crate::error::Result;
use crate::eval::{self, DataStore, OpStatus, Target, VarOp};
use crate::index::Index;
use crate::lint::helpers::KnownOverrides;
use crate::mcp::Ctx;
use crate::render::{self, Format, Table};
use crate::types::{FileKind, Loc};
use crate::workspace::Workspace;

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
    let target = recipe_target(
        &index,
        args.recipe.as_deref(),
        args.machine.as_deref(),
        args.distro.as_deref(),
    )?;
    let ds = eval::evaluate(&index, &ctx.workspace, &target)?;
    let mut t = match args.show {
        Show::Chain => chain(&ds, &target),
        Show::Conditional => conditional(&ds, &target),
        Show::Dead => dead(&index, &ctx.workspace, &ds, &target)?,
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
    for n in replay_notes(&ds, "OVERRIDES") {
        if n.starts_with("warning: anonymous python") && n.contains("computed names") {
            t.note(&n);
        }
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
    for (i, e) in ds.overrides().iter().enumerate() {
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
    if python {
        t.note("note: part of OVERRIDES comes from python, which is not evaluated; those entries are missing");
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
            for e in v.text.split(':').filter(|e| !e.is_empty()) {
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
                *python = true;
                (String::new(), None)
            }
            Piece::Ref(n) => {
                let v = ds.expand(&format!("${{{n}}}"));
                if v.unevaluated_python {
                    *python = true;
                }
                (v.text, Some(n.clone()))
            }
        };
        for _ in out.chars() {
            origin.push(from.clone());
        }
        expanded.push_str(&out);
    }
    let chars: Vec<char> = expanded.chars().collect();
    let mut start = 0;
    for i in 0..=chars.len() {
        if i < chars.len() && chars[i] != ':' {
            continue;
        }
        if i > start {
            let entry: String = chars[start..i].iter().collect();
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
        start = i + 1;
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
}

const KEYWORDS: [&str; 3] = ["append", "prepend", "remove"];

/// Every conditional operation in scope (see the module documentation).
fn conditionals(ds: &DataStore, target: &Target) -> Vec<Cond> {
    let pn = ds.value("PN").map(|v| v.text);
    let packages: BTreeSet<String> = packages(ds);
    let recipe_files: BTreeSet<(String, std::path::PathBuf)> = ds
        .files()
        .iter()
        .filter(|v| v.kind != FileKind::Conf)
        .map(|v| (v.loc.layer.clone(), v.loc.path.clone()))
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
            if parts
                .first()
                .is_some_and(|p| packages.contains(p) || p.contains("${"))
                && !KEYWORDS.contains(&parts[0].as_str())
            {
                parts.remove(0);
            }
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

/// Package names of the recipe: the words of `PACKAGES` plus `PN`.
fn packages(ds: &DataStore) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = ds
        .value("PACKAGES")
        .map(|v| v.text.split_whitespace().map(str::to_string).collect())
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

fn dead(index: &Index, ws: &Workspace, ds: &DataStore, target: &Target) -> Result<Table> {
    let known = KnownOverrides::from_index(index)?;
    let mut names: BTreeSet<String> = BTreeSet::new();
    names.extend(known.machines.iter().cloned());
    names.extend(known.distros.iter().cloned());
    names.extend(known.declared.iter().cloned());
    // OVERRIDES for every machine conf (default distro) and every distro conf (default
    // machine), each one a configuration pass (cached).
    let mut evaluated = 0;
    for (machine, distro) in known
        .machines
        .iter()
        .map(|m| (Some(m.clone()), target.distro.clone()))
        .chain(
            known
                .distros
                .iter()
                .map(|d| (target.machine.clone(), Some(d.clone()))),
        )
    {
        let cfg = eval::evaluate(
            index,
            ws,
            &Target {
                recipe: None,
                version: None,
                machine,
                distro,
            },
        )?;
        evaluated += 1;
        names.extend(cfg.overrides());
    }
    names.extend(ds.overrides());
    let pkgs = packages(ds);
    names.extend(pkgs.iter().cloned());

    let mut t = Table::new(&["override", "closest", "name", "loc", "snippet"]);
    for c in conditionals(ds, target) {
        for e in &c.elements {
            if names.contains(e) || pkgs.contains(e) || known.contains(e) {
                continue;
            }
            let closest = closest(e, &names).unwrap_or_else(|| "-".into());
            t.row(&[
                e,
                &closest,
                &eval::datastore::full_name(&c.op.var),
                &c.op.loc.to_string(),
                &op_snippet(ds, &c.op),
            ]);
        }
    }
    t.note(&format!(
        "note: known overrides: OVERRIDES of {evaluated} machine/distro configurations, machine and distro names, declared MACHINEOVERRIDES/DISTROOVERRIDES words, class-/pn-/layer- forms, and the recipe's packages"
    ));
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

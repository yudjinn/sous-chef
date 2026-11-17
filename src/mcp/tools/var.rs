//! Tool `var`: history and final value of a variable. See docs/mcp-tools.md.
//!
//! Static mode replays the configuration (and the recipe, when given) with
//! `eval::evaluate` and prints one row per operation on the variable in parse order:
//! `status op override loc snippet`, where `status` is `applied`, `inactive` (an override
//! condition is not in `OVERRIDES`), `shadowed` (overwritten, lost to another variant, or a
//! default that did not take effect) or `removed` (every word it added was removed again).
//! `unset` statements are rows with op `unset`. With `flags: true` the `VAR[flag]`
//! operations follow, per flag. A name written `VAR[flag]` shows that flag only.
//!
//! The notes carry the final value (`final: ...`, cut at [`MAX_FINAL_CHARS`] characters,
//! the full text in a handle), the python note when the value depends on unevaluated python
//! and the references left unexpanded. Anonymous python is never run; the output warns when
//! the recipe has any, and more strongly when one changes variables through computed names
//! (`AnonPython::opaque`). A function's value is shown as its pieces (pointers).
//!
//! `authoritative: true` is answered by [`super::var_authoritative::run`] (T5.5).
//!
//! The `recipe` argument is shared with `overrides` and `deps` ([`recipe_target`]): a `pn`
//! is resolved by the evaluator the way BitBake picks the file (`PREFERRED_VERSION`,
//! `DEFAULT_PREFERENCE`, layer priority, version: `eval::providers`); a `pn_pv` or a
//! pointer names one file.

use schemars::JsonSchema;
use serde::Deserialize;

use super::recipe;
use crate::error::{Error, Result};
use crate::eval::datastore::full_name;
use crate::eval::{self, DataStore, OpStatus, Target, UnsetRecord, VarOp};
use crate::handles::HandleKind;
use crate::index::Index;
use crate::index::query;
use crate::mcp::Ctx;
use crate::render::{self, Format, Table};
use crate::types::Loc;

/// Final values longer than this are cut; the full value goes to a handle.
pub const MAX_FINAL_CHARS: usize = 600;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Base name, e.g. `SRC_URI`; `FILES:${PN}` allowed.
    pub name: String,
    /// Recipe; omit for global configuration.
    pub recipe: Option<String>,
    /// Default: `MACHINE` from local.conf.
    pub machine: Option<String>,
    /// Default: `DISTRO` from local.conf.
    pub distro: Option<String>,
    /// Run `bitbake -e` instead of the static evaluator (default false).
    #[serde(default)]
    pub authoritative: bool,
    /// Also show `VAR[flag]` operations (default false).
    #[serde(default)]
    pub flags: bool,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<String> {
    if args.authoritative {
        return super::var_authoritative::run(ctx, &args).await;
    }
    let (name, flag) = split_flag(&args.name)?;
    let mut index = ctx.index.lock().await;
    let report = index.refresh(&ctx.workspace)?;
    let target = recipe_target(
        &index,
        args.recipe.as_deref(),
        args.machine.as_deref(),
        args.distro.as_deref(),
    )?;
    let ds = eval::evaluate(&index, &ctx.workspace, &target)?;
    drop(index);

    let mut t = Table::new(&["status", "op", "override", "loc", "snippet"]);
    let mut python = 0usize;

    // `FILES:${PN}` is expanded for the recipe before the lookup.
    let lookup = if name.contains("${") {
        let e = ds.expand(&name);
        if e.unevaluated_python || !e.unresolved.is_empty() {
            t.note(&format!(
                "note: the name {name} expands to {} and could not be expanded fully",
                e.text
            ));
        }
        e.text
    } else {
        name.clone()
    };

    let mut finals: Vec<String> = Vec::new();
    match &flag {
        Some(f) => {
            flag_rows(&mut t, &ds, &lookup, f);
            finals.push(flag_final(ctx, &ds, &lookup, f, &args, &mut python)?);
        }
        None => {
            value_rows(&mut t, &ds, &lookup);
            finals.push(final_line(ctx, &ds, &lookup, &args, &mut python)?);
            if args.flags {
                for f in ds.flag_names(&lookup) {
                    if f == "export" && ds.flag_history(&lookup, &f).is_empty() {
                        // Set by `export VAR`, which has no assignment of its own.
                        continue;
                    }
                    flag_rows(&mut t, &ds, &lookup, &f);
                    finals.push(flag_final(ctx, &ds, &lookup, &f, &args, &mut python)?);
                }
            }
        }
    }
    if t.is_empty() {
        t.note(&format!(
            "note: no operation on {lookup} {}",
            scope_text(&target)
        ));
    }
    for f in &finals {
        t.note(f);
    }
    for n in replay_notes(&ds, &lookup) {
        t.note(&n);
    }
    if target.recipe.is_none() {
        t.note("note: global configuration only (no recipe)");
    }
    if let Some(n) = render::python_note(python) {
        t.note(&n);
    }
    if let Some(line) = render::freshness_line(&report) {
        t.note(&line);
    }
    let budget = args
        .budget_tokens
        .unwrap_or(ctx.config.output.budget_tokens);
    Ok(t.render_as(args.format.unwrap_or_default(), budget))
}

// ---- shared with overrides and deps ----

/// The evaluation target for a `recipe` argument (`None`: the configuration only).
///
/// An exact `pn` is passed to the evaluator as a name, which picks the file the way
/// BitBake does (`eval::providers`); anything else (`pn_pv`, a pointer to a `.bb` or
/// `.bbappend`, an inexact name) goes through [`recipe::resolve_recipe`], which also gives
/// the error with candidate names.
pub fn recipe_target(
    index: &Index,
    recipe_arg: Option<&str>,
    machine: Option<&str>,
    distro: Option<&str>,
) -> Result<Target> {
    let machine = machine.map(str::trim).filter(|m| !m.is_empty());
    let distro = distro.map(str::trim).filter(|d| !d.is_empty());
    let recipe = match recipe_arg.map(str::trim) {
        None => None,
        Some(r) if !r.contains(':') && !query::find_recipes(index, r)?.is_empty() => {
            Some(r.to_string())
        }
        Some(r) => {
            let row = recipe::resolve_recipe(index, r, None)?;
            Some(format!("{}:{}", row.file.layer, row.file.path.display()))
        }
    };
    Ok(Target {
        recipe,
        version: None,
        machine: machine.map(str::to_string),
        distro: distro.map(str::to_string),
    })
}

/// Warnings from the replay that bear on `name`: the notes on the evaluation as a whole
/// (`ReplayNotes::target_notes`: a recipe file BitBake does not collect, where the machine
/// came from, no `MACHINE`), anonymous python (opaque or not) and references the replay
/// could not follow.
pub fn replay_notes(ds: &DataStore, name: &str) -> Vec<String> {
    let notes = ds.replay();
    let mut out = notes.target_notes.clone();
    let base = name.split(':').next().unwrap_or(name).to_string();
    if let Some(v) = notes.var_notes.get(&base) {
        out.extend(v.iter().cloned());
    }
    let opaque: Vec<&eval::AnonPython> = notes.anonymous.iter().filter(|a| a.opaque).collect();
    let naming: Vec<&eval::AnonPython> = notes
        .anonymous
        .iter()
        .filter(|a| !a.opaque && a.sets.iter().any(|s| s.split(':').next() == Some(&base)))
        .collect();
    for a in &opaque {
        out.push(format!(
            "warning: anonymous python at {} changes variables through computed names or hands d to other code; it is not run, so any value, this one included, may differ",
            a.loc
        ));
    }
    for a in &naming {
        out.push(format!(
            "warning: anonymous python at {} changes {base}; it is not run, so the value shown is the one before it",
            a.loc
        ));
    }
    let other = notes.anonymous.len() - opaque.len() - naming.len();
    if other > 0 {
        out.push(format!(
            "note: {other} anonymous python {} not run (none changes {base} by name)",
            if other == 1 {
                "function was"
            } else {
                "functions were"
            }
        ));
    }
    if !notes.references.is_empty() {
        let n = notes.references.len();
        out.push(format!(
            "note: {n} include/inherit {} not followed (python, undefined variables or missing files); values from those files are missing",
            if n == 1 { "reference was" } else { "references were" }
        ));
    }
    out
}

/// `status` column text.
pub fn status_str(s: OpStatus) -> &'static str {
    match s {
        OpStatus::Applied => "applied",
        OpStatus::InactiveOverride => "inactive",
        OpStatus::Shadowed => "shadowed",
        OpStatus::Removed => "removed",
    }
}

/// The statement as written, for the snippet column.
pub fn op_snippet(ds: &DataStore, op: &VarOp) -> String {
    let name = full_name(&op.var);
    let text = if is_function_op(ds, op) {
        format!("{name}() {{ ... }}")
    } else {
        match &op.var.flag {
            Some(f) => format!("{name}[{f}] {} \"{}\"", op.op, op.value),
            None => format!("{name} {} \"{}\"", op.op, op.value),
        }
    };
    render::snippet(&text)
}

/// The override column: the overrides as written (`append:boardx`), or `-`.
pub fn override_str(op: &VarOp) -> String {
    if op.var.overrides.is_empty() {
        "-".into()
    } else {
        op.var.overrides.join(":")
    }
}

fn is_function_op(ds: &DataStore, op: &VarOp) -> bool {
    op.var.flag.is_none()
        && ds.is_function(&ds.expanded_name(op))
        && !op.value.trim().is_empty()
        && op.value.lines().all(|l| {
            l.split(" -> ")
                .next()
                .unwrap_or(l)
                .trim()
                .parse::<Loc>()
                .is_ok()
        })
}

// ---- rows ----

fn split_flag(name: &str) -> Result<(String, Option<String>)> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Invalid {
            msg: "variable name is empty; give a name such as SRC_URI".into(),
        });
    }
    match name.split_once('[') {
        Some((n, rest)) => {
            let flag = rest
                .strip_suffix(']')
                .filter(|f| !f.is_empty() && !n.is_empty());
            match flag {
                Some(f) => Ok((n.to_string(), Some(f.to_string()))),
                None => Err(Error::Invalid {
                    msg: format!("bad name {name:?}; write VAR or VAR[flag]"),
                }),
            }
        }
        None => Ok((name.to_string(), None)),
    }
}

/// One row per operation on `name` and its variants, `unset` statements merged in by
/// position (file visit order, then line).
fn value_rows(t: &mut Table, ds: &DataStore, name: &str) {
    let history = ds.history(name);
    let unsets: Vec<UnsetRecord> = ds
        .unsets(name)
        .into_iter()
        .filter(|u| u.flag.is_none() && (u.name == name || !name.contains(':')))
        .collect();
    let mut rows: Vec<(Key, [String; 5])> = Vec::new();
    for (op, st) in &history {
        rows.push((
            position(ds, &op.loc),
            [
                status_str(*st).into(),
                op.op.to_string(),
                override_str(op),
                op.loc.to_string(),
                op_snippet(ds, op),
            ],
        ));
    }
    for u in unsets {
        let at = position(ds, &u.loc);
        let i = rows.iter().position(|(k, _)| *k > at).unwrap_or(rows.len());
        rows.insert(
            i,
            (
                at,
                [
                    "applied".into(),
                    "unset".into(),
                    "-".into(),
                    u.loc.to_string(),
                    render::snippet(&format!("unset {}", u.name)),
                ],
            ),
        );
    }
    for (_, r) in rows {
        t.row(&[&r[0], &r[1], &r[2], &r[3], &r[4]]);
    }
}

type Key = (usize, u32);

/// Position of a statement in parse order: the index of the first visit of its file, then
/// its line.
fn position(ds: &DataStore, loc: &Loc) -> Key {
    let file = ds
        .files()
        .iter()
        .position(|v| v.loc.layer == loc.layer && v.loc.path == loc.path)
        .unwrap_or(usize::MAX);
    (file, loc.lines.start)
}

fn flag_rows(t: &mut Table, ds: &DataStore, name: &str, flag: &str) {
    for (op, st) in ds.flag_history(name, flag) {
        let ovr = if op.var.overrides.is_empty() {
            format!("[{flag}]")
        } else {
            format!("{}[{flag}]", op.var.overrides.join(":"))
        };
        t.row(&[
            status_str(st),
            &op.op.to_string(),
            &ovr,
            &op.loc.to_string(),
            &op_snippet(ds, &op),
        ]);
    }
    for u in ds.unsets(name) {
        if u.flag.as_deref() == Some(flag) && u.name == name {
            t.row(&[
                "applied",
                "unset",
                &format!("[{flag}]"),
                &u.loc.to_string(),
                &render::snippet(&format!("unset {}[{flag}]", u.name)),
            ]);
        }
    }
}

fn final_line(
    ctx: &Ctx,
    ds: &DataStore,
    name: &str,
    args: &Args,
    python: &mut usize,
) -> Result<String> {
    if ds.is_function(name)
        && let Some(pieces) = ds.function(name)
    {
        let list: Vec<String> = pieces
            .iter()
            .map(|p| match &p.calls {
                Some(c) => format!("{} -> {c}", p.loc),
                None => p.loc.to_string(),
            })
            .collect();
        let n = list.len();
        return Ok(format!(
            "final: function {name}, {n} {} in order: {}",
            if n == 1 { "piece" } else { "pieces" },
            list.join(", ")
        ));
    }
    match ds.value(name) {
        None => Ok(format!("final: {name} is not set")),
        Some(v) => {
            if v.unevaluated_python {
                *python += 1;
            }
            let mut line = format!("final: {}", shown(ctx, &v.text, name, None, args)?);
            if !v.unresolved.is_empty() {
                line.push_str(&format!(
                    " (unresolved, left as written: {})",
                    v.unresolved.join(", ")
                ));
            }
            Ok(line)
        }
    }
}

fn flag_final(
    ctx: &Ctx,
    ds: &DataStore,
    name: &str,
    flag: &str,
    args: &Args,
    python: &mut usize,
) -> Result<String> {
    match ds.flag(name, flag) {
        None => Ok(format!("final[{flag}]: not set")),
        Some(v) => {
            if v.unevaluated_python {
                *python += 1;
            }
            let mut line = format!(
                "final[{flag}]: {}",
                shown(ctx, &v.text, name, Some(flag), args)?
            );
            if !v.unresolved.is_empty() {
                line.push_str(&format!(
                    " (unresolved, left as written: {})",
                    v.unresolved.join(", ")
                ));
            }
            Ok(line)
        }
    }
}

/// The value as shown: quoted, cut at [`MAX_FINAL_CHARS`] with the full text in a handle.
fn shown(ctx: &Ctx, text: &str, name: &str, flag: Option<&str>, args: &Args) -> Result<String> {
    if text.chars().count() <= MAX_FINAL_CHARS {
        return Ok(format!("\"{text}\""));
    }
    let mut command = format!("var {name}");
    if let Some(f) = flag {
        command.push_str(&format!("[{f}]"));
    }
    for (k, v) in [
        ("recipe", &args.recipe),
        ("machine", &args.machine),
        ("distro", &args.distro),
    ] {
        if let Some(v) = v {
            command.push_str(&format!(" {k}={v}"));
        }
    }
    let h = ctx
        .handles
        .put(HandleKind::Value, &command, text.as_bytes())?;
    Ok(format!(
        "\"{}",
        render::truncate_value(text, MAX_FINAL_CHARS, Some(&h.0))
    ))
}

fn scope_text(target: &Target) -> String {
    match &target.recipe {
        Some(r) => format!("in the evaluation of {r}"),
        None => "in the global configuration".into(),
    }
}

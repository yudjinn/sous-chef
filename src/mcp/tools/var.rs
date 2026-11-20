//! Tool `var`: history and final value of a variable. See docs/mcp-tools.md.
//!
//! Static mode replays the configuration (and the recipe, when given) with
//! `eval::evaluate` and prints one row per operation on the variable in parse order:
//! `status op override loc snippet`, where `status` is `applied`, `inactive` (an override
//! condition is not in `OVERRIDES`), `shadowed` (overwritten, lost to another variant, or a
//! default that did not take effect) or `removed` (every word it added was removed again).
//! `unset` statements are rows with op `unset`; BitBake's rewrite of virtual providers in
//! `DEPENDS` after parsing is a row with op `rewrite` (it replaces nothing: the rows before
//! it keep their status). With `flags: true` the `VAR[flag]`
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

use super::{recipe, taskfuncs};
use crate::error::{Error, Result};
use crate::eval::datastore::full_name;
use crate::eval::{self, DataStore, OpKind, OpStatus, Target, UnsetRecord, VarOp};
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
    /// Multiconfig name (or write the recipe `mc:<name>:<recipe>`).
    pub mc: Option<String>,
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
        if args.mc.as_deref().is_some_and(|m| !m.trim().is_empty())
            || args
                .recipe
                .as_deref()
                .is_some_and(|r| eval::multiconfig::split_mc(r.trim()).is_some())
        {
            return Err(Error::Invalid {
                msg: "authoritative mode does not take a multiconfig yet; drop authoritative for the static answer, or run bitbake op=env target=mc:<name>:<recipe>".into(),
            });
        }
        return super::var_authoritative::run(ctx, &args).await;
    }
    let (name, flag) = split_flag(&args.name)?;
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
    // A task function: what BitBake runs with it (`taskfuncs`).
    let task_run = if flag.is_none() && runs_as_task(&ds, &lookup) {
        let added = if ds.tasks().contains(&lookup) {
            recipe::addtask_locs(&index, &ds, &lookup)?
        } else {
            Vec::new()
        };
        Some(task_notes(ctx, &ds, &lookup, &added))
    } else {
        None
    };
    drop(index);

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
    for n in task_run.iter().flatten() {
        t.note(n);
    }
    if flag.is_none() && !ds.is_function(&lookup) {
        for n in python_eval_notes(&ds.python_log(&lookup)) {
            t.note(&n);
        }
        for n in uncertain_notes(&ds, &lookup) {
            t.note(&n);
        }
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

/// Notes on the inline python a value's expansion met (`DataStore::python_log`): one line
/// naming what was evaluated statically, one naming the first expression left unevaluated
/// because of what it reads (with the variable), at most.
pub fn python_eval_notes(log: &eval::overrides::PythonLog) -> Vec<String> {
    fn list(events: &[&eval::expand::PyEvent]) -> String {
        let mut s: Vec<String> = events
            .iter()
            .take(2)
            .map(|e| format!("`{}`", eval::pyexpr::short(&e.code)))
            .collect();
        if events.len() > 2 {
            s.push(format!("+{} more", events.len() - 2));
        }
        s.join(", ")
    }
    let mut out = Vec::new();
    let done: Vec<_> = log.own.iter().filter(|e| e.evaluated).collect();
    let ovr = log.overrides.iter().filter(|e| e.evaluated).count();
    let in_ovr = match ovr {
        0 => String::new(),
        1 => "; OVERRIDES, which decides its override variants, needed 1 more".into(),
        n => format!("; OVERRIDES, which decides its override variants, needed {n} more"),
    };
    if !done.is_empty() {
        out.push(format!(
            "note: python evaluated statically (pure helpers, exact): {}{in_ovr}",
            list(&done)
        ));
    } else if ovr > 0 {
        out.push(format!(
            "note: OVERRIDES, which decides its override variants, has python evaluated statically (pure helpers, exact): {}",
            list(&log.overrides.iter().filter(|e| e.evaluated).collect::<Vec<_>>())
        ));
    }
    // A helper that read a value that is not certain: say which.
    let blocked: Vec<_> = log
        .own
        .iter()
        .filter(|e| {
            !e.evaluated
                && e.why
                    .as_deref()
                    .is_some_and(|w| !w.starts_with("not in the evaluated subset"))
        })
        .collect();
    if let Some(first) = blocked.first() {
        let more = if blocked.len() > 1 {
            format!(" (+{} more)", blocked.len() - 1)
        } else {
            String::new()
        };
        out.push(format!(
            "note: python not evaluated: `{}`: {}{more}",
            eval::pyexpr::short(&first.code),
            first.why.as_deref().unwrap_or("")
        ));
    }
    out
}

/// At most this many `uncertain:` lines per value; the rest are counted.
pub const MAX_CAUSE_NOTES: usize = 3;

/// Why a flagged value may be wrong (`DataStore::flag_causes`): one `note: uncertain: ...`
/// line per cause, at most [`MAX_CAUSE_NOTES`], then how many more. Nothing for a value
/// that is not flagged.
pub fn uncertain_notes(ds: &DataStore, name: &str) -> Vec<String> {
    // Anonymous python that writes the variable itself by name has its own warning
    // ([`replay_notes`]): not repeated.
    let base = name.split(':').next().unwrap_or(name);
    let warned: Vec<String> = naming_functions(ds, base)
        .iter()
        .map(|a| format!("anonymous python at {} may write {name}", a.loc))
        .collect();
    let causes: Vec<String> = ds
        .flag_causes(name)
        .into_iter()
        .filter(|c| !warned.contains(c))
        .collect();
    let mut out: Vec<String> = causes
        .iter()
        .take(MAX_CAUSE_NOTES)
        .map(|c| format!("note: uncertain: {c}"))
        .collect();
    if causes.len() > MAX_CAUSE_NOTES {
        out.push(format!(
            "note: uncertain: +{} more causes",
            causes.len() - MAX_CAUSE_NOTES
        ));
    }
    out
}

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
    recipe_target_in(index, recipe_arg, machine, distro, None)
}

/// [`recipe_target`] in a multiconfig: `mc`, or the name a recipe written
/// `mc:<name>:<recipe>` gives (both, when they differ, are an error). The evaluator checks
/// the name (`eval::multiconfig::check`: an unknown one is an error listing the known ones).
pub fn recipe_target_in(
    index: &Index,
    recipe_arg: Option<&str>,
    machine: Option<&str>,
    distro: Option<&str>,
    mc: Option<&str>,
) -> Result<Target> {
    let machine = machine.map(str::trim).filter(|m| !m.is_empty());
    let distro = distro.map(str::trim).filter(|d| !d.is_empty());
    let split = eval::multiconfig::normalize_target(&Target {
        recipe: recipe_arg.map(str::to_string),
        mc: mc.map(str::to_string),
        ..Target::default()
    })?;
    let mc = split.mc;
    let recipe_arg = split.recipe.as_deref();
    let recipe = match recipe_arg.map(str::trim) {
        None => None,
        Some(r) if !r.contains(':') && !query::find_recipes(index, r)?.is_empty() => {
            Some(r.to_string())
        }
        // A variant pointer (`virtual:native:<layer>:<path>`): the evaluator takes it.
        Some(r) if eval::variant::split_virtual(r).is_some() => {
            let (ext, file) = eval::variant::split_virtual(r).ok_or_else(|| Error::Invalid {
                msg: format!("bad variant pointer {r:?}"),
            })?;
            let row = recipe::resolve_recipe(index, file, None)?;
            Some(format!(
                "{}{}:{}",
                ext.virtual_prefix(),
                row.file.layer,
                row.file.path.display()
            ))
        }
        // A class-extended variant (`gmp-native`): the evaluator makes it from the base
        // recipe as BitBake does (eval::variant).
        Some(r) if !r.contains(':') && recipe::class_extend_variant(index, r)?.is_some() => {
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
        mc,
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
    // A function proved to return at once writes nothing; a name whose writes were all
    // proved not to happen is not changed here (`ReplayNotes::no_writes`).
    let live: Vec<&eval::AnonPython> = notes
        .anonymous
        .iter()
        .filter(|a| a.returns_early.is_none())
        .collect();
    let opaque: Vec<&eval::AnonPython> = live.iter().copied().filter(|a| a.opaque).collect();
    let naming = naming_functions(ds, &base);
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
    for w in notes
        .no_writes
        .iter()
        .filter(|w| w.name.split(':').next() == Some(&base))
    {
        for (at, why) in &w.why {
            out.push(format!(
                "note: anonymous python at {at} does not write {} here: {why}",
                w.name
            ));
        }
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
            "note: {n} include/inherit {} not followed (python, undefined variables or missing files); every value the files they would read may change is flagged (a missing file or a cycle stops BitBake itself)",
            if n == 1 { "reference was" } else { "references were" }
        ));
    }
    for u in &notes.uncertain_references {
        let at = u
            .loc
            .as_ref()
            .map(|l| format!(" at {l}"))
            .unwrap_or_default();
        let why = if u.unresolved.is_empty() {
            "it depends on python or on overrides that are not all known".to_string()
        } else {
            format!(
                "it depends on unresolved {} or on overrides that are not all known",
                u.unresolved.join(", ")
            )
        };
        out.push(format!(
            "note: `{} {}`{at} was followed as {:?}, but {why}; another value would read other files, so every value is flagged",
            u.kind, u.text, u.expanded
        ));
    }
    // References followed only for the words certain whatever the python gives, whose
    // unread files write this variable.
    for e in notes
        .ref_effects
        .iter()
        .filter(|e| e.vars.as_ref().is_some_and(|m| m.contains_key(&base)))
    {
        let at = e
            .loc
            .as_ref()
            .map(|l| format!(" at {l}"))
            .unwrap_or_default();
        out.push(format!(
            "note: `{} {}`{at} was followed for the words it reads whatever its python gives; it may also read {}, which sous-chef did not read: the values they change are flagged",
            e.kind,
            e.text,
            e.not_read.join(", ")
        ));
    }
    out
}

/// The anonymous functions [`replay_notes`] warns about by name for `base`: not opaque,
/// not proved to return at once, and writing a name of `base` that is still in
/// `python_vars` (not proved unwritten).
fn naming_functions<'a>(ds: &'a DataStore, base: &str) -> Vec<&'a eval::AnonPython> {
    let notes = ds.replay();
    notes
        .anonymous
        .iter()
        .filter(|a| a.returns_early.is_none() && !a.opaque)
        .filter(|a| {
            a.writes()
                .any(|e| e.split(':').next() == Some(base) && notes.python_vars.contains(e))
        })
        .collect()
}

/// The target notes of a multiconfig evaluation (`eval::multiconfig`: which configuration,
/// which file), for the tools that do not print every target note.
pub fn mc_notes(ds: &DataStore) -> Vec<String> {
    ds.replay()
        .target_notes
        .iter()
        .filter(|n| n.starts_with("note: multiconfig "))
        .cloned()
        .collect()
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
    let text = if op.kind == OpKind::VirtualProviderRewrite {
        format!(
            "{name} rewritten after parsing (BB_RECIPE_VIRTUAL_PROVIDERS) to \"{}\"",
            op.value
        )
    } else if op.kind == OpKind::ClassExtend {
        format!(
            "{name} set by BitBake for the BBCLASSEXTEND variant to \"{}\"",
            op.value
        )
    } else if op.kind == OpKind::EventHandler {
        format!(
            "{name} set by an event handler while finalising to \"{}\"",
            op.value
        )
    } else if is_function_op(ds, op) {
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

// ---- what a task runs ----

/// `name` is a function that is a task (`addtask`) or has `[prefuncs]` / `[postfuncs]`.
fn runs_as_task(ds: &DataStore, name: &str) -> bool {
    ds.is_function(name)
        && (ds.tasks().iter().any(|t| t == name)
            || taskfuncs::RUN_FLAGS
                .iter()
                .any(|f| ds.raw_flag(name, f).is_some()))
}

/// The lines `var` adds for a task function (`taskfuncs`): that it is a task and the run
/// order, each prefunc and postfunc with where it is defined and the flag operation that
/// added it, the flags that change how it runs, and the functions not run that may change
/// the lists (at most [`taskfuncs::MAX_WRITER_NOTES`]). `added`: its `addtask` statements.
fn task_notes(ctx: &Ctx, ds: &DataStore, name: &str, added: &[Loc]) -> Vec<String> {
    let mut out = Vec::new();
    let is_task = ds.tasks().iter().any(|t| t == name);
    if is_task {
        let at = match added.split_first() {
            Some((first, [])) => format!(" (addtask at {first})"),
            Some((first, rest)) => format!(" (addtask at {first}, +{} more)", rest.len()),
            None => String::new(),
        };
        let noexec = ds
            .flag(name, "noexec")
            .is_some_and(|v| v.text.trim() == "1");
        if noexec {
            out.push(format!(
                "task: {name}{at} has [noexec] \"1\": BitBake does not run it or its [prefuncs] / [postfuncs], it only writes the stamp"
            ));
        } else {
            out.push(format!(
                "task: {name}{at} runs its [prefuncs], then {name}, then its [postfuncs]"
            ));
        }
    } else {
        out.push(format!(
            "note: {name} is not a task (no addtask): its [prefuncs] / [postfuncs] run only when it runs as a task, not when another function calls it"
        ));
    }
    for flag in taskfuncs::RUN_FLAGS {
        let one = flag.trim_end_matches('s');
        match taskfuncs::flag_funcs(ds, name, flag) {
            None => out.push(format!("{flag}: none set")),
            Some(ff) => {
                if let Some(why) = &ff.uncertain {
                    out.push(format!("{flag}: uncertain: {why}"));
                }
                for f in &ff.funcs {
                    let from = match (&f.set_op, &f.set_at) {
                        (Some(op), Some(at)) => format!(" (by [{flag}] {op} at {at})"),
                        _ => String::new(),
                    };
                    out.push(format!("{one}: {} {}{from}", f.name, f.defined));
                }
            }
        }
    }
    let flags = taskfuncs::run_flags(ds, name);
    if !flags.is_empty() {
        out.push(format!("flags: {}", flags.join("; ")));
    }
    let writers = taskfuncs::may_write(&ctx.workspace, ds, name);
    for w in writers.iter().take(taskfuncs::MAX_WRITER_NOTES) {
        out.push(w.note(name));
    }
    if writers.len() > taskfuncs::MAX_WRITER_NOTES {
        out.push(format!(
            "note: not run: +{} more functions may change {name}[prefuncs] / [postfuncs]",
            writers.len() - taskfuncs::MAX_WRITER_NOTES
        ));
    }
    out
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
                op.kind.label(op.op),
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
            line.push_str(&unresolved_note(ds, &v));
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
            line.push_str(&unresolved_note(ds, &v));
            Ok(line)
        }
    }
}

/// The note after a final value for its unresolved names. A name still in the text as
/// `${NAME}` is "left as written". Any other name reached the value because an override
/// decision it depends on (an override variant, or a conditional `:append` / `:prepend` /
/// `:remove`, of the variable or of one it references) was judged inactive while
/// `OVERRIDES` has that name unresolved, or because a statement the replay could not apply
/// may set the variable: the text is complete, but it may differ. The evaluator attaches
/// such names only to values whose result could depend on them (`eval::overrides`: a value
/// with no conditional operation in its history is not affected).
pub fn unresolved_note(ds: &DataStore, v: &eval::Value) -> String {
    if v.unresolved.is_empty() {
        return String::new();
    }
    let (left, other): (Vec<&String>, Vec<&String>) = v
        .unresolved
        .iter()
        .partition(|u| v.text.contains(&format!("${{{u}}}")));
    let join = |xs: &[&String]| xs.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
    let mut out = String::new();
    if !left.is_empty() {
        out.push_str(&format!(" (unresolved, left as written: {})", join(&left)));
    }
    if !other.is_empty() {
        let in_overrides = ds
            .overrides_value()
            .map(|o| o.unresolved)
            .unwrap_or_default();
        let (ovr, rest): (Vec<&String>, Vec<&String>) =
            other.into_iter().partition(|u| in_overrides.contains(u));
        if !ovr.is_empty() {
            out.push_str(&format!(
                " (may differ: an override decision it depends on is uncertain because OVERRIDES has unresolved {})",
                join(&ovr)
            ));
        }
        if !rest.is_empty() {
            out.push_str(&format!(
                " (may differ: depends on unresolved {})",
                join(&rest)
            ));
        }
    }
    out
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
        ("mc", &args.mc),
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

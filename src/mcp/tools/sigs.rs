//! Tool `sigs`: why a task re-ran. See docs/mcp-tools.md.
//!
//! Runs `bitbake-diffsigs -t <recipe> <task>` (or on two signature files) through the
//! build environment's runner, stores its whole output under a handle and returns a digest
//! (`bitbake::sigs`): the root-cause changes, each variable mapped to the assignment that
//! sets it now (through the evaluated datastore of the recipe the task belongs to), and the
//! chain of dependencies it came through. Never builds anything. Without a usable runner
//! it says how many signatures the stamps directory holds and stops.

use std::collections::HashMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::bitbake::sigs::{self, Change, DiffSigs, EnvDiffSigs, NoDiff};
use crate::bitbake::{BbEnv, logs, validate_arg};
use crate::error::{Error, Result};
use crate::eval::{self, DataStore, OpStatus, Target};
use crate::handles::HandleKind;
use crate::mcp::Ctx;
use crate::mcp::tools::recipe;
use crate::render::{self, Format, Table};
use crate::types::Loc;

/// Default budget: the digest stays under about 1 KB.
pub const DEFAULT_BUDGET: usize = 250;

/// How many cause rows are shown; the rest are counted.
const MAX_CAUSES: usize = 6;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Recipe (`pn`) whose task re-ran; with `task`.
    pub recipe: Option<String>,
    /// Task, e.g. `compile` or `do_compile`.
    pub task: Option<String>,
    /// Older signature file (with `sigfile2`, instead of recipe and task).
    pub sigfile1: Option<String>,
    /// Second (newer) signature file.
    pub sigfile2: Option<String>,
    /// Output budget in tokens (default 250).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<String> {
    let env = if ctx.config.bitbake.enabled {
        BbEnv::detect(&ctx.config, &ctx.workspace)
    } else {
        None
    };
    match env {
        Some(env) if EnvDiffSigs::allowed() => {
            run_with(ctx, args, Some(&EnvDiffSigs { env }), None).await
        }
        Some(_) => {
            run_with(
                ctx,
                args,
                None,
                Some(format!(
                    "{} is not in the environment runner's allowlist (bitbake::env::ALLOWED_PROGRAMS), so it was not run",
                    sigs::PROGRAM
                )),
            )
            .await
        }
        None => {
            let why = if ctx.config.bitbake.enabled {
                "no build environment was found (see status)"
            } else {
                "bitbake is disabled in sous-chef.toml ([bitbake] enabled = false)"
            };
            run_with(
                ctx,
                args,
                None,
                Some(format!("{} was not run: {why}", sigs::PROGRAM)),
            )
            .await
        }
    }
}

/// What is compared.
enum Subject {
    Task { pn: String, task: String },
    Files(String, String),
}

/// [`run`] with an explicit runner (`None`: none is usable, `why` says why). Tests pass a
/// fake one.
pub async fn run_with(
    ctx: &Ctx,
    args: Args,
    runner: Option<&dyn DiffSigs>,
    why: Option<String>,
) -> Result<String> {
    let subject = subject(ctx, &args)?;
    let budget = match args.budget_tokens {
        None | Some(0) => DEFAULT_BUDGET,
        Some(n) => n,
    };
    let format = args.format.unwrap_or_default();
    let mut t = Table::new(&["kind", "item", "detail", "loc"]);

    // What the stamps directory holds, read without running anything.
    let stamps = match (&subject, &ctx.workspace.build_dir) {
        (Subject::Task { pn, task }, Some(b)) => Some(sigs::stamp_sigfiles(b, pn, task, |p| {
            ctx.workspace.confine(p).is_ok()
        })),
        _ => None,
    };
    let stamps_note = stamps.as_ref().map(|s| {
        format!(
            "note: the stamps directory holds {} signature {} for this task ({}); bitbake-diffsigs also looks in the sstate cache",
            s.len(),
            if s.len() == 1 { "file" } else { "files" },
            "<build>/tmp*/stamps/*/<pn>/*.<task>.sigdata.*"
        )
    });

    let Some(runner) = runner else {
        let label = label(&subject);
        t.row(&["task", &label, "not compared", "-"]);
        t.note(&format!(
            "note: {}",
            why.unwrap_or_else(|| format!("{} was not run", sigs::PROGRAM))
        ));
        if let Some(n) = stamps_note {
            t.note(&n);
        }
        if let Subject::Task { pn, task } = &subject {
            t.note(&format!(
                "note: to compare by hand in the build environment: bitbake-diffsigs -t {pn} {task}"
            ));
        }
        return Ok(t.render_as(format, budget));
    };

    let argv: Vec<String> = match &subject {
        Subject::Task { pn, task } => vec![
            "-c".into(),
            "never".into(),
            "-t".into(),
            pn.clone(),
            task.clone(),
        ],
        Subject::Files(a, b) => vec!["-c".into(), "never".into(), a.clone(), b.clone()],
    };
    let out = runner.diffsigs(&argv).await?;
    let mut whole = out.stdout.clone();
    if !out.stderr.is_empty() {
        whole.push_str("\n--- stderr ---\n");
        whole.push_str(&out.stderr);
    }
    let command = format!("{} {}", sigs::PROGRAM, argv.join(" "));
    let shown_command = match &subject {
        Subject::Task { .. } => command.clone(),
        Subject::Files(a, b) => format!(
            "{} -c never {} {}",
            sigs::PROGRAM,
            shown(ctx, a),
            shown(ctx, b)
        ),
    };
    let handle = ctx
        .handles
        .put(HandleKind::Text, &command, whole.as_bytes())?;
    let diff = sigs::parse(&out.stdout, &out.stderr);
    let label = label(&subject);

    if out.timed_out {
        t.row(&["task", &label, "bitbake-diffsigs timed out", "-"]);
    } else if let Some(nd) = &diff.no_diff {
        let what = match nd {
            NoDiff::OnlyOne => {
                "only one signature: nothing to compare (the task has run once with these inputs, or older signatures were cleaned)"
            }
            NoDiff::NoneFound => {
                "no signature found: the task has not run (or its signatures were cleaned)"
            }
        };
        t.row(&["task", &label, what, "-"]);
    } else if diff.items.is_empty() {
        let what = if out.status == Some(0) {
            "the two signatures do not differ"
        } else {
            "bitbake-diffsigs failed"
        };
        t.row(&["task", &label, what, "-"]);
        for e in diff.errors.iter().take(3) {
            t.row(&["error", "-", &render::snippet(e), "-"]);
        }
    } else {
        digest(ctx, &mut t, &subject, &label, &diff).await?;
    }

    let size = whole.len();
    t.note(&format!(
        "note: from `{shown_command}` (BitBake's own comparison; nothing was built); full output {} ({} bytes): slice handle={}",
        handle, size, handle
    ));
    if diff.unknown > 0 {
        t.note(&format!(
            "note: {} output {} not recognised; see the handle",
            diff.unknown,
            if diff.unknown == 1 {
                "line was"
            } else {
                "lines were"
            }
        ));
    }
    if diff.no_diff.is_some()
        && let Some(n) = stamps_note
    {
        t.note(&n);
    }
    Ok(t.render_as(format, budget))
}

/// A path as `layer:path` when it is in the workspace (the build directory is the layer
/// `build`), else as it is.
fn shown(ctx: &Ctx, p: &str) -> String {
    match ctx
        .workspace
        .loc_for(std::path::Path::new(p), crate::types::LineSpan::line(1))
    {
        Some(l) => format!("{}:{}", l.layer, l.path.display()),
        None => p.to_string(),
    }
}

fn label(s: &Subject) -> String {
    match s {
        Subject::Task { pn, task } => format!("{pn}:{task}"),
        Subject::Files(a, b) => format!(
            "{} -> {}",
            a.rsplit('/').next().unwrap_or(a),
            b.rsplit('/').next().unwrap_or(b)
        ),
    }
}

fn subject(ctx: &Ctx, args: &Args) -> Result<Subject> {
    match (&args.recipe, &args.task, &args.sigfile1, &args.sigfile2) {
        (Some(r), Some(t), None, None) => {
            let pn = r.trim();
            logs::check_name("recipe", pn)?;
            let task = logs::normalize_task(t)?;
            Ok(Subject::Task {
                pn: pn.to_string(),
                task,
            })
        }
        (_, _, Some(a), Some(b)) if args.recipe.is_none() && args.task.is_none() => {
            let a = sigfile(ctx, a)?;
            let b = sigfile(ctx, b)?;
            Ok(Subject::Files(a, b))
        }
        _ => Err(Error::Invalid {
            msg: "give recipe and task (the two latest signatures of that task), or sigfile1 and sigfile2 (two signature files)".into(),
        }),
    }
}

/// A signature file argument: an existing file inside the workspace (the build directory),
/// whose path the runner can pass (`validate_arg`: no `:`, so an sstate `*.siginfo` name
/// cannot be given; use recipe and task for those).
fn sigfile(ctx: &Ctx, p: &str) -> Result<String> {
    let path = PathBuf::from(p.trim());
    let resolved = ctx.workspace.confine(&path)?;
    if !resolved.is_file() {
        return Err(Error::NotFound {
            what: format!("signature file {}", path.display()),
        });
    }
    let s = resolved.display().to_string();
    validate_arg(&s).map_err(|_| Error::Invalid {
        msg: format!(
            "signature file path {s:?} has characters the runner does not pass (only letters, digits and _ . + - /); give recipe and task instead"
        ),
    })?;
    Ok(s)
}

/// The rows of a digest: the task, the dependency path, the causes.
async fn digest(
    ctx: &Ctx,
    t: &mut Table,
    subject: &Subject,
    label: &str,
    diff: &sigs::Diff,
) -> Result<()> {
    let own = diff.own_basehash_changed();
    let verb = match subject {
        Subject::Task { .. } => "re-ran",
        Subject::Files(..) => "differs",
    };
    let path = diff.path();
    let how = if own {
        format!("{verb}: its own inputs changed (basehash)")
    } else if !path.is_empty() {
        format!(
            "{verb}: its own inputs did not change; a dependency's did, by way of {}",
            path.iter()
                .map(|(task, _, _)| *task)
                .collect::<Vec<_>>()
                .join(" -> ")
        )
    } else {
        format!("{verb}: its signature changed")
    };
    t.row(&["task", label, &how, "-"]);
    for (task, old, new) in &path {
        t.row(&[
            "via",
            task,
            &format!("hash {} -> {}", short(old), short(new)),
            "-",
        ]);
    }

    let mut index = ctx.index.lock().await;
    index.refresh(&ctx.workspace)?;
    let mut stores: HashMap<String, Option<DataStore>> = HashMap::new();
    // A basehash line says no more than the changes beside it; keep it only alone.
    let all = diff.causes();
    let causes: Vec<&sigs::Item> = all
        .iter()
        .filter(|i| {
            !matches!(i.change, Change::BaseHash { .. })
                || !all.iter().any(|o| {
                    o.depth == i.depth
                        && o.chain == i.chain
                        && !matches!(o.change, Change::BaseHash { .. })
                })
        })
        .copied()
        .collect();
    let shown = causes.len().min(MAX_CAUSES);
    for item in &causes[..shown] {
        // The recipe the change belongs to: the innermost dependency, else the subject.
        let owner = item
            .chain
            .last()
            .map(|t| sigs::split_task(t).0)
            .unwrap_or_else(|| match subject {
                Subject::Task { pn, .. } => Some(pn.clone()),
                Subject::Files(a, _) => stamp_owner(a),
            });
        let (what, detail, var, added) = describe(&item.change);
        let loc = match (&owner, &var) {
            (Some(pn), Some(v)) => {
                let ds = stores.entry(pn.clone()).or_insert_with(|| {
                    recipe::resolve_selected_variant(&index, &ctx.workspace, pn, None)
                        .ok()
                        .and_then(|(r, _, variant)| {
                            let target = Target {
                                recipe: Some(recipe::eval_label(&r, variant.as_ref())),
                                ..Target::default()
                            };
                            eval::evaluate(&index, &ctx.workspace, &target).ok()
                        })
                });
                ds.as_ref()
                    .and_then(|ds| var_loc(ds, v, &added))
                    .map_or("-".into(), |l| l.to_string())
            }
            _ => "-".into(),
        };
        let item_label = match &owner {
            Some(pn) if item.depth > 0 => format!("{what} ({pn})"),
            _ => what,
        };
        t.row(&["cause", &item_label, &render::snippet(&detail), &loc]);
    }
    if causes.len() > shown {
        t.limited(causes.len() - shown, MAX_CAUSES);
    }
    Ok(())
}

/// The recipe a stamps-directory signature file belongs to: `<STAMPS_DIR>/<arch>/<pn>/
/// <pv>.<task>.sigdata.<hash>` (`STAMP` in OE-core `bitbake.conf`). `None` for any other
/// name.
fn stamp_owner(path: &str) -> Option<String> {
    let p = std::path::Path::new(path);
    let name = p.file_name()?.to_str()?;
    if !name.contains(".sigdata.") {
        return None;
    }
    Some(p.parent()?.file_name()?.to_str()?.to_string())
}

fn short(h: &str) -> &str {
    h.get(..10).unwrap_or(h)
}

/// The row label, the detail, the variable to point at and the words the change added.
fn describe(c: &Change) -> (String, String, Option<String>, Vec<String>) {
    match c {
        Change::VarValue {
            var,
            old,
            new,
            diff,
        } => {
            let detail = match (old, new) {
                (Some(o), Some(n)) => format!("value changed from '{o}' to '{n}'"),
                _ => match diff.as_slice() {
                    [one] => format!("value changed: {one}"),
                    lines => {
                        let plus = lines
                            .iter()
                            .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
                            .count();
                        let minus = lines
                            .iter()
                            .filter(|l| l.starts_with('-') && !l.starts_with("---"))
                            .count();
                        format!(
                            "value changed: {plus} {} added, {minus} removed",
                            if plus == 1 { "line" } else { "lines" }
                        )
                    }
                },
            };
            let mut added = sigs::added_words(diff);
            if let Some(n) = new {
                added.extend(n.split_whitespace().map(str::to_string));
            }
            (var.clone(), detail, Some(var.clone()), added)
        }
        Change::VarDep { var, added } => (
            var.clone(),
            format!(
                "dependency on the variable {}",
                if *added { "added" } else { "removed" }
            ),
            Some(var.clone()),
            Vec::new(),
        ),
        Change::VarDeps { var, items } => (
            var.clone(),
            format!(
                "the variables it depends on changed{}",
                items.as_ref().map(|i| format!(": {i}")).unwrap_or_default()
            ),
            Some(var.clone()),
            Vec::new(),
        ),
        Change::BaseHash { old, new } => (
            "basehash".into(),
            format!(
                "{} -> {}: a variable or function the task uses changed",
                short(old),
                short(new)
            ),
            None,
            Vec::new(),
        ),
        Change::TaskDeps { lines } => {
            let (added, removed) = list_change(lines);
            (
                "variables".into(),
                format!(
                    "the task's variable dependencies changed: added {}; removed {}",
                    or_none(&added),
                    or_none(&removed)
                ),
                None,
                Vec::new(),
            )
        }
        Change::FileChecksum { file } => (
            file.clone(),
            "file checksum changed".into(),
            None,
            Vec::new(),
        ),
        Change::FileDep { file, added } => (
            file.clone(),
            format!(
                "dependency on the file's checksum {}",
                if *added { "added" } else { "removed" }
            ),
            None,
            Vec::new(),
        ),
        Change::TaskDep { task, added } => (
            task.clone(),
            format!(
                "dependency on the task {}",
                if *added { "added" } else { "removed" }
            ),
            None,
            Vec::new(),
        ),
        Change::DepHash { task, old, new } => (
            task.clone(),
            format!("hash {} -> {}", short(old), short(new)),
            None,
            Vec::new(),
        ),
        Change::RunTaskDeps { lines } => (
            "runtaskdeps".into(),
            format!(
                "the tasks it depends on changed{}",
                lines.first().map(|l| format!(": {l}")).unwrap_or_default()
            ),
            None,
            Vec::new(),
        ),
        Change::Taint { old, new } => (
            "taint".into(),
            format!("forced or invalidated task: taint {old} -> {new}"),
            None,
            Vec::new(),
        ),
        Change::IgnoreList { which } => (
            which.clone(),
            "changed (BB_BASEHASH_IGNORE_VARS / BB_TASKHASH_IGNORE_TASKS)".into(),
            None,
            Vec::new(),
        ),
        Change::Unmatched { text } => (
            "unmatched".into(),
            format!("{text} (recursion stopped here)"),
            None,
            Vec::new(),
        ),
    }
}

/// The words added to and removed from a `Task dependencies changed from:\n[..]\nto:\n[..]`.
fn list_change(lines: &[String]) -> (Vec<String>, Vec<String>) {
    let Ok(re) = regex::Regex::new(r"'([^']*)'") else {
        return (Vec::new(), Vec::new());
    };
    let words = |l: Option<&String>| -> Vec<String> {
        l.map(|l| re.captures_iter(l).map(|c| c[1].to_string()).collect())
            .unwrap_or_default()
    };
    let before = words(lines.first());
    let after = words(lines.iter().skip_while(|l| l.as_str() != "to:").nth(1));
    let added = after
        .iter()
        .filter(|w| !before.contains(w))
        .cloned()
        .collect();
    let removed = before
        .iter()
        .filter(|w| !after.contains(w))
        .cloned()
        .collect();
    (added, removed)
}

fn or_none(v: &[String]) -> String {
    if v.is_empty() {
        "none".into()
    } else {
        v.join(" ")
    }
}

/// The assignment that sets `var` now: the applied operation whose expanded value holds
/// one of the `added` words, else the last applied operation.
fn var_loc(ds: &DataStore, var: &str, added: &[String]) -> Option<Loc> {
    let applied: Vec<_> = ds
        .history(var)
        .into_iter()
        .filter(|(op, st)| *st == OpStatus::Applied && op.var.flag.is_none())
        .map(|(op, _)| op)
        .collect();
    if !added.is_empty()
        && let Some(op) = applied.iter().rev().find(|op| {
            // Signature data holds unexpanded values; match the words as written and
            // expanded.
            let v = format!("{} {}", op.value, ds.expand(&op.value).text);
            added
                .iter()
                .any(|w| v.split_whitespace().any(|x| x == w.as_str()))
        })
    {
        return Some(op.loc.clone());
    }
    applied.last().map(|op| op.loc.clone())
}

//! Tool `log`: digest of a build failure log. See docs/mcp-tools.md.
//!
//! Finds the log (through `bitbake-getvar` when a build environment is available, else by
//! searching the build directory), stores it whole under a `log` handle and returns the
//! digest from [`crate::bitbake::logs`]. Every file is read through `Workspace::confine`.

use std::fs;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Deserialize;

use crate::bitbake::BbEnv;
use crate::bitbake::logs::{self, DigestInput};
use crate::error::{Error, Result};
use crate::handles::HandleKind;
use crate::mcp::Ctx;
use crate::render::{DEFAULT_BUDGET_TOKENS, Format};

/// How many candidate logs are read when looking for the most recent failure.
const MAX_CANDIDATES: usize = 25;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Default: the most recent failure.
    pub recipe: Option<String>,
    /// Task, e.g. `compile`.
    pub task: Option<String>,
    /// Job id from the `bitbake` tool.
    pub job: Option<String>,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<String> {
    if let Some(job) = &args.job {
        // The job table (T5.4) has no query API yet; when it does, read the job's output
        // handle, locate its task log with `logs::failure_logfiles` and digest that.
        return Err(Error::Invalid {
            msg: format!(
                "job {job:?}: looking up jobs is not available yet; pass recipe and task instead"
            ),
        });
    }
    let task = args.task.as_deref().map(logs::normalize_task).transpose()?;
    if let Some(r) = &args.recipe {
        logs::check_name("recipe", r)?;
    }
    let build_dir = ctx
        .workspace
        .build_dir
        .clone()
        .ok_or_else(|| Error::NotFound {
            what: "a build directory (task logs live under <build>/tmp/work); set build_dir in \
               sous-chef.toml or pass --build-dir"
                .into(),
        })?;

    let mut notes = Vec::new();
    let path = locate(
        ctx,
        &build_dir,
        args.recipe.as_deref(),
        task.as_deref(),
        &mut notes,
    )
    .await?;

    let resolved = ctx.workspace.confine(&path)?;
    let bytes = fs::read(&resolved)?;
    let text = String::from_utf8_lossy(&bytes);
    let handle = ctx
        .handles
        .put(HandleKind::Log, &resolved.display().to_string(), &bytes)?;

    let run_script =
        logs::shell_trace(&text).and_then(|t| read_run_file(ctx, &resolved, &t.run_file));

    let meta = logs::meta_from_path(&resolved).or(logs::meta_from_text(&text));
    let definition = match (&meta.recipe, &meta.task) {
        (Some(r), Some(t)) => {
            let mut index = ctx.index.lock().await;
            // A failed refresh only costs the pointer; the digest is still useful.
            if let Err(e) = index.refresh(&ctx.workspace) {
                notes.push(format!("index refresh failed, no definition pointer: {e}"));
            }
            logs::task_definition(&index, &ctx.workspace, r, t)
        }
        _ => None,
    };

    let mut d = logs::digest(&DigestInput {
        text: &text,
        log_path: Some(&resolved),
        run_script: run_script.as_deref(),
        ws: Some(&ctx.workspace),
        handle: Some(&handle),
        definition: definition.as_ref(),
    });
    d.notes = notes;
    let budget = match args
        .budget_tokens
        .unwrap_or(ctx.config.output.budget_tokens)
    {
        0 => DEFAULT_BUDGET_TOKENS,
        n => n,
    };
    Ok(d.render(args.format.unwrap_or_default(), budget))
}

/// Finds the log to digest. Order:
///
/// 1. `recipe` given and a build environment available: `T` from `bitbake-getvar`.
/// 2. `recipe` given: the newest failing `log.do_*` of that recipe in the build directory.
/// 3. No `recipe`: the last `Logfile of failure stored in:` of the newest console log,
///    else the newest failing `log.do_*` of any recipe.
async fn locate(
    ctx: &Ctx,
    build_dir: &Path,
    recipe: Option<&str>,
    task: Option<&str>,
    notes: &mut Vec<String>,
) -> Result<PathBuf> {
    if let Some(r) = recipe {
        if ctx.config.bitbake.enabled
            && let Some(env) = BbEnv::detect(&ctx.config, &ctx.workspace)
        {
            match logs::t_dir_from_bitbake(&env, r).await {
                Ok(t) => {
                    let found = logs::logs_in_t_dir(&t, task);
                    if let Some(p) = pick_failed(ctx, &found) {
                        return Ok(p);
                    }
                    notes.push(format!(
                        "note: no matching log in {}; searched the build directory",
                        t.display()
                    ));
                }
                Err(e) => notes.push(format!("note: {e}; searched the build directory instead")),
            }
        }
        let found = logs::find_task_logs(build_dir, Some(r), task);
        return pick_failed(ctx, &found).ok_or_else(|| not_found(recipe, task));
    }

    if let Some(cooker) = logs::newest_cooker_log(build_dir)
        && let Ok(resolved) = ctx.workspace.confine(&cooker)
        && let Ok(out) = fs::read(&resolved)
    {
        let out = String::from_utf8_lossy(&out);
        let wanted = logs::failure_logfiles(&out).into_iter().rev().find(|p| {
            let meta = logs::meta_from_path(p);
            task.is_none_or(|t| meta.task.as_deref() == Some(t)) && p.exists()
        });
        if let Some(p) = wanted {
            return Ok(p);
        }
    }
    let found = logs::find_task_logs(build_dir, None, task);
    pick_failed(ctx, &found).ok_or_else(|| not_found(None, task))
}

/// The newest candidate whose text has a recognised error, else the newest candidate.
fn pick_failed(ctx: &Ctx, candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .take(MAX_CANDIDATES)
        .find(|p| {
            ctx.workspace
                .confine(p)
                .ok()
                .and_then(|r| fs::read(r).ok())
                .is_some_and(|b| {
                    let text = String::from_utf8_lossy(&b);
                    logs::detect(&text)
                        .iter()
                        .any(|f| f.rank <= logs::Rank::Generic)
                })
        })
        .or_else(|| candidates.first())
        .cloned()
}

/// The run file the trap report names: first next to the log (the build directory may
/// have moved since), then at the printed path. Confined; `None` if unreadable.
fn read_run_file(ctx: &Ctx, log: &Path, printed: &Path) -> Option<String> {
    let name = printed.file_name()?;
    let sibling = log.parent().map(|d| d.join(name));
    sibling
        .into_iter()
        .chain(std::iter::once(printed.to_path_buf()))
        .find_map(|p| {
            let r = ctx.workspace.confine(&p).ok()?;
            fs::read_to_string(r).ok()
        })
}

fn not_found(recipe: Option<&str>, task: Option<&str>) -> Error {
    let what = match (recipe, task) {
        (Some(r), Some(t)) => format!("a {t} log for recipe {r}"),
        (Some(r), None) => format!("a task log for recipe {r}"),
        (None, Some(t)) => format!("a {t} log"),
        (None, None) => "a failed task log".to_string(),
    };
    Error::NotFound {
        what: format!(
            "{what} under <build>/tmp*/work/*/<recipe>/*/temp; check the recipe name or run the task"
        ),
    }
}

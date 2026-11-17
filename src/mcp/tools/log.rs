//! Tool `log`: digest of a build failure log. See docs/mcp-tools.md.
//!
//! Finds the log (through `bitbake-getvar` when a build environment is available, else by
//! searching the build directory), stores it whole under a `log` handle and returns the
//! digest from [`crate::bitbake::logs`]. Every file is read through `Workspace::confine`.
//!
//! `job=<id>` digests a finished `bitbake` job: the failing task's log named in the job's
//! output, else the output itself (stored under the job's handle).

use std::fs;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Deserialize;

use crate::bitbake::logs::{self, DigestInput};
use crate::bitbake::{BbEnv, JobState};
use crate::error::{Error, Result};
use crate::handles::{Handle, HandleKind};
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
    let task = args.task.as_deref().map(logs::normalize_task).transpose()?;
    if let Some(r) = &args.recipe {
        logs::check_name("recipe", r)?;
    }
    let mut notes = Vec::new();
    let path = match &args.job {
        Some(job) => {
            match locate_job(
                ctx,
                job,
                args.recipe.as_deref(),
                task.as_deref(),
                &mut notes,
            )? {
                JobLog::Task(p) => p,
                JobLog::Console { text, handle } => {
                    return render_digest(ctx, &args, &text, None, None, handle.as_ref(), notes)
                        .await;
                }
            }
        }
        None => {
            let build_dir = ctx
                .workspace
                .build_dir
                .clone()
                .ok_or_else(|| Error::NotFound {
                    what: "a build directory (task logs live under <build>/tmp/work); set \
                           build_dir in sous-chef.toml or pass --build-dir"
                        .into(),
                })?;
            locate(
                ctx,
                &build_dir,
                args.recipe.as_deref(),
                task.as_deref(),
                &mut notes,
            )
            .await?
        }
    };

    let resolved = ctx.workspace.confine(&path)?;
    let bytes = fs::read(&resolved)?;
    let text = String::from_utf8_lossy(&bytes);
    let handle = ctx
        .handles
        .put(HandleKind::Log, &resolved.display().to_string(), &bytes)?;

    let run_script =
        logs::shell_trace(&text).and_then(|t| read_run_file(ctx, &resolved, &t.run_file));
    render_digest(
        ctx,
        &args,
        &text,
        Some(&resolved),
        run_script.as_deref(),
        Some(&handle),
        notes,
    )
    .await
}

/// Digests `text` (a task log at `log_path`, or a job's console output) with a pointer to
/// the task definition, and renders it.
async fn render_digest(
    ctx: &Ctx,
    args: &Args,
    text: &str,
    log_path: Option<&Path>,
    run_script: Option<&str>,
    handle: Option<&Handle>,
    mut notes: Vec<String>,
) -> Result<String> {
    let text_meta = logs::meta_from_text(text);
    let meta = match log_path {
        Some(p) => logs::meta_from_path_in(p, Some(&ctx.workspace)).or(text_meta),
        None => text_meta,
    };
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
        text,
        log_path,
        run_script,
        ws: Some(&ctx.workspace),
        handle,
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

/// What `log job=...` digests.
enum JobLog {
    /// The failing task's log, named in the job's output.
    Task(PathBuf),
    /// The job's own output, when it names no readable task log.
    Console {
        text: String,
        handle: Option<Handle>,
    },
}

/// The log of a finished job: the first failed task's log (`ERROR: Task (...) failed`
/// after `Logfile of failure stored in:`), then any other log the output names
/// ([`logs::failure_logfiles`]), filtered by `recipe` and `task`; else the console output.
fn locate_job(
    ctx: &Ctx,
    id: &str,
    recipe: Option<&str>,
    task: Option<&str>,
    notes: &mut Vec<String>,
) -> Result<JobLog> {
    ctx.jobs.settle(&ctx.handles);
    let job = ctx.jobs.get(id).ok_or_else(|| Error::NotFound {
        what: format!("job {id:?}; `bitbake op=status` shows the last job"),
    })?;
    if job.state == JobState::Running {
        return Err(Error::Invalid {
            msg: format!(
                "{} is still running; poll with bitbake op=status, then call log again",
                job.id
            ),
        });
    }
    let bytes = match &job.handle {
        Some(h) => ctx.handles.get(h)?,
        None => fs::read(&job.output)?,
    };
    let out = String::from_utf8_lossy(&bytes).into_owned();
    let mut candidates: Vec<PathBuf> = logs::failed_tasks(&out)
        .into_iter()
        .filter_map(|t| t.log)
        .collect();
    for p in logs::failure_logfiles(&out) {
        if !candidates.contains(&p) {
            candidates.push(p);
        }
    }
    candidates.retain(|p| {
        let meta = logs::meta_from_path_in(p, Some(&ctx.workspace));
        task.is_none_or(|t| meta.task.as_deref() == Some(t))
            && recipe.is_none_or(|r| meta.recipe.as_deref() == Some(r))
    });
    if let Some(p) = candidates
        .iter()
        .find(|p| ctx.workspace.confine(p).is_ok_and(|r| r.is_file()))
    {
        return Ok(JobLog::Task(p.clone()));
    }
    match candidates.first() {
        Some(p) => notes.push(format!(
            "note: the task log {} is not readable; digest of the {} output",
            p.display(),
            job.id
        )),
        None => notes.push(format!(
            "note: {} ({}) names no task log; digest of its output",
            job.id,
            job.state.as_str()
        )),
    }
    Ok(JobLog::Console {
        text: out,
        handle: job.handle,
    })
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

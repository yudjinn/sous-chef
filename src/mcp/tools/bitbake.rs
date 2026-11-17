//! Tool `bitbake`: run bitbake operations as background jobs. See docs/mcp-tools.md.
//!
//! Every operation except `status` and `cancel` builds an argv from an allowlist
//! ([`plan`]), checks the destructive gates, finds the build environment and starts a job
//! in `ctx.jobs` ([`crate::bitbake::jobs`]). The call returns at once with the job id;
//! `op: status` reports the state, the last lines while running and, once finished, the
//! handle of the whole output and (for a failure) a digest from
//! [`crate::bitbake::logs::digest`].
//!
//! Every user-supplied recipe, task, image and layer name goes through
//! [`validate_name`] before any argv is built: [`crate::bitbake::BbEnv::command`] cannot
//! tell a target from an option.
//!
//! One job at a time ([`crate::bitbake::jobs`], module docs): a new operation while a
//! read-only job (`env`, `layers`) runs waits for it, up to [`QUEUE_WAIT`], then starts;
//! while any other job runs it is refused at once with [`not_started_note`].

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bitbake::env::KILL_GRACE;
use crate::bitbake::jobs::{self, JobInfo, JobSpec, JobState, QUEUE_WAIT, Started};
use crate::bitbake::logs::{self, Digest, DigestInput};
use crate::bitbake::{BbEnv, HostEnv, validate_name};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::handles::HandleKind;
use crate::mcp::Ctx;
use crate::render::{self, DEFAULT_BUDGET_TOKENS, Format, Table};
use crate::workspace::Workspace;

/// Lines of output shown for a running job.
pub const RUNNING_TAIL_LINES: usize = 5;
/// Lines of output shown for a finished job that did not fail.
pub const FINISHED_TAIL_LINES: usize = 3;

/// Operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// `bitbake <target>`.
    Build,
    /// `bitbake -c <task> <target>`.
    Task,
    /// `bitbake -e [<target>]`, stored as a handle.
    Env,
    /// `bitbake -c clean|cleansstate|cleanall <target>`.
    Clean,
    /// `devtool modify|status|finish|reset|build`.
    Devtool,
    /// `bitbake-layers show-layers|show-appends|show-recipes|show-overlayed`.
    Layers,
    /// State of the current or last job.
    Status,
    /// Cancel the running job.
    Cancel,
}

impl Op {
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Build => "build",
            Op::Task => "task",
            Op::Env => "env",
            Op::Clean => "clean",
            Op::Devtool => "devtool",
            Op::Layers => "layers",
            Op::Status => "status",
            Op::Cancel => "cancel",
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// `build`, `task`, `env`, `clean`, `devtool`, `layers`, `status` or `cancel`.
    pub op: Op,
    /// Recipe or image.
    pub target: Option<String>,
    /// For `op: task`, e.g. `compile`.
    pub task: Option<String>,
    /// Op-specific arguments, validated against an allowlist: build `continue` (-k); task
    /// `force` (-f); clean `clean` (default), `cleansstate` or `cleanall`; devtool the
    /// subcommand `modify`, `status`, `finish` (then the destination layer), `reset` or
    /// `build`; layers `show-layers` (default), `show-appends`, `show-recipes` or
    /// `show-overlayed`.
    #[serde(default)]
    pub args: Vec<String>,
    /// Must be true for destructive operations (cleansstate, cleanall, devtool reset,
    /// devtool finish), which also need `[bitbake] allow_destructive = true`.
    #[serde(default)]
    pub confirm: bool,
    /// For `op: status` and `op: cancel`: the job id (default: the running job, else the
    /// last one).
    pub job: Option<String>,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

/// What an operation runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub spec: JobSpec,
    /// Needs `confirm: true` and `bitbake.allow_destructive = true`.
    pub destructive: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<String> {
    let budget = match args
        .budget_tokens
        .unwrap_or(ctx.config.output.budget_tokens)
    {
        0 => DEFAULT_BUDGET_TOKENS,
        n => n,
    };
    let format = args.format.unwrap_or_default();
    ctx.jobs.settle(&ctx.handles);
    match args.op {
        Op::Status => {
            let job = pick_job(ctx, args.job.as_deref())?;
            report(ctx, &job, &[], format, budget)
        }
        Op::Cancel => {
            let id = match &args.job {
                Some(id) => id.clone(),
                None => {
                    ctx.jobs
                        .running()
                        .ok_or_else(|| Error::Invalid {
                            msg: "no job is running; `bitbake op=status` shows the last one".into(),
                        })?
                        .id
                }
            };
            ctx.jobs.cancel(&id)?;
            // SIGTERM, up to KILL_GRACE, then SIGKILL: wait a little longer than that.
            let job = ctx
                .jobs
                .wait(&id, KILL_GRACE + Duration::from_secs(3))
                .await
                .ok_or_else(|| Error::NotFound { what: id.clone() })?;
            ctx.jobs.settle(&ctx.handles);
            let job = ctx.jobs.get(&job.id).unwrap_or(job);
            let notes = if job.state == JobState::Running {
                vec!["note: still stopping; poll with op=status".to_string()]
            } else {
                Vec::new()
            };
            report(ctx, &job, &notes, format, budget)
        }
        _ => {
            let plan = plan(&ctx.config, &ctx.workspace, &args)?;
            check_destructive(&ctx.config, &plan, args.confirm)?;
            let env = BbEnv::probe(&ctx.config, &ctx.workspace, &HostEnv::from_process()).map_err(
                |reason| Error::Invalid {
                    msg: format!("no bitbake environment: {reason}"),
                },
            )?;
            let started = ctx
                .jobs
                .start_or_queue(&env, plan.spec, &ctx.jobs_dir(), QUEUE_WAIT, |_| {})
                .await?;
            match started {
                Started::Started(job) => {
                    let notes = [format!(
                        "started; poll with bitbake op=status job={}, stop with op=cancel",
                        job.id
                    )];
                    report(ctx, &job, &notes, format, budget)
                }
                Started::AlreadyRunning(job) => {
                    report(ctx, &job, &[not_started_note(&job)], format, budget)
                }
            }
        }
    }
}

/// Why nothing was started while `job` runs (module docs of [`crate::bitbake::jobs`]).
pub fn not_started_note(job: &JobInfo) -> String {
    let what = match &job.target {
        Some(t) => format!("{} {t}", job.op),
        None => job.op.clone(),
    };
    if job.is_read_only() {
        format!(
            "not started: {} ({what}) is already running (one job at a time) and did not end \
             within {} s of waiting; poll with op=status job={} and call again once it has \
             ended, or stop it with op=cancel",
            job.id,
            QUEUE_WAIT.as_secs(),
            job.id
        )
    } else {
        format!(
            "not started: {} ({what}) is already running (one job at a time): it holds \
             BitBake's lock on the build directory, and a second BitBake client waits for \
             that lock and gives up after about 20 s, so sous-chef starts nothing while a \
             build, task, clean or devtool job runs; poll with op=status job={} and call \
             again once it has ended, or stop it with op=cancel",
            job.id, job.id
        )
    }
}

fn pick_job(ctx: &Ctx, id: Option<&str>) -> Result<JobInfo> {
    match id {
        Some(id) => ctx.jobs.get(id).ok_or_else(|| Error::NotFound {
            what: format!("job {id:?}; omit job to see the last one"),
        }),
        None => ctx
            .jobs
            .running()
            .or_else(|| ctx.jobs.last())
            .ok_or_else(|| Error::NotFound {
                what: "a bitbake job: none has been started; start one with op=build".into(),
            }),
    }
}

fn invalid(msg: String) -> Error {
    Error::Invalid { msg }
}

fn need_target<'a>(args: &'a Args, op: &str) -> Result<&'a str> {
    let t = args
        .target
        .as_deref()
        .ok_or_else(|| invalid(format!("op={op} needs target (a recipe or image)")))?;
    validate_name(t)?;
    Ok(t)
}

fn optional_target(args: &Args) -> Result<Option<&str>> {
    match args.target.as_deref() {
        Some(t) => {
            validate_name(t)?;
            Ok(Some(t))
        }
        None => Ok(None),
    }
}

/// Refuses any `args` entry outside `allowed`.
fn check_args(op: &str, args: &[String], allowed: &[&str]) -> Result<()> {
    if let Some(bad) = args.iter().find(|a| !allowed.contains(&a.as_str())) {
        let allowed = if allowed.is_empty() {
            "none".to_string()
        } else {
            allowed.join(", ")
        };
        return Err(invalid(format!(
            "op={op}: argument {bad:?} is not allowed; allowed: {allowed}"
        )));
    }
    Ok(())
}

/// The argv for an operation, from the allowlist. Validates every name. Does not run
/// anything and does not check the destructive gates ([`check_destructive`]).
///
/// The forms were checked against BitBake `lib/bb/main.py` (`-k`/`--continue`,
/// `-f`/`--force`, `-c`/`--cmd`, `-e`/`--environment`), `lib/bblayers/query.py` and
/// OE-core `scripts/lib/devtool/standard.py` and `build.py`; see "Running BitBake" in
/// docs/bitbake-semantics.md.
pub fn plan(_config: &Config, ws: &Workspace, args: &Args) -> Result<Plan> {
    let op = args.op.as_str();
    let a = &args.args;
    let mut destructive = false;
    let (program, argv, kind, target): (&str, Vec<String>, HandleKind, Option<String>) = match args
        .op
    {
        Op::Build => {
            check_args(op, a, &["continue"])?;
            let t = need_target(args, op)?;
            let mut v = Vec::new();
            if a.iter().any(|x| x == "continue") {
                v.push("-k".to_string());
            }
            v.push(t.to_string());
            ("bitbake", v, HandleKind::Build, Some(t.to_string()))
        }
        Op::Task => {
            check_args(op, a, &["force"])?;
            let t = need_target(args, op)?;
            let task = args
                .task
                .as_deref()
                .map(str::trim)
                .ok_or_else(|| invalid("op=task needs task, e.g. compile".into()))?;
            // BitBake adds `do_` itself (cooker `buildTargets`); pass the short form.
            let task = task.strip_prefix("do_").unwrap_or(task);
            validate_name(task)?;
            let mut v = vec!["-c".to_string(), task.to_string()];
            if a.iter().any(|x| x == "force") {
                v.push("-f".to_string());
            }
            v.push(t.to_string());
            ("bitbake", v, HandleKind::Build, Some(t.to_string()))
        }
        Op::Env => {
            check_args(op, a, &[])?;
            let t = optional_target(args)?;
            let mut v = vec!["-e".to_string()];
            v.extend(t.map(str::to_string));
            ("bitbake", v, HandleKind::Env, t.map(str::to_string))
        }
        Op::Clean => {
            const CLEANS: [&str; 3] = ["clean", "cleansstate", "cleanall"];
            check_args(op, a, &CLEANS)?;
            if a.len() > 1 {
                return Err(invalid(format!(
                    "op=clean takes one of {}",
                    CLEANS.join(", ")
                )));
            }
            let t = need_target(args, op)?;
            let which = a.first().map(String::as_str).unwrap_or("clean");
            destructive = which != "clean";
            let v = vec!["-c".to_string(), which.to_string(), t.to_string()];
            ("bitbake", v, HandleKind::Build, Some(t.to_string()))
        }
        Op::Devtool => {
            const SUBS: [&str; 5] = ["modify", "status", "finish", "reset", "build"];
            let sub = a.first().map(String::as_str).ok_or_else(|| {
                invalid(format!(
                    "op=devtool needs args: [subcommand], one of {}",
                    SUBS.join(", ")
                ))
            })?;
            if !SUBS.contains(&sub) {
                return Err(invalid(format!(
                    "devtool subcommand {sub:?} is not allowed; allowed: {}",
                    SUBS.join(", ")
                )));
            }
            let extra = &a[1..];
            match sub {
                "status" => {
                    if !extra.is_empty() || args.target.is_some() {
                        return Err(invalid(
                            "devtool status takes no target or arguments".into(),
                        ));
                    }
                    ("devtool", vec!["status".into()], HandleKind::Build, None)
                }
                "finish" => {
                    destructive = true;
                    let t = need_target(args, "devtool finish")?;
                    let [layer] = extra else {
                        return Err(invalid(
                            "devtool finish needs args: [\"finish\", \"<destination layer>\"]"
                                .into(),
                        ));
                    };
                    validate_name(layer)?;
                    let known = ws.layers.iter().find(|l| &l.name == layer);
                    match known {
                        Some(l) if l.writable => {}
                        Some(_) => {
                            return Err(invalid(format!(
                                "layer {layer} is not writable; list it in writable_layers in sous-chef.toml"
                            )));
                        }
                        None => {
                            let names: Vec<&str> =
                                ws.layers.iter().map(|l| l.name.as_str()).collect();
                            return Err(Error::NotFound {
                                what: format!("layer {layer}; known layers: {}", names.join(", ")),
                            });
                        }
                    }
                    let v = vec!["finish".into(), t.to_string(), layer.clone()];
                    ("devtool", v, HandleKind::Build, Some(t.to_string()))
                }
                _ => {
                    if !extra.is_empty() {
                        return Err(invalid(format!(
                            "devtool {sub} takes only target, no further arguments"
                        )));
                    }
                    destructive = sub == "reset";
                    let t = need_target(args, &format!("devtool {sub}"))?;
                    let v = vec![sub.to_string(), t.to_string()];
                    ("devtool", v, HandleKind::Build, Some(t.to_string()))
                }
            }
        }
        Op::Layers => {
            const SUBS: [&str; 4] = [
                "show-layers",
                "show-appends",
                "show-recipes",
                "show-overlayed",
            ];
            check_args(op, a, &SUBS)?;
            if a.len() > 1 {
                return Err(invalid(format!(
                    "op=layers takes one of {}",
                    SUBS.join(", ")
                )));
            }
            let sub = a.first().map(String::as_str).unwrap_or("show-layers");
            let t = optional_target(args)?;
            let takes_pn = matches!(sub, "show-recipes" | "show-appends");
            if t.is_some() && !takes_pn {
                return Err(invalid(format!(
                    "bitbake-layers {sub} takes no target; only show-recipes and show-appends do"
                )));
            }
            let mut v = vec![sub.to_string()];
            v.extend(t.map(str::to_string));
            ("bitbake-layers", v, HandleKind::Text, t.map(str::to_string))
        }
        Op::Status | Op::Cancel => {
            return Err(invalid(format!("op={op} does not start a job")));
        }
    };
    Ok(Plan {
        spec: JobSpec {
            op: op.to_string(),
            target,
            program: program.to_string(),
            args: argv,
            kind,
        },
        destructive,
    })
}

/// Destructive operations need `confirm: true` and `[bitbake] allow_destructive = true`.
pub fn check_destructive(config: &Config, plan: &Plan, confirm: bool) -> Result<()> {
    if !plan.destructive {
        return Ok(());
    }
    let what = plan.spec.command_line();
    if !confirm {
        return Err(invalid(format!(
            "`{what}` is destructive: pass confirm: true (it also needs \
             bitbake.allow_destructive = true, under [bitbake] in sous-chef.toml)"
        )));
    }
    if !config.bitbake.allow_destructive {
        return Err(invalid(format!(
            "`{what}` is destructive and bitbake.allow_destructive is false; set \
             allow_destructive = true under [bitbake] in sous-chef.toml and restart the server"
        )));
    }
    Ok(())
}

/// The whole output of a job: its handle, else its output file.
fn job_output(ctx: &Ctx, job: &JobInfo) -> Option<String> {
    let bytes = match &job.handle {
        Some(h) => ctx.handles.get(h).ok()?,
        None => std::fs::read(&job.output).ok()?,
    };
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[derive(Debug, Serialize)]
struct JsonReport<'a> {
    job: &'a JobInfo,
    tail: Vec<String>,
    digest: Option<Digest>,
    notes: Vec<String>,
}

/// The job as a `field value` table: what ran, its state, the last lines while running
/// or after success, and for a failure the digest of its output.
fn report(
    ctx: &Ctx,
    job: &JobInfo,
    notes: &[String],
    format: Format,
    budget: usize,
) -> Result<String> {
    let mut notes: Vec<String> = notes.to_vec();
    let (tail, digest) = match job.state {
        JobState::Running => (jobs::tail(&job.output, RUNNING_TAIL_LINES), None),
        JobState::Failed => {
            let d = job_output(ctx, job).map(|text| {
                logs::digest(&DigestInput {
                    text: &text,
                    ws: Some(&ctx.workspace),
                    handle: job.handle.as_ref(),
                    ..DigestInput::default()
                })
            });
            if !logs_hint_useless(job) {
                notes.push(format!("task log digest: log job={}", job.id));
            }
            (Vec::new(), d)
        }
        JobState::Succeeded | JobState::Cancelled => {
            let tail = job_output(ctx, job)
                .map(|t| jobs::tail_of(&t, FINISHED_TAIL_LINES))
                .unwrap_or_default();
            if let Some(h) = &job.handle {
                notes.push(match job.kind {
                    HandleKind::Env => format!("slice handle={h} var=NAME | grep=RE"),
                    _ => format!("slice handle={h} page=1 | grep=RE"),
                });
            }
            (tail, None)
        }
    };

    if format == Format::Json {
        let r = JsonReport {
            job,
            tail,
            digest,
            notes,
        };
        return serde_json::to_string(&r).map_err(|e| invalid(format!("serialising: {e}")));
    }

    let mut t = Table::new(&["field", "value"]);
    t.row(&["job", &job.id]);
    t.row(&["op", &job.op]);
    if let Some(target) = &job.target {
        t.row(&["target", target]);
    }
    t.row(&["command", &render::snippet(&job.command)]);
    t.row(&["state", job.state.as_str()]);
    t.row(&["elapsed", &format!("{}s", job.elapsed_secs)]);
    if let Some(c) = job.exit_code {
        t.row(&["exit", &c.to_string()]);
    }
    // A failure's digest carries the handle itself.
    if digest.is_none()
        && let Some(h) = &job.handle
    {
        t.row(&["handle", &h.0]);
    }
    if let Some(e) = &job.handle_error {
        t.row(&["output", &render::snippet(&format!("not stored: {e}"))]);
    }
    for l in &tail {
        t.row(&["tail", &render::snippet(l)]);
    }
    for n in &notes {
        t.note(n);
    }
    let mut out = t.render(budget);
    if let Some(d) = digest {
        out.push_str(&d.render(Format::Text, budget));
    }
    Ok(out)
}

/// `log job=` finds task logs only for operations that run tasks.
fn logs_hint_useless(job: &JobInfo) -> bool {
    matches!(job.op.as_str(), "env" | "layers")
}

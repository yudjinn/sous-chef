//! `var` with `authoritative: true`: answers from `bitbake -e`. Called by `var::run` (T5.5).
//!
//! **How it runs.** `bitbake -e [<recipe>]` runs as a job in the server's [`JobTable`]
//! (operation `env`, so `bitbake op=status` and `op=cancel` see it), and the tool waits for
//! it up to [`WAIT`] (about 15 s on a real environment). The job table runs one job at a
//! time because BitBake allows one client per build directory, so:
//!
//! - when another job is running (a build, say), nothing is started and the tool returns an
//!   error naming that job, unless a cached dump for the unchanged tree exists, which is
//!   used without running anything;
//! - when its own job is still running after [`WAIT`], the tool returns a note with the job
//!   id; the next call with the same recipe and an unchanged tree waits for that job
//!   instead of starting another.
//!
//! The output is stored under an `env` handle (when the job settles), then parsed with
//! [`crate::bitbake::envdump`], keeping only the variable asked for, its `${...}`
//! references and `OVERRIDES`.
//!
//! **Cache.** One entry per recipe (or the global configuration) in
//! `<cache dir>/env-cache.json`: the tree key and the handle. The key is a sha256 over the
//! recipe name and the indexed sha256 of every `.conf`, `.bbclass` and `.inc` file of the
//! workspace and of the recipe's `.bb` files and their bbappends, after an index refresh;
//! an unchanged tree reuses the handle, across server restarts too, while the handle store
//! keeps it. A change anywhere in that set runs BitBake again.
//!
//! **Names.** Only the recipe name reaches BitBake, as an argument checked by
//! [`validate_name`]. The variable name is never passed to any command: it is a key into
//! the parsed dump, so `FILES:${PN}` or `SRC_URI:append` need no relaxation of
//! `validate_name`. A `${NAME}` in it is expanded with the dump's own final values.
//!
//! **Output.** The table of static mode (`status op override loc snippet`), one row per
//! operation in BitBake's history, in the order BitBake printed it. BitBake records that an
//! operation ran, not whether it contributes, so `status` is `inactive` when an override
//! the operation depends on is not in BitBake's final `OVERRIDES` and `active` otherwise.
//! `loc` is a workspace pointer when the file is in the workspace, else the absolute path
//! (or BitBake's own `file.py:line` for an operation made by python code). Then the notes:
//! `final:` (cut at [`MAX_FINAL_CHARS`] with the full text in a `value` handle, as in static
//! mode), export state, the source line naming BitBake and the handle, and a count of
//! lines the parser did not recognise.
//!
//! `machine` and `distro` are refused: `bitbake -e` uses the build configuration's
//! `MACHINE` and `DISTRO`, and the runner passes no environment to BitBake.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::var::{Args, MAX_FINAL_CHARS, status_str};
use crate::bitbake::envdump::{self, EnvOp, Export, OpKind, Variable};
use crate::bitbake::{BbEnv, HostEnv, JobInfo, JobSpec, JobState, Started, validate_name};
use crate::error::{Error, Result};
use crate::eval::OpStatus;
use crate::handles::{Handle, HandleKind};
use crate::index::{Index, query};
use crate::mcp::Ctx;
use crate::render::{self, Table};
use crate::types::FileKind;

/// How long a call waits for its `bitbake -e` job before returning the job id.
pub const WAIT: Duration = Duration::from_secs(120);

/// The cache file, in the cache directory next to `handles/`.
pub const CACHE_FILE: &str = "env-cache.json";

/// Renders the history and final value of `args.name` from `bitbake -e`, in the same table
/// format as static mode.
pub async fn run(ctx: &Ctx, args: &Args) -> Result<String> {
    run_with(ctx, args, WAIT).await
}

/// [`run`] with an explicit wait for the job (a test seam).
pub async fn run_with(ctx: &Ctx, args: &Args, wait: Duration) -> Result<String> {
    for (what, v) in [("machine", &args.machine), ("distro", &args.distro)] {
        if v.as_deref().is_some_and(|s| !s.trim().is_empty()) {
            return Err(Error::Invalid {
                msg: format!(
                    "{what} cannot be set with authoritative=true: bitbake -e uses MACHINE and DISTRO from the build configuration; drop {what} or use static mode"
                ),
            });
        }
    }
    let (name, flag) = split_flag(&args.name)?;
    let recipe = args
        .recipe
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());
    if let Some(r) = recipe {
        validate_name(r).map_err(|_| Error::Invalid {
            msg: format!(
                "authoritative=true needs a recipe name BitBake accepts (letters, digits and _ . + - /), such as curl; {r:?} is not one"
            ),
        })?;
    }
    let env = BbEnv::probe(&ctx.config, &ctx.workspace, &HostEnv::from_process())
        .map_err(|_| Error::BitbakeUnavailable)?;

    let mut index = ctx.index.lock().await;
    let report = index.refresh(&ctx.workspace)?;
    let key = tree_key(&index, recipe)?;
    drop(index);

    let (handle, reused) = match obtain(ctx, &env, recipe, &key, wait).await? {
        Obtained::Handle { handle, reused } => (handle, reused),
        Obtained::Running(job) => return Ok(still_running(&job, recipe, wait)),
    };

    let bytes = ctx.handles.get(&handle)?;
    let wanted = wanted_names(&name);
    let mut dump = envdump::parse_filtered(Cursor::new(&bytes), |n| wanted.matches(n))?;
    if !dump.stats.found_header {
        forget(ctx, recipe);
        return Err(Error::Invalid {
            msg: format!(
                "the output of {} holds no environment dump; read it with slice handle={handle}",
                command_line(recipe)
            ),
        });
    }
    // `FILES:${PN}`: expand with BitBake's own values, then look again if needed.
    let lookup = if name.contains("${") {
        dump.expand(&name)
    } else {
        name.clone()
    };
    if lookup != name && dump.variable(&lookup).is_none() && dump.function(&lookup).is_none() {
        dump = envdump::parse_filtered(Cursor::new(&bytes), |n| n == lookup || n == "OVERRIDES")?;
    }

    let overrides = dump.overrides();
    let mut t = Table::new(&["status", "op", "override", "loc", "snippet"]);
    let mut finals = Vec::new();
    if lookup != name {
        finals.push(format!("note: the name {name} expands to {lookup}"));
    }
    if let Some(f) = dump.function(&lookup) {
        let loc = match &f.file {
            Some(file) => envdump::location(Some(&ctx.workspace), file, f.line),
            None => "-".to_string(),
        };
        t.row(&[
            "active",
            if f.python { "python" } else { "shell" },
            "-",
            &loc,
            &render::snippet(&if f.python {
                format!("python {}() {{ ... }}", f.name)
            } else {
                format!("{}() {{ ... }}", f.name)
            }),
        ]);
        let n = f.body.lines().count();
        finals.push(format!(
            "final: {} function {}, {n} {} as BitBake runs it; slice handle={handle} var={} shows the body",
            if f.python { "python" } else { "shell" },
            f.name,
            if n == 1 { "line" } else { "lines" },
            f.name
        ));
    } else if let Some(v) = dump.variable(&lookup) {
        match &flag {
            Some(fl) => {
                flag_rows(&mut t, ctx, v, fl, &overrides);
                finals.push(flag_final(ctx, v, fl, recipe)?);
            }
            None => {
                for op in v.value_ops() {
                    op_row(&mut t, ctx, v, op, &overrides);
                }
                finals.push(final_line(ctx, v, recipe)?);
                if args.flags {
                    for fl in v.flag_names() {
                        flag_rows(&mut t, ctx, v, &fl, &overrides);
                        finals.push(flag_final(ctx, v, &fl, recipe)?);
                    }
                }
            }
        }
        match v.export {
            Export::Exported => finals.push(format!(
                "note: exported to task environments (bitbake -e prints export {lookup}=...)"
            )),
            Export::Unexported => finals.push(format!(
                "note: unexported (bitbake -e prints unset {lookup}): BitBake removes it from task environments"
            )),
            Export::No => {}
        }
        if t.is_empty() && v.ops.is_empty() && !v.no_history {
            t.note(&format!("note: BitBake recorded no operation on {lookup}"));
        }
    } else {
        finals.push(format!(
            "note: BitBake has no variable or function {lookup} {}; names are case-sensitive",
            scope_text(recipe)
        ));
    }
    for f in &finals {
        t.note(f);
    }
    t.note(&format!(
        "source: BitBake ({}), handle {handle}{}; slice handle={handle} var={lookup} shows the raw block",
        command_line(recipe),
        if reused {
            ", reused: the tree is unchanged since it ran"
        } else {
            ""
        }
    ));
    t.note("note: status is inactive when an override the operation needs is not in BitBake's final OVERRIDES, else active (BitBake records that an operation ran, not whether it contributes)");
    if dump.stats.skipped > 0 {
        t.note(&format!(
            "note: {} line(s) of the dump were not recognised and were skipped",
            dump.stats.skipped
        ));
    }
    if recipe.is_none() {
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

// ---- the names the parse keeps ----

struct Wanted {
    names: Vec<String>,
}

impl Wanted {
    fn matches(&self, n: &str) -> bool {
        self.names.iter().any(|w| w == n)
    }
}

/// The variable, the names its `${...}` references, and `OVERRIDES`.
fn wanted_names(name: &str) -> Wanted {
    let mut names = vec![name.to_string(), "OVERRIDES".to_string()];
    names.extend(envdump::references(name));
    Wanted { names }
}

// ---- rows ----

/// Whether every override `op` depends on is in `overrides`.
fn status(op: &EnvOp, overrides: &[String]) -> &'static str {
    if op.conditions().all(|c| overrides.iter().any(|o| o == c)) {
        "active"
    } else {
        status_str(OpStatus::InactiveOverride)
    }
}

/// The operator column: the statement's operator when the operation came from a
/// statement, else BitBake's own operation name.
fn op_text(op: &EnvOp) -> String {
    if op.func.is_some() {
        return match &op.kind {
            OpKind::RenameFrom(_) => "rename".into(),
            _ => op.raw.rsplit(':').next().unwrap_or(&op.raw).to_string(),
        };
    }
    match &op.kind {
        OpKind::Set | OpKind::OverrideAppend | OpKind::OverridePrepend | OpKind::OverrideRemove => {
            "=".into()
        }
        OpKind::Default => "?=".into(),
        OpKind::WeakDefault => "??=".into(),
        OpKind::Immediate => ":=".into(),
        OpKind::Append => "+=".into(),
        OpKind::Prepend => "=+".into(),
        OpKind::AppendDot => ".=".into(),
        OpKind::PrependDot => "=.".into(),
        OpKind::Exported | OpKind::EnvExport => "export".into(),
        OpKind::Del => "unset".into(),
        OpKind::DelFlag => "unset".into(),
        OpKind::RenameFrom(_) | OpKind::RenameTo => "rename".into(),
        OpKind::FromEnv => "env".into(),
        OpKind::Other(s) => s.clone(),
    }
}

/// The overrides as written (`append:class-target`), like static mode.
fn override_text(op: &EnvOp) -> Vec<String> {
    let mut parts: Vec<String> = op.overrides.clone();
    let word = match op.kind {
        OpKind::OverrideAppend => Some("append"),
        OpKind::OverridePrepend => Some("prepend"),
        OpKind::OverrideRemove => Some("remove"),
        _ => None,
    };
    if let Some(w) = word {
        parts.push(w.to_string());
        parts.extend(op.condition.iter().cloned());
    }
    parts
}

fn snippet(v: &Variable, op: &EnvOp) -> String {
    let base = v.name.split(':').next().unwrap_or(&v.name);
    let ovr = override_text(op);
    let mut name = if ovr.is_empty() {
        v.name.clone()
    } else {
        format!("{base}:{}", ovr.join(":"))
    };
    if let Some(f) = &op.flag {
        name.push_str(&format!("[{f}]"));
    }
    let text = match &op.kind {
        OpKind::RenameFrom(old) => format!("{old} renamed to {name}"),
        OpKind::FromEnv => format!("{name} = \"{}\" (from the environment)", op.value),
        _ => match &op.func {
            Some(func) => format!("{func}: {name} {} \"{}\"", op.raw, op.value),
            None => format!("{name} {} \"{}\"", op_text(op), op.value),
        },
    };
    render::snippet(&text)
}

fn op_row(t: &mut Table, ctx: &Ctx, v: &Variable, op: &EnvOp, overrides: &[String]) {
    let ovr = override_text(op);
    let mut ovr = if ovr.is_empty() {
        "-".to_string()
    } else {
        ovr.join(":")
    };
    if let Some(f) = &op.flag {
        ovr = if ovr == "-" {
            format!("[{f}]")
        } else {
            format!("{ovr}[{f}]")
        };
    }
    t.row(&[
        status(op, overrides),
        &op_text(op),
        &ovr,
        &op.location(Some(&ctx.workspace)),
        &snippet(v, op),
    ]);
}

fn flag_rows(t: &mut Table, ctx: &Ctx, v: &Variable, flag: &str, overrides: &[String]) {
    for op in v.flag_ops(flag) {
        op_row(t, ctx, v, op, overrides);
    }
}

fn final_line(ctx: &Ctx, v: &Variable, recipe: Option<&str>) -> Result<String> {
    if let Some(e) = &v.error {
        return Ok(format!("final: BitBake could not expand {}: {e}", v.name));
    }
    match (&v.value, v.export) {
        (Some(text), _) => Ok(format!(
            "final: {}",
            shown(ctx, text, &v.name, None, recipe)?
        )),
        (None, Export::Unexported) => Ok(format!(
            "final: not printed (unexported); pre-expansion value: {}",
            shown(
                ctx,
                v.pre_expansion.as_deref().unwrap_or("None"),
                &v.name,
                None,
                recipe
            )?
        )),
        (None, _) => Ok(format!("final: {} is not set", v.name)),
    }
}

fn flag_final(ctx: &Ctx, v: &Variable, flag: &str, recipe: Option<&str>) -> Result<String> {
    let n = v.flag_ops(flag).count();
    if n == 0 {
        return Ok(format!("final[{flag}]: not set"));
    }
    match v.flag_value(flag) {
        Some(text) => Ok(format!(
            "final[{flag}]: {}",
            shown(ctx, &text, &v.name, Some(flag), recipe)?
        )),
        None => Ok(format!(
            "final[{flag}]: not printed by bitbake -e and not derivable from its {n} operation(s) above"
        )),
    }
}

/// The value as shown: quoted, cut at [`MAX_FINAL_CHARS`] with the full text in a
/// `value` handle.
fn shown(
    ctx: &Ctx,
    text: &str,
    name: &str,
    flag: Option<&str>,
    recipe: Option<&str>,
) -> Result<String> {
    if text.chars().count() <= MAX_FINAL_CHARS {
        return Ok(format!("\"{text}\""));
    }
    let mut command = format!("var {name}");
    if let Some(f) = flag {
        command.push_str(&format!("[{f}]"));
    }
    if let Some(r) = recipe {
        command.push_str(&format!(" recipe={r}"));
    }
    command.push_str(" authoritative=true");
    let h = ctx
        .handles
        .put(HandleKind::Value, &command, text.as_bytes())?;
    Ok(format!(
        "\"{}",
        render::truncate_value(text, MAX_FINAL_CHARS, Some(&h.0))
    ))
}

fn split_flag(name: &str) -> Result<(String, Option<String>)> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Invalid {
            msg: "variable name is empty; give a name such as SRC_URI".into(),
        });
    }
    match name.split_once('[') {
        Some((n, rest)) => match rest
            .strip_suffix(']')
            .filter(|f| !f.is_empty() && !n.is_empty())
        {
            Some(f) => Ok((n.to_string(), Some(f.to_string()))),
            None => Err(Error::Invalid {
                msg: format!("bad name {name:?}; write VAR or VAR[flag]"),
            }),
        },
        None => Ok((name.to_string(), None)),
    }
}

fn scope_text(recipe: Option<&str>) -> String {
    match recipe {
        Some(r) => format!("in bitbake -e {r}"),
        None => "in the global configuration (bitbake -e)".into(),
    }
}

fn command_line(recipe: Option<&str>) -> String {
    match recipe {
        Some(r) => format!("bitbake -e {r}"),
        None => "bitbake -e".into(),
    }
}

fn still_running(job: &JobInfo, recipe: Option<&str>, wait: Duration) -> String {
    format!(
        "{} is running as {} ({} s so far, still running after waiting {} s); call var again with authoritative=true once bitbake op=status job={} shows it finished, the dump is then reused for this tree\n",
        command_line(recipe),
        job.id,
        job.elapsed_secs,
        wait.as_secs(),
        job.id
    )
}

// ---- the tree key ----

/// sha256 over the recipe name and the indexed sha256 of every `.conf`, `.bbclass` and
/// `.inc` file and of the recipe's `.bb` files and their bbappends. A recipe name the
/// index does not know (a `-native` variant, say) is looked up without its `-native`,
/// `-cross` or `nativesdk-` part; when that finds nothing either, every `.bb` and
/// `.bbappend` counts. The caller has refreshed the index.
pub fn tree_key(index: &Index, recipe: Option<&str>) -> Result<String> {
    let mut files: BTreeMap<(String, PathBuf), String> = BTreeMap::new();
    let mut add = |f: &query::FileRow| {
        files.insert((f.layer.clone(), f.path.clone()), f.sha256.clone());
    };
    for f in query::files_by_kind(index, &[FileKind::Conf, FileKind::Class, FileKind::Include])? {
        add(&f);
    }
    if let Some(pn) = recipe {
        let mut rows = Vec::new();
        for candidate in candidates(pn) {
            rows = query::find_recipes(index, &candidate)?;
            if !rows.is_empty() {
                break;
            }
        }
        if rows.is_empty() {
            for f in query::files_by_kind(index, &[FileKind::Recipe, FileKind::Append])? {
                add(&f);
            }
        }
        for r in &rows {
            add(&r.file);
            for a in query::appends_for(index, r.file.id)? {
                add(&a);
            }
        }
    }
    let mut h = Sha256::new();
    h.update(recipe.unwrap_or("").as_bytes());
    h.update([0u8]);
    for ((layer, path), sha) in &files {
        h.update(layer.as_bytes());
        h.update([0u8]);
        h.update(path.to_string_lossy().as_bytes());
        h.update([0u8]);
        h.update(sha.as_bytes());
        h.update(b"\n");
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn candidates(pn: &str) -> Vec<String> {
    let mut out = vec![pn.to_string()];
    for suffix in ["-native", "-cross", "-crosssdk", "-cross-canadian"] {
        if let Some(base) = pn.strip_suffix(suffix) {
            out.push(base.to_string());
        }
    }
    if let Some(base) = pn.strip_prefix("nativesdk-") {
        out.push(base.to_string());
    }
    out
}

// ---- the cache and the job ----

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    #[serde(default)]
    entries: BTreeMap<String, CacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    key: String,
    handle: Option<String>,
    /// A job started for this key by the process `pid`, not settled yet.
    job: Option<String>,
    pid: Option<u32>,
}

enum Obtained {
    Handle { handle: Handle, reused: bool },
    Running(JobInfo),
}

fn cache_dir(ctx: &Ctx) -> PathBuf {
    let h = ctx.handles.dir();
    h.parent().unwrap_or(h).to_path_buf()
}

fn cache_path(ctx: &Ctx) -> PathBuf {
    cache_dir(ctx).join(CACHE_FILE)
}

fn label(recipe: Option<&str>) -> String {
    recipe.unwrap_or("").to_string()
}

fn load(path: &Path) -> CacheFile {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(path: &Path, cache: &CacheFile) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(cache).map_err(|e| Error::Invalid {
        msg: format!("cannot write {}: {e}", path.display()),
    })?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn remember(ctx: &Ctx, recipe: Option<&str>, entry: CacheEntry) -> Result<()> {
    let path = cache_path(ctx);
    let mut cache = load(&path);
    cache.entries.insert(label(recipe), entry);
    save(&path, &cache)
}

fn forget(ctx: &Ctx, recipe: Option<&str>) {
    let path = cache_path(ctx);
    let mut cache = load(&path);
    if cache.entries.remove(&label(recipe)).is_some() {
        let _ = save(&path, &cache);
    }
}

/// The cached handle for this key, the result of this key's pending job, or a new job.
async fn obtain(
    ctx: &Ctx,
    env: &BbEnv,
    recipe: Option<&str>,
    key: &str,
    wait: Duration,
) -> Result<Obtained> {
    let cached = load(&cache_path(ctx)).entries.remove(&label(recipe));
    if let Some(e) = cached.filter(|e| e.key == key) {
        if let Some(h) = e.handle.as_deref().and_then(|h| h.parse::<Handle>().ok())
            && ctx
                .handles
                .meta(&h)
                .is_ok_and(|m| m.kind == HandleKind::Env)
        {
            return Ok(Obtained::Handle {
                handle: h,
                reused: true,
            });
        }
        if let (Some(job), Some(pid)) = (&e.job, e.pid)
            && pid == std::process::id()
            && ctx.jobs.get(job).is_some()
        {
            return finish(ctx, recipe, key, job, wait).await;
        }
    }

    if let Some(running) = ctx.jobs.running() {
        return Err(busy(&running));
    }
    let mut args = vec!["-e".to_string()];
    args.extend(recipe.map(str::to_string));
    let spec = JobSpec {
        op: "env".into(),
        target: recipe.map(str::to_string),
        program: "bitbake".into(),
        args,
        kind: HandleKind::Env,
    };
    let job = match ctx.jobs.start(env, spec, &cache_dir(ctx).join("jobs"))? {
        Started::Started(j) => j,
        Started::AlreadyRunning(j) => return Err(busy(&j)),
    };
    remember(
        ctx,
        recipe,
        CacheEntry {
            key: key.to_string(),
            handle: None,
            job: Some(job.id.clone()),
            pid: Some(std::process::id()),
        },
    )?;
    finish(ctx, recipe, key, &job.id, wait).await
}

/// Waits for job `id`, settles it and records its handle.
async fn finish(
    ctx: &Ctx,
    recipe: Option<&str>,
    key: &str,
    id: &str,
    wait: Duration,
) -> Result<Obtained> {
    let job = ctx
        .jobs
        .wait(id, wait)
        .await
        .ok_or_else(|| Error::NotFound {
            what: format!("job {id}"),
        })?;
    if job.state == JobState::Running {
        return Ok(Obtained::Running(job));
    }
    ctx.jobs.settle(&ctx.handles);
    let job = ctx.jobs.get(id).unwrap_or(job);
    let handle = match (&job.handle, &job.handle_error) {
        (Some(h), _) => h.clone(),
        (None, Some(e)) => {
            forget(ctx, recipe);
            return Err(Error::Invalid {
                msg: format!("the output of {} could not be stored: {e}", job.command),
            });
        }
        (None, None) => {
            forget(ctx, recipe);
            return Err(Error::Invalid {
                msg: format!("the output of {} was not stored; run it again", job.command),
            });
        }
    };
    if job.state != JobState::Succeeded {
        forget(ctx, recipe);
        let errors = first_errors(ctx, &handle);
        return Err(Error::Invalid {
            msg: format!(
                "{} {} ({}){}; the output is in slice handle={handle}",
                job.command,
                job.state.as_str(),
                job.exit_code
                    .map_or_else(|| "no exit status".to_string(), |c| format!("exit {c}")),
                if errors.is_empty() {
                    String::new()
                } else {
                    format!(": {}", errors.join(" | "))
                }
            ),
        });
    }
    remember(
        ctx,
        recipe,
        CacheEntry {
            key: key.to_string(),
            handle: Some(handle.0.clone()),
            job: None,
            pid: None,
        },
    )?;
    Ok(Obtained::Handle {
        handle,
        reused: false,
    })
}

/// The first three `ERROR:` lines of a stored output, shortened.
fn first_errors(ctx: &Ctx, handle: &Handle) -> Vec<String> {
    let Ok(bytes) = ctx.handles.get(handle) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|l| l.contains("ERROR:"))
        .take(3)
        .map(render::snippet)
        .collect()
}

fn busy(job: &JobInfo) -> Error {
    Error::Invalid {
        msg: format!(
            "cannot run bitbake -e now: {} ({}{}) is running, and BitBake allows one client per build directory; wait for it (bitbake op=status job={}) or use static mode (authoritative=false)",
            job.id,
            job.op,
            job.target
                .as_deref()
                .map(|t| format!(" {t}"))
                .unwrap_or_default(),
            job.id
        ),
    }
}

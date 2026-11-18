//! Claude Code hook entry point (T7.1): reads the event JSON from stdin, dispatches on the
//! event and tool name to a [`Handler`], writes the response JSON, exits 0.
//!
//! The wire format is the one in the Claude Code hooks reference, as recorded in
//! `plugin/README.md` (checked against Claude Code 2.1.326):
//!
//! - Input: one JSON object with the common fields (`session_id`, `cwd`,
//!   `hook_event_name`, ...) and, for `PostToolUse`, `tool_name`, `tool_input` and
//!   `tool_response` (the tool's structured output object, not the text the model sees).
//! - Output: nothing (no-op), or one JSON object
//!   `{"hookSpecificOutput": {"hookEventName": "PostToolUse", "additionalContext": "..."}}`.
//!   `additionalContext` is added next to the tool result; the tool output itself is not
//!   changed. A handler may also set `updatedToolOutput` (replaces the output; the value must
//!   match the tool's output shape), which the rules below allow only where it cannot hide
//!   content an Edit may rely on.
//!
//! Rules every handler follows (`docs/milestones/M7-hooks-plugin.md`):
//!
//! - **Fail open.** [`run`] turns any error, panic, unexpected input or budget overrun into
//!   "no output", and the process always exits 0. The reason goes to the debug log
//!   `<cache dir>/hook.log` (see [`LOG_FILE`]), never to stdout.
//! - **Fast.** [`BUDGET`] (150 ms) per call. The index is opened read-only
//!   ([`Index::open_read_only`]); the hook never scans. A file whose bytes on disk differ from
//!   the indexed sha256 is treated as not indexed ([`Env::fresh_file`]): the hook stays
//!   silent about it rather than refresh. The CLI path also arms a watchdog that exits 0
//!   after [`WATCHDOG`] if something blocks.
//! - **Additions only.** Handlers return `additionalContext`. The one exception is the Bash
//!   digest (`bash.rs`), which replaces large bitbake output after storing all of it under a
//!   handle; Bash output is never content an Edit relies on. The edit hook (`edit.rs`) is
//!   the one writer: it reindexes the edited file through its own read-write connection.
//! - **Yocto files inside the workspace only** ([`is_yocto_path`], [`Env::fresh_file`]).
//!
//! - **Switches.** `[hooks]` in `sous-chef.toml` (`grep_read`, `bash`, `edit`, default on)
//!   turns handlers off by [`Handler::switch`]; a switched-off handler is not run, and with
//!   none left the call exits 0 with no output. The grep-through-Bash handler
//!   (`bash_grep.rs`) follows `grep_read`, the BitBake digest `bash`.
//!
//! Adding a handler: implement [`Handler`] in its file and add it to [`HANDLERS`]. Several
//! handlers may take the same tool (`Bash`); they are tried in order.

pub mod bash;
pub mod bash_grep;
pub mod edit;
pub mod grep_read;

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::config::{CliOverrides, Config};
use crate::error::{Error, Result};
use crate::index::query::{self, FileRow};
use crate::index::{self, INDEX_FILE, Index};
use crate::types::FileKind;
use crate::workspace::Workspace;

/// Time budget for one hook call. A call that takes longer produces no output.
pub const BUDGET: Duration = Duration::from_millis(150);

/// Environment variable overriding [`BUDGET`] in milliseconds (tests, slow machines).
pub const BUDGET_ENV: &str = "SOUS_CHEF_HOOK_BUDGET_MS";

/// The CLI path exits 0 with no output if a call is still running after this long.
pub const WATCHDOG: Duration = Duration::from_secs(2);

/// How long a read waits on a lock held by a writer (the MCP server refreshing).
const BUSY: Duration = Duration::from_millis(50);

/// Claude Code caps `additionalContext` at 10,000 characters (longer text is moved to a
/// file). Handlers stay under this.
pub const CONTEXT_CAP: usize = 9_000;

/// The debug log, in the workspace cache directory (`<cache root>/sous-chef/<key>/`), or in
/// `<cache root>/sous-chef/` when no workspace was found.
pub const LOG_FILE: &str = "hook.log";

/// The log is truncated when it grows past this.
const LOG_MAX: u64 = 1 << 20;

/// The `hook_event_name` handled so far.
pub const POST_TOOL_USE: &str = "PostToolUse";

/// How the hook finds the workspace and the index. The CLI fills it from the global flags.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// `--build-dir`, relative to the event's `cwd`.
    pub build_dir: Option<PathBuf>,
    /// `--config`, relative to the event's `cwd`.
    pub config: Option<PathBuf>,
    /// `--layer`, relative to the event's `cwd`.
    pub layers: Vec<PathBuf>,
    /// Cache root in place of `$XDG_CACHE_HOME` / `$HOME/.cache`.
    pub cache_root: Option<PathBuf>,
    /// In place of [`BUDGET`] (and [`BUDGET_ENV`]).
    pub budget: Option<Duration>,
}

impl Options {
    fn budget(&self) -> Duration {
        if let Some(b) = self.budget {
            return b;
        }
        std::env::var(BUDGET_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(BUDGET)
    }

    fn cache_root(&self) -> Result<PathBuf> {
        match &self.cache_root {
            Some(r) => Ok(r.clone()),
            None => index::cache_root(),
        }
    }
}

/// One hook event, as received on stdin.
#[derive(Debug, Clone)]
pub struct Event {
    /// `hook_event_name`, e.g. `PostToolUse`.
    pub hook_event_name: String,
    /// `tool_name` (empty for events without one).
    pub tool_name: String,
    /// `cwd`, when present and absolute.
    pub cwd: Option<PathBuf>,
    /// `tool_input` (`Null` when absent).
    pub tool_input: Value,
    /// `tool_response` (`Null` when absent): the tool's structured output object.
    pub tool_response: Value,
    /// The whole object.
    pub raw: Value,
}

impl Event {
    /// Parses the stdin text. Anything but a JSON object with a string `hook_event_name` is
    /// `Error::Invalid`.
    pub fn parse(text: &str) -> Result<Event> {
        let raw: Value = serde_json::from_str(text).map_err(|e| Error::Invalid {
            msg: format!("hook input is not JSON: {e}"),
        })?;
        let Some(obj) = raw.as_object() else {
            return Err(Error::Invalid {
                msg: "hook input is not a JSON object".into(),
            });
        };
        let Some(name) = obj.get("hook_event_name").and_then(Value::as_str) else {
            return Err(Error::Invalid {
                msg: "hook input has no hook_event_name".into(),
            });
        };
        let tool_name = obj
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let cwd = obj
            .get("cwd")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute());
        Ok(Event {
            hook_event_name: name.to_string(),
            tool_name: tool_name.to_string(),
            cwd,
            tool_input: obj.get("tool_input").cloned().unwrap_or(Value::Null),
            tool_response: obj.get("tool_response").cloned().unwrap_or(Value::Null),
            raw,
        })
    }

    /// A string field of `tool_input`.
    pub fn input_str(&self, key: &str) -> Option<&str> {
        self.tool_input.get(key).and_then(Value::as_str)
    }

    /// A path in the event, made absolute against `cwd` if it is relative (file tools send
    /// absolute paths; Grep's `path` may be relative). `None` if it cannot be made absolute.
    pub fn abs_path(&self, p: &str) -> Option<PathBuf> {
        let p = Path::new(p);
        if p.is_absolute() {
            Some(p.to_path_buf())
        } else {
            self.cwd.as_ref().map(|c| c.join(p))
        }
    }
}

/// What a handler wants added to (or, where allowed, replaced in) the tool result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Response {
    /// `hookSpecificOutput.additionalContext`: text shown to Claude next to the tool result.
    pub additional_context: Option<String>,
    /// `hookSpecificOutput.updatedToolOutput`: replaces the tool output. Must match the
    /// tool's output shape. Only the Bash digest (T7.3) sets it.
    pub updated_tool_output: Option<Value>,
}

impl Response {
    /// A response that only adds `text` as context (trimmed to [`CONTEXT_CAP`]).
    pub fn context(text: impl Into<String>) -> Response {
        Response {
            additional_context: Some(cap_text(text.into(), CONTEXT_CAP)),
            updated_tool_output: None,
        }
    }

    /// The stdout JSON for `event_name`, or `None` when there is nothing to say.
    pub fn to_json(&self, event_name: &str) -> Option<String> {
        let mut specific = Map::new();
        if let Some(c) = self.additional_context.as_ref().filter(|c| !c.is_empty()) {
            specific.insert("additionalContext".into(), Value::String(c.clone()));
        }
        if let Some(o) = &self.updated_tool_output {
            specific.insert("updatedToolOutput".into(), o.clone());
        }
        if specific.is_empty() {
            return None;
        }
        specific.insert(
            "hookEventName".into(),
            Value::String(event_name.to_string()),
        );
        let mut top = Map::new();
        top.insert("hookSpecificOutput".into(), Value::Object(specific));
        serde_json::to_string(&Value::Object(top)).ok()
    }
}

/// Cuts `text` to at most `cap` bytes at a line boundary, with a marker.
fn cap_text(text: String, cap: usize) -> String {
    if text.len() <= cap {
        return text;
    }
    let mut cut = cap.saturating_sub(40);
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = &text[..cut];
    let head = head.rfind('\n').map_or(head, |i| &head[..i]);
    let dropped = text[head.len()..].lines().filter(|l| !l.is_empty()).count();
    format!("{head}\n(+{dropped} more lines)")
}

/// One hook handler. Handlers are stateless; per-call state is in [`Ctx`].
pub trait Handler: Sync {
    /// Short name for the debug log.
    fn name(&self) -> &'static str;
    /// The `[hooks]` key in `sous-chef.toml` that switches this handler
    /// (`config::HooksConfig::enabled`); by default its [`Handler::name`].
    fn switch(&self) -> &'static str {
        self.name()
    }
    /// The `hook_event_name` values handled.
    fn events(&self) -> &'static [&'static str];
    /// The `tool_name` values handled (exact match).
    fn tools(&self) -> &'static [&'static str];
    /// Handles one event. `Ok(None)` changes nothing. Errors are logged and become no-ops.
    fn handle(&self, cx: &mut Ctx<'_>, ev: &Event) -> Result<Option<Response>>;
}

/// Every handler, in dispatch order. Each switched-on handler whose event and tool match
/// is tried in turn until one returns a response or an error; the rest are not run. Two
/// handle `Bash`: the BitBake digest (switch `bash`) and the grep annotation (switch
/// `grep_read`); they act on disjoint commands.
pub static HANDLERS: &[&dyn Handler] = &[
    &grep_read::GrepRead,
    &bash::BashDigest,
    &bash_grep::BashGrep,
    &edit::EditFollowUp,
];

/// The first handler for an event and tool name, if any.
pub fn handler_for(event: &str, tool: &str) -> Option<&'static dyn Handler> {
    handlers_for(event, tool).next()
}

/// Every handler for an event and tool name, in dispatch order.
pub fn handlers_for<'a>(
    event: &'a str,
    tool: &'a str,
) -> impl Iterator<Item = &'static dyn Handler> + 'a {
    HANDLERS
        .iter()
        .copied()
        .filter(move |h| h.events().contains(&event) && h.tools().contains(&tool))
}

/// `.bb`, `.bbappend`, `.inc`, `.bbclass` or `.conf`.
pub fn is_yocto_path(p: &Path) -> bool {
    FileKind::from_path(p).is_some()
}

/// The workspace and its index, opened for one call.
pub struct Env<'c> {
    pub ws: &'c Workspace,
    pub index: &'c Index,
    fresh: &'c mut HashMap<PathBuf, Option<FileRow>>,
    deadline: Instant,
    budget: Duration,
}

impl Env<'_> {
    /// The time left in the call's budget.
    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// `Err` once the call's budget is spent (the same clock as [`Ctx::check_budget`]).
    pub fn check_budget(&self) -> Result<()> {
        if Instant::now() > self.deadline {
            Err(Error::Invalid {
                msg: format!("over the {} ms budget", self.budget.as_millis()),
            })
        } else {
            Ok(())
        }
    }

    /// The index row for `abs` if it is a Yocto file inside the workspace, indexed, and
    /// unchanged since it was indexed (sha256 of the bytes on disk equals the row's). Any
    /// other case is `None`: the hook says nothing about files it cannot vouch for.
    /// Memoised per call.
    pub fn fresh_file(&mut self, abs: &Path) -> Result<Option<FileRow>> {
        if let Some(r) = self.fresh.get(abs) {
            return Ok(r.clone());
        }
        let row = self.lookup(abs)?;
        self.fresh.insert(abs.to_path_buf(), row.clone());
        Ok(row)
    }

    fn lookup(&self, abs: &Path) -> Result<Option<FileRow>> {
        if !is_yocto_path(abs) {
            return Ok(None);
        }
        let Some(row) = query::file_by_abs(self.index, self.ws, abs)? else {
            return Ok(None);
        };
        let Ok(real) = self.ws.confine(abs) else {
            return Ok(None);
        };
        let Ok(bytes) = std::fs::read(&real) else {
            return Ok(None);
        };
        let sha = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        Ok((sha == row.sha256).then_some(row))
    }
}

/// Per-call state: options, the clock, the lazily opened workspace and index, the log.
pub struct Ctx<'o> {
    opts: &'o Options,
    cwd: Option<PathBuf>,
    start: Instant,
    budget: Duration,
    ws: Option<Workspace>,
    cfg: Option<Config>,
    index: Option<Index>,
    env_error: Option<String>,
    fresh: HashMap<PathBuf, Option<FileRow>>,
    log: Vec<String>,
}

impl<'o> Ctx<'o> {
    /// A context for an event whose working directory is `cwd`.
    pub fn new(opts: &'o Options, cwd: Option<PathBuf>) -> Ctx<'o> {
        Ctx {
            opts,
            cwd,
            start: Instant::now(),
            budget: opts.budget(),
            ws: None,
            cfg: None,
            index: None,
            env_error: None,
            fresh: HashMap::new(),
            log: Vec::new(),
        }
    }

    /// Time since the call started.
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// True once the budget is spent. Handlers check it between files.
    pub fn over_budget(&self) -> bool {
        self.elapsed() > self.budget
    }

    /// `Err` once the budget is spent.
    pub fn check_budget(&self) -> Result<()> {
        if self.over_budget() {
            Err(Error::Invalid {
                msg: format!("over the {} ms budget", self.budget.as_millis()),
            })
        } else {
            Ok(())
        }
    }

    /// Adds a line to the debug log.
    pub fn note(&mut self, msg: impl Into<String>) {
        self.log.push(msg.into());
    }

    /// The workspace (discovered from the event's `cwd` and the options) and the existing
    /// index, opened read-only. Never scans: a missing or outdated index is an error. The
    /// first failure is remembered for the rest of the call.
    pub fn env(&mut self) -> Result<Env<'_>> {
        if let Some(e) = &self.env_error {
            return Err(Error::Invalid { msg: e.clone() });
        }
        if self.index.is_none()
            && let Err(e) = self.open()
        {
            self.env_error = Some(e.to_string());
            return Err(e);
        }
        match (&self.ws, &self.index) {
            (Some(ws), Some(index)) => Ok(Env {
                ws,
                index,
                fresh: &mut self.fresh,
                deadline: self.start + self.budget,
                budget: self.budget,
            }),
            _ => Err(Error::Invalid {
                msg: "workspace not open".into(),
            }),
        }
    }

    /// The configuration the workspace was discovered with (`sous-chef.toml` and the hook's
    /// flags), once [`Ctx::env`] or [`Ctx::load_config`] has loaded it.
    pub fn config(&self) -> Option<&Config> {
        self.cfg.as_ref()
    }

    /// Loads the configuration (`sous-chef.toml` from the event's `cwd`, and the hook's
    /// flags) once; no workspace discovery.
    pub fn load_config(&mut self) -> Result<&Config> {
        if self.cfg.is_none() {
            let Some(cwd) = self.cwd.clone() else {
                return Err(Error::Invalid {
                    msg: "event has no absolute cwd".into(),
                });
            };
            let cfg = Config::load(&CliOverrides {
                build_dir: self.opts.build_dir.clone(),
                config: self.opts.config.clone(),
                layers: self.opts.layers.clone(),
                work_dir: cwd,
            })?;
            self.cfg = Some(cfg);
        }
        self.cfg.as_ref().ok_or_else(|| Error::Invalid {
            msg: "no configuration".into(),
        })
    }

    /// Discovers the workspace (kept even when the index is missing, so the log lands in
    /// the workspace cache directory), then opens the index.
    fn open(&mut self) -> Result<()> {
        if self.ws.is_none() {
            let cfg = self.load_config()?.clone();
            self.ws = Some(Workspace::discover(&cfg)?);
        }
        let Some(ws) = &self.ws else {
            return Err(Error::Invalid {
                msg: "workspace not open".into(),
            });
        };
        let path = index::cache_dir_in(&self.opts.cache_root()?, ws).join(INDEX_FILE);
        if !path.is_file() {
            return Err(Error::NotFound {
                what: format!("index {} (run `sous-chef index` once)", path.display()),
            });
        }
        self.index = Some(Index::open_read_only(&path, BUSY)?);
        Ok(())
    }

    /// Where the debug log goes.
    fn log_dir(&self) -> Option<PathBuf> {
        let root = self.opts.cache_root().ok()?;
        Some(match &self.ws {
            Some(ws) => index::cache_dir_in(&root, ws),
            None => root.join("sous-chef"),
        })
    }
}

/// Handles one hook event (the JSON text from stdin) with default [`Options`] and returns
/// the response JSON to print (`None` for no output). Errors are already turned into `None`
/// by [`run`]; this keeps the M0 signature and never returns `Err`.
pub fn handle(event_json: &str) -> Result<Option<String>> {
    Ok(run(event_json, &Options::default()))
}

/// The fail-open wrapper: parses, dispatches, enforces the budget, logs, and returns the
/// stdout text. Errors, panics, unknown events and tools, and overruns all give `None`.
pub fn run(event_json: &str, opts: &Options) -> Option<String> {
    let ev = match Event::parse(event_json) {
        Ok(ev) => ev,
        Err(e) => {
            let cx = Ctx::new(opts, None);
            write_log(&cx, "-", "-", &format!("skipped: {e}"));
            return None;
        }
    };
    let handlers: Vec<&dyn Handler> = handlers_for(&ev.hook_event_name, &ev.tool_name).collect();
    if handlers.is_empty() {
        // The common case for a broad matcher: say nothing, log nothing.
        return None;
    }
    let mut cx = Ctx::new(opts, ev.cwd.clone());
    let mut out = None;
    let mut ran: Option<(&dyn Handler, String)> = None;
    let mut off: Vec<&str> = Vec::new();
    for handler in handlers {
        // `[hooks]` switches. A configuration that cannot be loaded switches nothing off;
        // the handler then fails open on it if it needs the workspace.
        let on = cx
            .load_config()
            .map(|c| c.hooks.enabled(handler.switch()))
            .unwrap_or(true);
        if !on {
            if !off.contains(&handler.switch()) {
                off.push(handler.switch());
            }
            continue;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handler.handle(&mut cx, &ev)
        }));
        let (o, outcome, done) = match result {
            Ok(Ok(Some(_))) if cx.over_budget() => (
                None,
                format!("dropped: over budget ({} ms)", cx.elapsed().as_millis()),
                true,
            ),
            Ok(Ok(Some(resp))) => match resp.to_json(&ev.hook_event_name) {
                Some(json) => (Some(json), "context added".to_string(), true),
                None => (None, "nothing to add".to_string(), false),
            },
            Ok(Ok(None)) => (None, "nothing to add".to_string(), false),
            Ok(Err(e)) => (None, format!("failed open: {e}"), true),
            Err(_) => (None, "failed open: handler panicked".to_string(), true),
        };
        out = o;
        ran = Some((handler, outcome));
        if done {
            break;
        }
    }
    let mut line = match &ran {
        Some((h, outcome)) => format!("{} {} ms: {outcome}", h.name(), cx.elapsed().as_millis()),
        None => format!("- {} ms: nothing run", cx.elapsed().as_millis()),
    };
    if !off.is_empty() {
        line.push_str(&format!("; switched off by [hooks] {}", off.join(", ")));
    }
    for n in std::mem::take(&mut cx.log) {
        line.push_str("; ");
        line.push_str(&n);
    }
    write_log(&cx, &ev.hook_event_name, &ev.tool_name, &line);
    out
}

/// Appends one line to the debug log. Failures are ignored: the log is best effort.
fn write_log(cx: &Ctx<'_>, event: &str, tool: &str, msg: &str) {
    let Some(dir) = cx.log_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(LOG_FILE);
    let truncate = std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX);
    let mut opts = OpenOptions::new();
    opts.create(true);
    if truncate {
        opts.write(true).truncate(true);
    } else {
        opts.append(true);
    }
    let Ok(mut f) = opts.open(&path) else { return };
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let msg = msg.replace('\n', " ");
    let _ = writeln!(f, "{secs} {event} {tool} {msg}");
}

/// Set once stdout is being written, so the watchdog does not cut a response in half.
static WRITING: AtomicBool = AtomicBool::new(false);

/// For the `sous-chef hook` command (`main.rs`): starts a thread that exits the process with
/// status 0 and no output if the call is still running after [`WATCHDOG`], unless
/// [`begin_output`] was called first.
pub fn arm_watchdog() {
    std::thread::spawn(|| {
        std::thread::sleep(WATCHDOG);
        if !WRITING.load(Ordering::SeqCst) {
            std::process::exit(0);
        }
    });
}

/// Call before writing the response, so the watchdog does not cut it in half.
pub fn begin_output() {
    WRITING.store(true, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_non_objects() {
        assert!(Event::parse("").is_err());
        assert!(Event::parse("[1]").is_err());
        assert!(Event::parse("{\"tool_name\": \"Read\"}").is_err());
        let ev = Event::parse("{\"hook_event_name\": \"PostToolUse\", \"cwd\": \"rel\"}").unwrap();
        assert_eq!(ev.tool_name, "");
        assert_eq!(ev.cwd, None);
    }

    #[test]
    fn response_json_shape() {
        assert_eq!(Response::default().to_json(POST_TOOL_USE), None);
        let j = Response::context("hi").to_json(POST_TOOL_USE).unwrap();
        let v: Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        assert_eq!(v["hookSpecificOutput"]["additionalContext"], "hi");
        assert_eq!(v.as_object().unwrap().len(), 1);
    }

    #[test]
    fn cap_keeps_whole_lines() {
        let text: String = (0..1000).map(|i| format!("line {i}\n")).collect();
        let c = cap_text(text, 200);
        assert!(c.len() <= 200);
        assert!(c.ends_with("more lines)"));
        assert!(c.lines().rev().nth(1).unwrap().starts_with("line "));
    }

    #[test]
    fn dispatch_table() {
        assert_eq!(
            handler_for(POST_TOOL_USE, "Grep").map(|h| h.name()),
            Some("grep_read")
        );
        assert_eq!(
            handler_for(POST_TOOL_USE, "Read").map(|h| h.name()),
            Some("grep_read")
        );
        assert_eq!(
            handler_for(POST_TOOL_USE, "Bash").map(|h| h.name()),
            Some("bash")
        );
        assert_eq!(
            handler_for(POST_TOOL_USE, "Edit").map(|h| h.name()),
            Some("edit")
        );
        let bash: Vec<(&str, &str)> = handlers_for(POST_TOOL_USE, "Bash")
            .map(|h| (h.name(), h.switch()))
            .collect();
        assert_eq!(bash, [("bash", "bash"), ("bash_grep", "grep_read")]);
        for h in HANDLERS {
            assert!(
                ["grep_read", "bash", "edit"].contains(&h.switch()),
                "{} has no [hooks] key",
                h.name()
            );
        }
        assert!(handler_for(POST_TOOL_USE, "WebFetch").is_none());
        assert!(handler_for("PreToolUse", "Read").is_none());
    }
}

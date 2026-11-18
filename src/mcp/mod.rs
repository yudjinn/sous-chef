//! MCP server and tool registry.
//!
//! Each tool lives in `tools/<name>.rs` with `pub struct Args` (Deserialize + JsonSchema)
//! and `pub async fn run(ctx: &Ctx, args: Args) -> Result<String>`. [`TOOLS`] is the single
//! table of name, description, loading class, schema and dispatch. [`Server`] serves that
//! table over stdio with `rmcp` by implementing `ServerHandler::list_tools` / `call_tool`
//! directly (no `#[tool]` macros), so tool files never depend on `rmcp`.
//!
//! Deferred tools: MCP itself has no notion of deferred loading; every tool is listed in
//! `tools/list` and every client can call it. Claude Code's tool search (on by default)
//! defers every MCP tool, keeping only its name and the server instructions in context
//! until the agent searches for it, unless the tool's `_meta` has
//! `"anthropic/alwaysLoad": true`: the six always-loaded tools carry it. A client without
//! tool search (or a session whose `--tools` leaves `ToolSearch` out) loads every
//! definition upfront, so all of them are kept short: a deferred tool has a one-line
//! description, and every schema is compacted ([`compact_schema`]). The server
//! instructions name the deferred tools and point at `sous-chef://help` for the long form.
//!
//! Startup never kills the transport: if the configuration or workspace cannot be loaded,
//! the server still serves (degraded). Every tool then returns the startup error as a tool
//! error, and `status` reports it, so the client sees why instead of a dead server.
//!
//! Logging goes to stderr only; stdout belongs to the MCP transport.

pub mod tools;

/// Progress of the tool call being served, as MCP `notifications/progress`, without any
/// `rmcp` type in the tool files.
///
/// **What MCP says** (specification 2025-06-18, "Progress"): a request asks for progress by
/// carrying `_meta.progressToken` (a string or an integer); the receiver MAY send
/// `notifications/progress` with that token, `progress` (which MUST increase with each
/// notification), and optional `total` and `message`; notifications MUST only reference
/// the token of an active request and MUST stop after completion; both sides SHOULD rate
/// limit.
///
/// **How rmcp 3.10.0 gives it to a server**: `ServerHandler::call_tool(request, context)`
/// receives a `RequestContext<RoleServer>` whose `meta` (`RequestMetaObject`, the request's
/// `params._meta`, moved there by the service loop in `service.rs`) has
/// `get_progress_token() -> Option<ProgressToken>`, and whose `peer: Peer<RoleServer>` has
/// `notify_progress(ProgressNotificationParam)` (`service/server.rs`, a `peer_not` method:
/// it returns once the transport has written the notification).
///
/// **Here**: [`Server`]'s `call_tool` makes a [`Sink`] only when the request carries a
/// token, and runs the tool inside [`scope`], a tokio task-local, so any code running in
/// that tool call (also synchronous code, such as `audit`'s `fn(&str)` progress hook) can
/// call [`step`] / [`of`] / [`at`]; without a sink (no token, or outside a tool call: the
/// CLI, tests) they do nothing. Values are made strictly increasing here; a value that is
/// not above the last one is dropped. A forwarder task ([`forward`]) sends at most one
/// notification per [`MIN_INTERVAL`], always the newest (intermediate ones are dropped),
/// and the newest pending one when the tool ends; `call_tool` waits for it (at most
/// [`FLUSH_WAIT`], then aborts it) before it returns the result, so no notification follows
/// the response.
pub mod progress {
    use std::future::Future;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tokio::sync::mpsc;
    use tokio::time::Instant;

    /// Least time between two notifications of one call: at most four a second.
    pub const MIN_INTERVAL: Duration = Duration::from_millis(250);

    /// How long a finished call waits for its last notifications to be written.
    pub const FLUSH_WAIT: Duration = Duration::from_secs(2);

    /// One progress update: `progress` increases from update to update.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Update {
        pub progress: f64,
        pub total: Option<f64>,
        pub message: String,
    }

    /// Where the updates of one tool call go.
    #[derive(Debug)]
    pub struct Sink {
        tx: mpsc::UnboundedSender<Update>,
        last: Mutex<f64>,
        /// When the call began ([`elapsed`]).
        start: std::time::Instant,
    }

    tokio::task_local! {
        static SINK: Arc<Sink>;
    }

    /// A sink and the receiving end the forwarder reads.
    pub fn channel() -> (Arc<Sink>, mpsc::UnboundedReceiver<Update>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Arc::new(Sink {
                tx,
                last: Mutex::new(0.0),
                start: std::time::Instant::now(),
            }),
            rx,
        )
    }

    /// Runs `f` with `sink` as the progress sink of the current tool call (none: progress
    /// calls inside `f` do nothing). The sink is dropped when `f` ends, which closes the
    /// channel.
    pub async fn scope<F: Future>(sink: Option<Arc<Sink>>, f: F) -> F::Output {
        match sink {
            Some(s) => SINK.scope(s, f).await,
            None => f.await,
        }
    }

    /// Whether the current tool call reports progress (the client sent a token).
    pub fn active() -> bool {
        SINK.try_with(|_| ()).is_ok()
    }

    /// Reports `progress` (and `total`, if known) with `message`; dropped unless `progress`
    /// is above the last value reported in this call.
    pub fn at(progress: f64, total: Option<f64>, message: &str) {
        let _ = SINK.try_with(|s| {
            let mut last = s.last.lock().unwrap_or_else(|e| e.into_inner());
            if progress.is_finite() && progress > *last {
                *last = progress;
                let _ = s.tx.send(Update {
                    progress,
                    total,
                    message: message.to_string(),
                });
            }
        });
    }

    /// Reports one more step (the last value plus one), with no total.
    pub fn step(message: &str) {
        let _ = SINK.try_with(|s| {
            let mut last = s.last.lock().unwrap_or_else(|e| e.into_inner());
            *last += 1.0;
            let _ = s.tx.send(Update {
                progress: *last,
                total: None,
                message: message.to_string(),
            });
        });
    }

    /// Reports the seconds since the tool call began as the progress value, with no total:
    /// for waits whose length is not known (a job, the lock).
    pub fn elapsed(message: &str) {
        let Ok(secs) = SINK.try_with(|s| s.start.elapsed().as_secs_f64()) else {
            return;
        };
        at(secs, None, message);
    }

    /// Reports `done` of `total` items (files, say).
    pub fn of(done: u64, total: u64, message: &str) {
        at(done as f64, Some(total as f64), message);
    }

    /// Runs the synchronous, long `f` (a lint run) from a tool call. On a multi-threaded
    /// runtime (the server's) it runs under `block_in_place`, which hands this worker's
    /// queue to another thread meanwhile, so the forwarder and rmcp's service loop keep
    /// running and notifications go out while `f` runs, not after it; the progress sink of
    /// the call stays in scope. Elsewhere (a current-thread runtime, no runtime) `f` just
    /// runs.
    pub fn blocking<T>(f: impl FnOnce() -> T) -> T {
        match tokio::runtime::Handle::try_current() {
            Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(f)
            }
            _ => f(),
        }
    }

    /// Reads updates from `rx` until it closes and hands them to `send`: the first at once,
    /// then at most one per `interval`, always the newest received (older ones waiting are
    /// dropped), and the newest pending one as soon as `rx` closes. Stops early when `send`
    /// returns false (the transport is gone).
    pub async fn forward<S, Fut>(
        mut rx: mpsc::UnboundedReceiver<Update>,
        interval: Duration,
        mut send: S,
    ) where
        S: FnMut(Update) -> Fut,
        Fut: Future<Output = bool>,
    {
        let mut last_sent: Option<Instant> = None;
        while let Some(mut update) = rx.recv().await {
            if let Some(t) = last_sent {
                let due = t + interval;
                loop {
                    match tokio::time::timeout_at(due, rx.recv()).await {
                        Ok(Some(newer)) => update = newer,
                        Ok(None) => {
                            send(update).await;
                            return;
                        }
                        Err(_) => break,
                    }
                }
            }
            if !send(update).await {
                return;
            }
            last_sent = Some(Instant::now());
        }
    }
}

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListResourcesResult, ListToolsResult, MetaObject, PaginatedRequestParams,
    ProgressNotificationParam, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    Resource, ResourceContents, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde::de::DeserializeOwned;

use crate::bitbake::JobTable;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::handles::HandleStore;
use crate::index::Index;
use crate::workspace::Workspace;

/// Writes one log line to stderr. Never stdout: that is the MCP transport.
pub fn log(msg: &str) {
    eprintln!("sous-chef: {msg}");
}

/// Shared state for every tool call. Built once at startup.
#[derive(Debug)]
pub struct Ctx {
    pub config: Config,
    pub workspace: Workspace,
    /// `rusqlite::Connection` is `Send` but not `Sync`, hence the mutex. A tokio mutex, so a
    /// tool may hold the guard across `.await`: `let mut index = ctx.index.lock().await;`.
    pub index: tokio::sync::Mutex<Index>,
    pub handles: HandleStore,
    pub jobs: JobTable,
}

impl Ctx {
    /// Discovers the workspace, opens the index and the handle store.
    ///
    /// A configuration that fails `Config::validate` is logged, not fatal: `status` reports
    /// it on every call.
    pub fn open(config: Config) -> Result<Ctx> {
        Ctx::open_labelled(config).map_err(|(_, e)| e)
    }

    /// As [`Ctx::open`], naming the step that failed.
    fn open_labelled(config: Config) -> std::result::Result<Ctx, (&'static str, Error)> {
        let workspace =
            Workspace::discover(&config).map_err(|e| ("discovering the workspace", e))?;
        if let Err(e) = config.validate(&workspace) {
            log(&format!("configuration problem: {e}"));
        }
        let index = Index::open(&workspace).map_err(|e| ("opening the index", e))?;
        // After `Index::open`, and in the index's own directory: the store keeps its rows in
        // the index database, which a schema rebuild replaces. The retention limits are
        // `[handles]` of sous-chef.toml (`Config::from_toml` has checked their range).
        let handles = HandleStore::open_with_retention(
            &index.cache_dir().join("handles"),
            config.handles.retention(),
        )
        .map_err(|e| ("opening the handle store", e))?;
        let ctx = Ctx {
            config,
            workspace,
            index: tokio::sync::Mutex::new(index),
            handles,
            jobs: JobTable::new(),
        };
        // Output files a crashed server left behind (`bitbake::jobs`, module docs).
        ctx.jobs.sweep_in_background(&ctx.jobs_dir());
        Ok(ctx)
    }

    /// Where job output is written while a job runs: `<cache dir>/jobs`, next to `handles/`.
    pub fn jobs_dir(&self) -> PathBuf {
        let h = self.handles.dir();
        h.parent().unwrap_or(h).join("jobs")
    }
}

/// The per-workspace cache directory. The single definition is [`crate::index::cache_dir`]
/// (`$XDG_CACHE_HOME/sous-chef/<workspace key>/`, `$HOME/.cache` as the fallback root), so
/// this always names the directory `Index::open` uses. Only when neither variable gives a
/// usable root (where `Index::open` itself fails) does it fall back to the temp directory.
pub fn cache_dir(ws: &Workspace) -> PathBuf {
    crate::index::cache_dir(ws)
        .unwrap_or_else(|_| crate::index::cache_dir_in(&std::env::temp_dir(), ws))
}

/// Path of the index database for a workspace (see [`cache_dir`]).
pub fn index_path(ws: &Workspace) -> PathBuf {
    cache_dir(ws).join("index.sqlite")
}

/// Whether the client should load a tool's full definition up front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loading {
    /// Always loaded: full description.
    Always,
    /// Deferred: short description, loaded on demand by the client.
    Deferred,
}

impl Loading {
    /// How `sous-chef://help` names the class.
    pub fn as_str(self) -> &'static str {
        match self {
            Loading::Always => "always",
            Loading::Deferred => "deferred",
        }
    }
}

/// The future a tool call returns. `Send`, because rmcp runs handlers on a multi-threaded
/// runtime.
pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;

/// Type-erased tool entry point: deserialises the arguments and calls `tools::<name>::run`.
pub type ToolFn = for<'a> fn(&'a Ctx, JsonObject) -> ToolFuture<'a>;

/// One row of the tool registry.
#[derive(Debug, Clone, Copy)]
pub struct ToolEntry {
    pub name: &'static str,
    pub description: &'static str,
    pub loading: Loading,
    /// JSON Schema (draft 2020-12, as rmcp generates it) of `tools::<name>::Args`.
    pub schema: fn() -> Arc<JsonObject>,
    pub call: ToolFn,
}

impl ToolEntry {
    /// The rmcp tool definition advertised in `tools/list`: the description and the
    /// [`compact_schema`] of the arguments. An always-loaded tool carries
    /// `_meta["anthropic/alwaysLoad"] = true`, the key Claude Code reads to load a tool
    /// upfront while tool search defers the others; a deferred tool has no `_meta`.
    pub fn to_rmcp(&self) -> Tool {
        let tool = Tool::new(
            self.name,
            self.description,
            Arc::new(compact_schema(&(self.schema)())),
        );
        if self.loading == Loading::Always {
            let mut meta = MetaObject::new();
            meta.0.insert(ALWAYS_LOAD_META_KEY.into(), true.into());
            tool.with_meta(meta)
        } else {
            tool
        }
    }
}

/// `_meta` key with which Claude Code loads a tool upfront when tool search is on
/// (code.claude.com/docs/en/mcp, "Mark a tool to load upfront or stay deferred").
pub const ALWAYS_LOAD_META_KEY: &str = "anthropic/alwaysLoad";

/// Descriptions of the two arguments every tool takes; the server instructions explain
/// them once, so `tools/list` leaves them out. A tool whose own text differs (another
/// default) keeps it.
const COMMON_ARG_DOCS: &[&str] = &[
    "Output budget in tokens (default 600).",
    "`text` (default) or `json`.",
];

/// The argument schema as `tools/list` sends it: the schemars output without what costs
/// context and tells a client nothing it needs to call the tool. `$schema` and `title`
/// go; an enum (a `$defs` entry of `const` strings, or a plain `enum`) is written in
/// place as `{"type": "string", "enum": [...]}` without the per-value descriptions (the
/// property's own description names the values; the long form is `sous-chef://help`);
/// `Option<T>` is written as `T` (an argument left out is the same as null);
/// integer `format` / `minimum` annotations go (serde rejects a bad value with a clear
/// error); newlines in descriptions become spaces. Property names, `required`,
/// defaults and the accepted values do not change. `sous-chef://help` is generated from
/// the full schema.
pub fn compact_schema(schema: &JsonObject) -> JsonObject {
    let defs = schema
        .get("$defs")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let mut out = JsonObject::new();
    let mut kept_defs = JsonObject::new();
    for (k, v) in schema {
        match k.as_str() {
            "$schema" | "title" | "$defs" => {}
            "properties" => {
                let mut props = JsonObject::new();
                if let Some(obj) = v.as_object() {
                    for (name, p) in obj {
                        let mut p = compact_node(p, &defs, &mut kept_defs);
                        if let Some(o) = p.as_object_mut() {
                            let common = o
                                .get("description")
                                .and_then(|d| d.as_str())
                                .is_some_and(|d| COMMON_ARG_DOCS.contains(&d));
                            if common {
                                o.remove("description");
                            }
                        }
                        props.insert(name.clone(), p);
                    }
                }
                out.insert(k.clone(), serde_json::Value::Object(props));
            }
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    if !kept_defs.is_empty() {
        out.insert("$defs".into(), serde_json::Value::Object(kept_defs));
    }
    out
}

/// The string values of an enum definition, if it is one (`oneOf` of `const` strings, or
/// `enum` of strings).
fn enum_values(def: &serde_json::Value) -> Option<Vec<serde_json::Value>> {
    let o = def.as_object()?;
    if let Some(list) = o.get("enum").and_then(|v| v.as_array()) {
        return list.iter().all(|v| v.is_string()).then(|| list.clone());
    }
    let one_of = o.get("oneOf")?.as_array()?;
    one_of
        .iter()
        .map(|v| v.get("const").filter(|c| c.is_string()).cloned())
        .collect()
}

/// One property schema compacted ([`compact_schema`]).
fn compact_node(
    node: &serde_json::Value,
    defs: &JsonObject,
    kept_defs: &mut JsonObject,
) -> serde_json::Value {
    use serde_json::Value;
    let Some(o) = node.as_object() else {
        return node.clone();
    };
    // `anyOf: [X, {type: null}]` (an `Option` of a referenced type) is X.
    if let Some(any) = o.get("anyOf").and_then(|v| v.as_array()) {
        let non_null: Vec<&Value> = any
            .iter()
            .filter(|v| v.get("type").and_then(|t| t.as_str()) != Some("null"))
            .collect();
        if non_null.len() == 1 && any.len() == 2 {
            let mut inner = compact_node(non_null[0], defs, kept_defs);
            if let (Some(io), Some(d)) = (inner.as_object_mut(), o.get("description")) {
                io.insert("description".into(), d.clone());
            }
            return inner;
        }
    }
    let mut out = serde_json::Map::new();
    if let Some(r) = o.get("$ref").and_then(|v| v.as_str()) {
        let name = r.strip_prefix("#/$defs/").unwrap_or(r);
        match defs.get(name) {
            Some(def) if enum_values(def).is_some() => {
                out.insert("type".into(), "string".into());
                out.insert(
                    "enum".into(),
                    Value::Array(enum_values(def).unwrap_or_default()),
                );
            }
            Some(def) => {
                kept_defs.insert(name.to_string(), def.clone());
                out.insert("$ref".into(), r.into());
            }
            None => {
                out.insert("$ref".into(), r.into());
            }
        }
    }
    for (k, v) in o {
        match k.as_str() {
            "$ref" | "format" | "minimum" => {}
            "type" => {
                let t = match v.as_array() {
                    Some(list) => {
                        let non_null: Vec<&Value> =
                            list.iter().filter(|t| t.as_str() != Some("null")).collect();
                        if non_null.len() == 1 {
                            non_null[0].clone()
                        } else {
                            v.clone()
                        }
                    }
                    None => v.clone(),
                };
                out.insert(k.clone(), t);
            }
            "description" => {
                let d = v.as_str().map(|s| s.replace('\n', " ")).unwrap_or_default();
                out.insert(k.clone(), d.into());
            }
            "items" => {
                out.insert(k.clone(), compact_node(v, defs, kept_defs));
            }
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(out)
}

/// Deserialises tool arguments; a mismatch is an `Invalid` error naming the problem.
pub fn parse_args<A: DeserializeOwned>(args: JsonObject) -> Result<A> {
    serde_json::from_value(serde_json::Value::Object(args)).map_err(|e| Error::Invalid {
        msg: format!("invalid arguments: {e}"),
    })
}

macro_rules! tool {
    ($name:ident, $loading:ident, $description:expr) => {
        ToolEntry {
            name: stringify!($name),
            description: $description,
            loading: Loading::$loading,
            schema: || rmcp::handler::server::common::schema_for_type::<tools::$name::Args>(),
            call: |ctx, args| {
                Box::pin(async move {
                    let args: tools::$name::Args = parse_args(args)?;
                    tools::$name::run(ctx, args).await
                })
            },
        }
    };
}

/// Every tool, in the order of docs/mcp-tools.md. The first six are always loaded.
pub static TOOLS: &[ToolEntry] = &[
    tool!(
        recipe,
        Always,
        "A Yocto recipe's shape; use instead of reading .bb/.bbappend/.inc files. show: \
         summary (file, versions, bbappends in order, classes, key values), tree \
         (include/inherit tree), outline (tasks, functions), sources (SRC_URI entries), \
         tasks (task graph), packages (PACKAGES, FILES, RDEPENDS), cve (CVE check inputs)."
    ),
    tool!(
        var,
        Always,
        "Final value and history of a BitBake variable for a recipe (or the global \
         configuration): every operation in parse order with its status (applied, inactive, \
         shadowed, removed), override and pointer, through includes, inherits, bbappends and \
         overrides. Use instead of grepping for assignments."
    ),
    tool!(
        overrides,
        Always,
        "OVERRIDES for a recipe/machine/distro. show: chain (entries by priority and where \
         each comes from), conditional (override-conditional statements, active or not), \
         dead (overrides matching no machine, distro, class or package: likely typos)."
    ),
    tool!(
        item,
        Always,
        "Exact source lines of a Yocto file, instead of Read: give loc (layer:path:A-B), or \
         recipe + name (e.g. do_install:append), or file + name. Lines are byte-exact, usable \
         as Edit old_string; capped at 8 KB."
    ),
    tool!(
        search,
        Always,
        "Search the Yocto layers. mode=name (default): statement names by substring; \
         mode=regex: file content line by line; mode=fts: full text of names and snippets. \
         Filter by kind (assign, function, task, inherit, include, ...), layer, file_kind or \
         recipe (that recipe's tree). Each hit: pointer, kind, enclosing item with its range, \
         snippet; limit defaults to 30. Prefer this over Grep for Yocto metadata."
    ),
    tool!(
        class,
        Always,
        "A bbclass. show: summary (which file wins, classes inherited, tasks, \
         EXPORT_FUNCTIONS, variables set with ?=), users (who inherits it), outline \
         (functions and tasks)."
    ),
    tool!(
        layers,
        Deferred,
        "List layers, shadowed recipes/classes, or bbappends and what they match."
    ),
    tool!(
        deps,
        Deferred,
        "Build, runtime, PACKAGECONFIG and reverse dependencies of a recipe, or providers of a name."
    ),
    tool!(
        lint,
        Deferred,
        "Lint a recipe, file or layer against Yocto conventions."
    ),
    tool!(
        scaffold,
        Deferred,
        "Create a recipe, bbappend, layer, class or .inc split (dry run by default), or advise where a change belongs."
    ),
    tool!(
        bitbake,
        Deferred,
        "Run bitbake, devtool or bitbake-layers as a background job; poll with op=status."
    ),
    tool!(
        log,
        Deferred,
        "Digest of a build failure log with the first error and a handle to the full log."
    ),
    tool!(
        slice,
        Deferred,
        "Read part of a stored output handle by lines, grep, variable, failures or page."
    ),
    tool!(
        status,
        Deferred,
        "Workspace, index and bitbake environment status."
    ),
    tool!(
        audit,
        Deferred,
        "Findings report over every layer of the repository, stored under a handle; returns a digest."
    ),
    tool!(
        diff,
        Deferred,
        "Statement-level diff of a layer's recipes between two git revisions, and its effect on your bbappends."
    ),
    tool!(
        sigs,
        Deferred,
        "Why a task re-ran: digest of bitbake-diffsigs (changed variables with pointers, the dependency path); builds nothing."
    ),
    tool!(
        pkgdata,
        Deferred,
        "Which package and recipe ship a file; a package's files, recipe and runtime deps, from a build's package data."
    ),
];

/// Looks up a registry entry by tool name.
pub fn find_tool(name: &str) -> Option<&'static ToolEntry> {
    TOOLS.iter().find(|t| t.name == name)
}

/// The server instructions sent in the `initialize` result.
pub fn instructions() -> String {
    let deferred: Vec<&str> = TOOLS
        .iter()
        .filter(|t| t.loading == Loading::Deferred)
        .map(|t| t.name)
        .collect();
    format!(
        "sous-chef indexes the Yocto / OpenEmbedded layers of this workspace and models \
         includes, inherits, bbappends and the override chain the way BitBake does. For \
         .bb, .bbappend, .inc, .bbclass and .conf files, prefer `recipe`, `var`, `item` and \
         `search` over Grep and Read: answers are pointers (layer:path:A-B) with one-line \
         snippets, and `item` returns exact lines usable as Edit text. Every tool takes \
         budget_tokens (default 600) and format (text or json). Deferred tools with \
         one-line descriptions: {}; their arguments are in the resource {HELP_URI}, and the \
         lint rule catalogue is in {RULES_URI}. Call `status` if something looks wrong.",
        deferred.join(", ")
    )
}

/// Resource URI of the condensed tool reference.
pub const HELP_URI: &str = "sous-chef://help";
/// Resource URI of the lint rule catalogue.
pub const RULES_URI: &str = "sous-chef://rules";

/// `sous-chef://help`: docs/mcp-tools.md condensed, generated from [`TOOLS`] and the
/// argument schemas so it never drifts from what the server accepts.
pub fn help_text() -> String {
    let mut out = String::new();
    out.push_str("# sous-chef tools\n\n");
    out.push_str(
        "Answers are pointers `layer:path:A-B` with one-line snippets; `item` returns exact \
         lines. Common to every tool (not repeated below): budget_tokens (default 600; rows \
         over budget are replaced by `+N more ...`) and format (`text`: tab-separated, \
         header row first; `json`: the same data). Each call refreshes the index first and \
         reports reindexed files only when something changed. Recipe arguments accept pn, \
         pn_pv or a pointer.\n",
    );
    for t in TOOLS {
        out.push_str(&format!(
            "\n## {} ({})\n{}\n",
            t.name,
            t.loading.as_str(),
            t.description
        ));
        let schema = (t.schema)();
        let required: Vec<&str> = schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
            for (name, prop) in props {
                if name == "budget_tokens" || name == "format" {
                    continue;
                }
                let desc = prop
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .replace('\n', " ");
                let req = if required.contains(&name.as_str()) {
                    " (required)"
                } else {
                    ""
                };
                out.push_str(&format!("- {name}{req}: {desc}\n"));
                // The values of an enum argument, each with its doc (left out of the
                // compact schema `tools/list` sends).
                for (value, doc) in enum_docs(&schema, prop) {
                    out.push_str(&format!("  - `{value}`: {doc}\n"));
                }
            }
        }
    }
    out
}

/// The documented values of an enum property of the full schema (`oneOf` of `const`
/// strings with a description each, reached through `$ref` / `anyOf`); empty otherwise.
fn enum_docs(schema: &JsonObject, prop: &serde_json::Value) -> Vec<(String, String)> {
    let mut node = prop;
    if let Some(first) = node
        .get("anyOf")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
    {
        node = first;
    }
    let def = node
        .get("$ref")
        .and_then(|v| v.as_str())
        .and_then(|r| r.strip_prefix("#/$defs/"))
        .and_then(|name| schema.get("$defs")?.get(name));
    let Some(list) = def.and_then(|d| d.get("oneOf")).and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|v| {
            let value = v.get("const")?.as_str()?;
            let doc = v.get("description")?.as_str()?.replace('\n', " ");
            Some((value.to_string(), doc))
        })
        .collect()
}

/// `sous-chef://rules`: the lint rule catalogue, generated by
/// [`tools::lint::rules_resource`] (rows `id severity scope flags`, plus the rules disabled
/// by config).
pub fn rules_text(config: Option<&Config>) -> String {
    tools::lint::rules_resource(config)
}

/// What to tell the client when a tool fails: one line, saying what to do next.
pub fn tool_error_message(tool: &str, e: &Error) -> String {
    let hint = match e {
        Error::Io(_) => "check that the file exists and is readable".to_string(),
        Error::Sqlite(_) => "if it persists, run `sous-chef index` or call `status`".to_string(),
        Error::Config { .. } => {
            "fix sous-chef.toml or the --build-dir/--layer flags, then restart the server"
                .to_string()
        }
        Error::NotFound { .. } => {
            "use `search` (mode=name) or `layers` to find the right name".to_string()
        }
        Error::Ambiguous { .. } => "call again with one of the candidates".to_string(),
        Error::Confined { .. } => "only files inside the workspace layers can be used".to_string(),
        Error::BitbakeUnavailable => {
            "set build_dir and init_script in sous-chef.toml, or start the server from an \
             initialised build environment"
                .to_string()
        }
        Error::Invalid { msg } if msg == "not implemented" => {
            format!("`{tool}` is not implemented in this build of sous-chef yet")
        }
        Error::Invalid { msg } if msg.starts_with("invalid arguments") => {
            format!("see the input schema of `{tool}` or the resource {HELP_URI}")
        }
        Error::Invalid { .. } => String::new(),
    };
    let base = one_line(&e.to_string());
    if hint.is_empty() {
        format!("{tool}: {base}")
    } else {
        format!("{tool}: {base}; {hint}")
    }
}

/// Collapses whitespace runs containing newlines so a message stays on one line.
fn one_line(s: &str) -> String {
    s.split(['\n', '\r'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the server runs against: a full context, or the reason it could not build one.
#[derive(Debug)]
enum State {
    Ready(Arc<Ctx>),
    // Boxed: `Config` is much larger than the `Arc` of the other variant.
    Degraded { config: Box<Config>, error: String },
}

/// The MCP server: serves [`TOOLS`] against one [`Ctx`].
#[derive(Debug, Clone)]
pub struct Server {
    state: Arc<State>,
}

impl Server {
    pub fn new(ctx: Arc<Ctx>) -> Server {
        Server {
            state: Arc::new(State::Ready(ctx)),
        }
    }

    /// A server whose context could not be built. Tools fail with `error`; `status` reports
    /// it.
    pub fn degraded(config: Config, error: String) -> Server {
        Server {
            state: Arc::new(State::Degraded {
                config: Box::new(config),
                error,
            }),
        }
    }

    fn config(&self) -> &Config {
        match &*self.state {
            State::Ready(ctx) => &ctx.config,
            State::Degraded { config, .. } => config,
        }
    }

    /// Runs one tool and maps the outcome to a tool result (never a protocol error).
    async fn run_tool(&self, entry: &ToolEntry, args: JsonObject) -> CallToolResult {
        let outcome = match &*self.state {
            State::Ready(ctx) => (entry.call)(ctx, args).await,
            State::Degraded { config, error } if entry.name == "status" => {
                parse_args(args).and_then(|a| tools::status::degraded(config, error, a))
            }
            State::Degraded { error, .. } => {
                return CallToolResult::error(vec![ContentBlock::text(format!(
                    "{}: sous-chef failed at startup ({}); fix it and restart the server \
                     (call `status` for details)",
                    entry.name,
                    one_line(error)
                ))]);
            }
        };
        match outcome {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(e) => {
                CallToolResult::error(vec![ContentBlock::text(tool_error_message(entry.name, &e))])
            }
        }
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new("sous-chef", env!("CARGO_PKG_VERSION")))
        .with_instructions(instructions())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            TOOLS.iter().map(ToolEntry::to_rmcp).collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let Some(entry) = find_tool(&request.name) else {
            let names: Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
            return Err(ErrorData::invalid_params(
                format!(
                    "unknown tool {:?}; available: {}",
                    request.name,
                    names.join(", ")
                ),
                None,
            ));
        };
        // `_meta.progressToken` (module `progress`): rmcp moves the request's `_meta` into
        // `context.meta`; the params' own copy is read too, in case it is kept there.
        let token = context
            .meta
            .get_progress_token()
            .or_else(|| request.meta.as_ref().and_then(|m| m.get_progress_token()));
        let args = request.arguments.unwrap_or_default();
        let Some(token) = token else {
            return Ok(self.run_tool(entry, args).await.into());
        };
        let (sink, rx) = progress::channel();
        let peer = context.peer.clone();
        let mut forwarder = tokio::spawn(progress::forward(
            rx,
            progress::MIN_INTERVAL,
            move |u: progress::Update| {
                let peer = peer.clone();
                let mut param = ProgressNotificationParam::new(token.clone(), u.progress)
                    .with_message(u.message);
                if let Some(total) = u.total {
                    param = param.with_total(total);
                }
                async move { peer.notify_progress(param).await.is_ok() }
            },
        ));
        let result = progress::scope(Some(sink), self.run_tool(entry, args)).await;
        // The sink is gone with the scope, so the forwarder sends what is pending and ends;
        // no notification may follow the response.
        if tokio::time::timeout(progress::FLUSH_WAIT, &mut forwarder)
            .await
            .is_err()
        {
            forwarder.abort();
        }
        Ok(result.into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(HELP_URI, "help")
                .with_title("sous-chef tool reference")
                .with_description("Every tool with its arguments, condensed.")
                .with_mime_type("text/markdown"),
            Resource::new(RULES_URI, "rules")
                .with_title("sous-chef lint rules")
                .with_description("The lint rule catalogue: id, severity, scope, what it flags.")
                .with_mime_type("text/tab-separated-values"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let contents = match request.uri.as_str() {
            HELP_URI => {
                ResourceContents::text(help_text(), HELP_URI).with_mime_type("text/markdown")
            }
            RULES_URI => ResourceContents::text(rules_text(Some(self.config())), RULES_URI)
                .with_mime_type("text/tab-separated-values"),
            other => {
                return Err(ErrorData::resource_not_found(
                    format!("unknown resource {other:?}; available: {HELP_URI}, {RULES_URI}"),
                    None,
                ));
            }
        };
        Ok(ReadResourceResult::new(vec![contents]).into())
    }
}

/// Serves `server` over stdio until the client closes the connection.
async fn serve_server(server: Server) -> Result<()> {
    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| Error::Invalid {
            msg: format!("mcp initialisation failed: {e}"),
        })?;
    running.waiting().await.map_err(|e| Error::Invalid {
        msg: format!("mcp server stopped: {e}"),
    })?;
    Ok(())
}

/// `sous-chef serve`: builds the context and serves MCP over stdio until the client closes.
/// If the context cannot be built, serves degraded (see the module docs) rather than exit.
pub async fn serve(config: Config) -> Result<()> {
    let server = match Ctx::open_labelled(config.clone()) {
        Ok(ctx) => {
            log(&format!(
                "serving {} tools on stdio for {} layers",
                TOOLS.len(),
                ctx.workspace.layers.len()
            ));
            Server::new(Arc::new(ctx))
        }
        Err((step, e)) => {
            let error = format!("{step}: {e}");
            log(&format!("serving degraded: {error}"));
            Server::degraded(config, error)
        }
    };
    serve_server(server).await
}

/// `sous-chef serve` when the configuration itself could not be loaded: serves degraded,
/// with `config` built from the command-line flags only.
pub async fn serve_degraded(config: Config, error: String) -> Result<()> {
    log(&format!("serving degraded: {error}"));
    serve_server(Server::degraded(config, error)).await
}

/// `sous-chef index`: refreshes the index and returns the report as text.
pub fn index_command(config: &Config) -> Result<String> {
    let ws = Workspace::discover(config)?;
    if let Err(e) = config.validate(&ws) {
        log(&format!("configuration problem: {e}"));
    }
    let mut index = Index::open(&ws)?;
    let report = index.refresh(&ws)?;
    Ok(format_refresh(&report, &index_path(&ws)))
}

/// The `sous-chef index` report: a summary line, then one pointer per reindexed file.
fn format_refresh(report: &crate::index::RefreshReport, path: &Path) -> String {
    let mut out = format!(
        "scanned {} files, reindexed {}, removed {} in {} ms ({})",
        report.scanned,
        report.reindexed.len(),
        report.removed,
        report.millis,
        path.display()
    );
    for loc in &report.reindexed {
        out.push_str(&format!("\nreindexed\t{loc}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_schema_is_an_object() {
        for t in TOOLS {
            let schema = (t.schema)();
            assert_eq!(
                schema.get("type").and_then(|v| v.as_str()),
                Some("object"),
                "{}",
                t.name
            );
            let tool = t.to_rmcp();
            assert_eq!(tool.name, t.name);
            let always = tool
                .meta
                .as_ref()
                .and_then(|m| m.0.get(ALWAYS_LOAD_META_KEY))
                .cloned();
            match t.loading {
                Loading::Always => assert_eq!(always, Some(true.into()), "{}", t.name),
                Loading::Deferred => assert!(tool.meta.is_none(), "{}", t.name),
            }
        }
    }

    /// The enum values a property accepts, `$ref`s resolved, in the full or compact schema.
    fn accepted(schema: &JsonObject, prop: &serde_json::Value) -> Vec<serde_json::Value> {
        let defs = schema.get("$defs").and_then(|v| v.as_object());
        let mut node = prop.clone();
        if let Some(any) = node.get("anyOf").and_then(|v| v.as_array()) {
            node = any[0].clone();
        }
        if let Some(r) = node.get("$ref").and_then(|v| v.as_str()) {
            let name = r.trim_start_matches("#/$defs/");
            node = defs.and_then(|d| d.get(name)).cloned().unwrap_or_default();
        }
        enum_values(&node).unwrap_or_default()
    }

    #[test]
    fn compaction_keeps_names_required_and_values() {
        for t in TOOLS {
            let full = (t.schema)();
            let compact = compact_schema(&full);
            assert!(!compact.contains_key("$schema") && !compact.contains_key("title"));
            assert_eq!(full.get("required"), compact.get("required"), "{}", t.name);
            let fp = full["properties"].as_object().unwrap();
            let cp = compact["properties"].as_object().unwrap();
            assert_eq!(
                fp.keys().collect::<Vec<_>>(),
                cp.keys().collect::<Vec<_>>(),
                "{}",
                t.name
            );
            for (name, p) in fp {
                assert_eq!(
                    accepted(&full, p),
                    accepted(&compact, &cp[name]),
                    "{} {name}",
                    t.name
                );
                assert_eq!(
                    p.get("default"),
                    cp[name].get("default"),
                    "{} {name}",
                    t.name
                );
            }
            let text = serde_json::to_string(&compact).unwrap();
            assert!(!text.contains("\"null\""), "{}: {text}", t.name);
            assert!(!text.contains("#/$defs/"), "{}: {text}", t.name);
        }
        // The values of `recipe show` are all there, in order.
        let recipe = compact_schema(&(TOOLS[0].schema)());
        assert_eq!(
            recipe["properties"]["show"]["enum"],
            serde_json::json!([
                "summary", "tree", "outline", "sources", "tasks", "packages", "cve"
            ])
        );
    }

    #[test]
    fn six_always_loaded_first() {
        let always: Vec<_> = TOOLS
            .iter()
            .take_while(|t| t.loading == Loading::Always)
            .map(|t| t.name)
            .collect();
        assert_eq!(
            always,
            ["recipe", "var", "overrides", "item", "search", "class"]
        );
        assert!(TOOLS[6..].iter().all(|t| t.loading == Loading::Deferred));
    }

    #[test]
    fn deferred_descriptions_are_one_line() {
        for t in TOOLS.iter().filter(|t| t.loading == Loading::Deferred) {
            assert!(!t.description.contains('\n'), "{}", t.name);
            assert!(t.description.len() <= 120, "{}", t.name);
        }
        for t in TOOLS.iter().filter(|t| t.loading == Loading::Always) {
            assert!(t.description.len() > 150, "{}", t.name);
        }
    }

    #[test]
    fn args_parse_and_reject() {
        let mut obj = JsonObject::new();
        obj.insert("name".into(), "hello".into());
        obj.insert("show".into(), "tree".into());
        let a: tools::recipe::Args = parse_args(obj).unwrap();
        assert_eq!(a.show, tools::recipe::Show::Tree);
        let a: tools::status::Args = parse_args(JsonObject::new()).unwrap();
        assert!(a.budget_tokens.is_none());
        let err = parse_args::<tools::recipe::Args>(JsonObject::new()).unwrap_err();
        assert!(err.to_string().contains("name"), "{err}");
    }

    #[test]
    fn instructions_prefer_the_index_and_name_deferred_tools() {
        let text = instructions();
        for word in ["recipe", "var", "item", "search", "Grep", "Read", HELP_URI] {
            assert!(text.contains(word), "{word}");
        }
        for t in TOOLS.iter().filter(|t| t.loading == Loading::Deferred) {
            assert!(text.contains(t.name), "{}", t.name);
        }
        assert!(!text.contains('\n'));
    }

    #[test]
    fn help_lists_every_tool_and_argument() {
        let help = help_text();
        for t in TOOLS {
            assert!(help.contains(&format!("## {} (", t.name)), "{}", t.name);
        }
        assert!(help.contains("- name (required):"));
        assert!(!help.contains("- budget_tokens:"));
        assert!(help.contains("budget_tokens (default 600"));
        // The per-value docs that the compact schema leaves out are here.
        assert!(help.contains("  - `cve`: "), "{help}");
        assert!(help.contains("  - `dead`: "), "{help}");
    }

    #[test]
    fn rules_resource_lists_every_rule() {
        let text = rules_text(None);
        let rows: Vec<&str> = text.lines().skip(1).collect();
        assert_eq!(rows.len(), crate::lint::all_rules().len());
        for r in crate::lint::all_rules() {
            assert!(
                rows.iter()
                    .any(|row| row.starts_with(&format!("{}\t", r.id()))),
                "{}",
                r.id()
            );
        }
        assert!(rows.iter().all(|r| r.split('\t').count() == 4));
        let mut cfg = Config::default();
        cfg.lint.disable = vec!["style".into()];
        assert!(rules_text(Some(&cfg)).ends_with("disabled in sous-chef.toml: style\n"));
    }

    #[test]
    fn tool_errors_are_one_line_and_actionable() {
        let cases = [
            (Error::not_implemented(), "not implemented in this build"),
            (
                Error::NotFound {
                    what: "recipe foo".into(),
                },
                "use `search`",
            ),
            (
                Error::Ambiguous {
                    what: "recipe hello".into(),
                    candidates: vec!["hello_1.0".into(), "hello_1.1".into()],
                },
                "hello_1.0, hello_1.1; call again",
            ),
            (Error::BitbakeUnavailable, "init_script"),
            (
                Error::Config {
                    msg: "bad\nvalue".into(),
                },
                "restart the server",
            ),
            (
                parse_args::<tools::recipe::Args>(JsonObject::new()).unwrap_err(),
                HELP_URI,
            ),
            (
                Error::Io(std::io::Error::other("gone")),
                "exists and is readable",
            ),
        ];
        for (e, needle) in cases {
            let msg = tool_error_message("recipe", &e);
            assert!(msg.starts_with("recipe: "), "{msg}");
            assert!(!msg.contains('\n'), "{msg}");
            assert!(msg.contains(needle), "{msg} lacks {needle}");
        }
    }

    #[test]
    fn cache_dir_is_stable_and_distinct() {
        let a = Workspace::default();
        let b = Workspace {
            build_dir: Some("/tmp/build".into()),
            ..Workspace::default()
        };
        assert_eq!(cache_dir(&a), cache_dir(&a));
        assert_ne!(cache_dir(&a), cache_dir(&b));
        let name = cache_dir(&b)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(name.len(), 16);
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(index_path(&b).file_name().unwrap(), "index.sqlite");
    }

    #[tokio::test]
    async fn progress_outside_a_sink_does_nothing_and_values_only_increase() {
        assert!(!progress::active());
        progress::step("ignored");
        progress::at(5.0, None, "ignored");
        let (sink, mut rx) = progress::channel();
        progress::scope(Some(sink), async {
            assert!(progress::active());
            progress::step("one");
            progress::at(1.0, None, "not above 1: dropped");
            progress::of(3, 10, "three of ten");
            progress::at(2.5, None, "below 3: dropped");
            progress::at(f64::NAN, None, "not a number: dropped");
            progress::step("four");
        })
        .await;
        let mut got = Vec::new();
        while let Some(u) = rx.recv().await {
            got.push((u.progress, u.total, u.message));
        }
        assert_eq!(
            got,
            [
                (1.0, None, "one".to_string()),
                (3.0, Some(10.0), "three of ten".to_string()),
                (4.0, None, "four".to_string()),
            ]
        );
        // The scope is over: nothing reaches anyone.
        assert!(!progress::active());
        progress::scope(None, async { assert!(!progress::active()) }).await;
    }

    #[tokio::test]
    async fn the_forwarder_rate_limits_keeps_the_newest_and_flushes_at_the_end() {
        use std::sync::Mutex as StdMutex;
        use std::time::{Duration, Instant};
        let (sink, rx) = progress::channel();
        let sent: Arc<StdMutex<Vec<(Instant, String)>>> = Arc::default();
        let log = Arc::clone(&sent);
        let interval = Duration::from_millis(100);
        let forwarder = tokio::spawn(progress::forward(rx, interval, move |u| {
            log.lock().unwrap().push((Instant::now(), u.message));
            async { true }
        }));
        progress::scope(Some(sink), async {
            // A burst: the first goes at once, the rest collapse into the newest.
            for i in 0..50 {
                progress::step(&format!("burst {i}"));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
            progress::step("late 1");
            progress::step("late 2");
        })
        .await;
        forwarder.await.unwrap();
        let sent = sent.lock().unwrap();
        let msgs: Vec<&str> = sent.iter().map(|(_, m)| m.as_str()).collect();
        assert_eq!(msgs, ["burst 0", "burst 49", "late 2"], "{msgs:?}");
        // At least one interval between the first two (the last is flushed on close).
        assert!(sent[1].0 - sent[0].0 >= interval - Duration::from_millis(5));
    }

    #[tokio::test]
    async fn the_forwarder_stops_when_sending_fails() {
        let (sink, rx) = progress::channel();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let n = Arc::clone(&calls);
        let forwarder = tokio::spawn(progress::forward(
            rx,
            std::time::Duration::from_millis(1),
            move |_| {
                n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { false }
            },
        ));
        progress::scope(Some(sink), async {
            progress::step("a");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            progress::step("b");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        })
        .await;
        forwarder.await.unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn index_report_format() {
        let report = crate::index::RefreshReport {
            scanned: 3,
            reindexed: vec!["meta-core:recipes-x/x/x_1.0.bb:1".parse().unwrap()],
            removed: 1,
            millis: 4,
        };
        let text = format_refresh(&report, Path::new("/c/index.sqlite"));
        assert_eq!(
            text,
            "scanned 3 files, reindexed 1, removed 1 in 4 ms (/c/index.sqlite)\n\
             reindexed\tmeta-core:recipes-x/x/x_1.0.bb:1"
        );
    }
}

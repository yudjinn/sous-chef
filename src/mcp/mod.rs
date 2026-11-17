//! MCP server and tool registry.
//!
//! Each tool lives in `tools/<name>.rs` with `pub struct Args` (Deserialize + JsonSchema)
//! and `pub async fn run(ctx: &Ctx, args: Args) -> Result<String>`. [`TOOLS`] is the single
//! table of name, description, loading class, schema and dispatch. [`Server`] serves that
//! table over stdio with `rmcp` by implementing `ServerHandler::list_tools` / `call_tool`
//! directly (no `#[tool]` macros), so tool files never depend on `rmcp`.
//!
//! Deferred tools: MCP has no notion of deferred loading. A deferred tool is still listed
//! in `tools/list` (every client can call it), but with a one-line description, and its
//! `_meta` carries `"sous-chef/loading": "deferred"` (`"always"` for the six full tools).
//! The server instructions name the deferred tools and point at `sous-chef://help` for
//! their arguments.
//!
//! Startup never kills the transport: if the configuration or workspace cannot be loaded,
//! the server still serves (degraded). Every tool then returns the startup error as a tool
//! error, and `status` reports it, so the client sees why instead of a dead server.
//!
//! Logging goes to stderr only; stdout belongs to the MCP transport.

pub mod tools;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListResourcesResult, ListToolsResult, MetaObject, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerConfig, Tool,
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
        // the index database, which a schema rebuild replaces.
        let handles = HandleStore::open(&index.cache_dir().join("handles"))
            .map_err(|e| ("opening the handle store", e))?;
        Ok(Ctx {
            config,
            workspace,
            index: tokio::sync::Mutex::new(index),
            handles,
            jobs: JobTable::new(),
        })
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
    /// The value of the `sous-chef/loading` key in a tool's `_meta`.
    pub fn as_str(self) -> &'static str {
        match self {
            Loading::Always => "always",
            Loading::Deferred => "deferred",
        }
    }
}

/// `_meta` key that tells the client whether a tool is always loaded or deferred.
pub const LOADING_META_KEY: &str = "sous-chef/loading";

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
    /// The rmcp tool definition advertised in `tools/list`.
    pub fn to_rmcp(&self) -> Tool {
        let mut meta = MetaObject::new();
        meta.0
            .insert(LOADING_META_KEY.into(), self.loading.as_str().into());
        Tool::new(self.name, self.description, (self.schema)()).with_meta(meta)
    }
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
        "Resolve a Yocto recipe (pn, pn_pv or pointer) and show its shape. show=summary \
         (default): recipe file and layer, all versions and the preferred one, each bbappend \
         with its layer and priority in application order, inherited classes, key values \
         (SUMMARY, LICENSE, SRC_URI and DEPENDS counts). show=tree: the include/inherit tree \
         in parse order, one row per file with the pointer of the statement that pulled it \
         in. show=outline: every task and function across the tree. An ambiguous name is an \
         error listing the candidates. Prefer this over reading .bb/.bbappend/.inc files."
    ),
    tool!(
        var,
        Always,
        "History and final value of a BitBake variable (e.g. SRC_URI, FILES:${PN}) for a \
         recipe, or for the global configuration when recipe is omitted, with optional \
         machine/distro. One row per operation in parse order: status (applied, inactive, \
         shadowed, removed), operator, override, pointer layer:path:A-B and snippet; then the \
         final value. flags=true also shows VAR[flag] operations. Prefer this over grepping \
         for assignments: it follows includes, inherits, bbappends and the override chain."
    ),
    tool!(
        overrides,
        Always,
        "The OVERRIDES machinery for a recipe/machine/distro. show=chain (default): OVERRIDES \
         entries lowest to highest priority with where each comes from. show=conditional: \
         every override-conditional statement in the recipe tree, marked active or inactive. \
         show=dead: statements whose override matches no known machine, distro, class or \
         package (likely typos)."
    ),
    tool!(
        item,
        Always,
        "Return exact source lines. Give loc (a pointer layer:path:A-B), or recipe + name \
         (e.g. do_install:append, searched across the recipe's tree), or file + name; \
         context adds lines around. Output is a header `layer:path:A-B sha256=<12 hex>` then \
         the lines byte-exact, usable as Edit old_string. Capped at 8 KB with a marker. This \
         is how Yocto file text should enter context instead of Read."
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
        "A bbclass. show=summary (default): its file(s) across layers and which one wins, \
         classes it inherits, tasks added, EXPORT_FUNCTIONS, and the variables it sets with \
         ?= (its interface). show=users: recipes and classes that inherit it, direct and \
         transitive, with pointers. show=outline: functions and tasks with pointers."
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
            }
        }
    }
    out
}

/// `sous-chef://rules`: the lint rule catalogue (docs/lint-rules.md tables) as
/// tab-separated rows `id severity scope description`, plus the rules disabled by config.
pub fn rules_text(config: Option<&Config>) -> String {
    const DOC: &str = include_str!("../../docs/lint-rules.md");
    let mut out = String::from("id\tseverity\tscope\tflags\n");
    for line in DOC.lines() {
        let Some(rest) = line.strip_prefix("| `") else {
            continue;
        };
        let cells: Vec<&str> = rest.trim_end_matches('|').split(" | ").collect();
        let [id, sev, scope, flags] = cells.as_slice() else {
            continue;
        };
        let id = id.trim_end_matches('`');
        let sev = match sev.trim() {
            "E" => "error",
            "W" => "warning",
            "I" => "info",
            other => other,
        };
        out.push_str(&format!(
            "{id}\t{sev}\t{}\t{}\n",
            scope.trim(),
            flags.trim()
        ));
    }
    if let Some(cfg) = config
        && !cfg.lint.disable.is_empty()
    {
        out.push_str(&format!(
            "disabled in sous-chef.toml: {}\n",
            cfg.lint.disable.join(", ")
        ));
    }
    out
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
        _context: RequestContext<RoleServer>,
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
        let args = request.arguments.unwrap_or_default();
        Ok(self.run_tool(entry, args).await.into())
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
            let meta = tool.meta.unwrap();
            assert_eq!(meta.0[LOADING_META_KEY], t.loading.as_str());
        }
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
            assert!(t.description.len() > 200, "{}", t.name);
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

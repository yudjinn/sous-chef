//! MCP server and tool registry.
//!
//! Each tool lives in `tools/<name>.rs` with `pub struct Args` (Deserialize + JsonSchema)
//! and `pub async fn run(ctx: &Ctx, args: Args) -> Result<String>`. [`TOOLS`] is the single
//! table of name, description, loading class, schema and dispatch. [`Server`] serves that
//! table over stdio with `rmcp` by implementing `ServerHandler::list_tools` / `call_tool`
//! directly (no `#[tool]` macros), so tool files never depend on `rmcp`.
//!
//! Stub (M0): T2.5 owns this file. Still to do there: `Ctx::open`, server instructions,
//! resources (`sous-chef://help`, `sous-chef://rules`), error mapping polish, `index_command`.

pub mod tools;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, JsonObject,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
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
    // Stub (M0): T2.5 implements it.
    pub fn open(_config: Config) -> Result<Ctx> {
        Err(Error::not_implemented())
    }
}

/// Whether the client should load a tool's full definition up front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loading {
    /// Always loaded: full description.
    Always,
    /// Deferred: short description, loaded on demand by the client.
    Deferred,
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
    /// The rmcp tool definition advertised in `tools/list`.
    pub fn to_rmcp(&self) -> Tool {
        Tool::new(self.name, self.description, (self.schema)())
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
        "Resolve a Yocto recipe (pn, pn_pv or pointer) and show its shape: file, versions, \
         bbappends in application order, inherited classes and key values (show=summary), \
         the include/inherit tree (show=tree), or every task and function (show=outline). \
         Prefer this over reading recipe files."
    ),
    tool!(
        var,
        Always,
        "History and final value of a BitBake variable for a recipe (or global configuration) \
         and machine/distro: every operation in parse order with status (applied, inactive, \
         shadowed, removed), pointer and snippet, then the final value. Prefer this over \
         grepping for assignments."
    ),
    tool!(
        overrides,
        Always,
        "The OVERRIDES chain for a recipe/machine/distro with where each entry comes from \
         (show=chain), every override-conditional statement marked active or inactive \
         (show=conditional), or overrides that match nothing known (show=dead)."
    ),
    tool!(
        item,
        Always,
        "Return exact source lines: expand a pointer layer:path:A-B, or fetch a named item \
         (variable, function, task) from a recipe's tree or one file. Output is byte-exact, \
         usable as Edit text. This is how file text should enter context."
    ),
    tool!(
        search,
        Always,
        "Search the Yocto layers: statement names (mode=name), file content by regex \
         (mode=regex) or full text (mode=fts), filtered by kind, layer, file kind or recipe \
         tree. Hits are pointers with their enclosing item and a snippet."
    ),
    tool!(
        class,
        Always,
        "A bbclass: which file wins across layers, classes it inherits, tasks, \
         EXPORT_FUNCTIONS and its ?= interface (show=summary); recipes and classes that \
         inherit it (show=users); functions and tasks (show=outline)."
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

/// The MCP server: serves [`TOOLS`] against one [`Ctx`].
#[derive(Debug, Clone)]
pub struct Server {
    ctx: Arc<Ctx>,
}

impl Server {
    pub fn new(ctx: Arc<Ctx>) -> Server {
        Server { ctx }
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
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
            return Err(ErrorData::invalid_params(
                format!("unknown tool {:?}", request.name),
                None,
            ));
        };
        let args = request.arguments.unwrap_or_default();
        let result = match (entry.call)(&self.ctx, args).await {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_string())]),
        };
        Ok(result.into())
    }
}

/// `sous-chef serve`: builds the context and serves MCP over stdio until the client closes.
pub async fn serve(config: Config) -> Result<()> {
    let ctx = Ctx::open(config)?;
    let running = Server::new(Arc::new(ctx))
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

/// `sous-chef index`: refreshes the index and returns the report as text.
// Stub (M0): T2.5 implements it.
pub fn index_command(_config: &Config) -> Result<String> {
    Err(Error::not_implemented())
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
}

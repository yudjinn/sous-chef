# Architecture and shared contracts

Everything here is a contract between agents. M0 creates it as compiling code. Change it only
with an explicit note in your hand-off.

## Module layout

```
src/
  main.rs              CLI entry (clap): serve | index | lint | hook; each calls one library
                       function (mcp::serve, mcp::index_command, lint::cli, hook::handle)
  lib.rs               module declarations only
  types.rs             shared types (below)
  error.rs             Error, Result
  config.rs            sous-chef.toml and workspace discovery
  parse/
    mod.rs             pub fn parse(src) -> ParsedFile
    lexer.rs           logical lines, continuation, comments
    ast.rs             Statement and friends
    parser.rs
  workspace/
    mod.rs             Workspace, Layer
    layers.rs          bblayers.conf and layer.conf
    resolve.rs         BBPATH lookup, class lookup, bbappend matching
  index/
    mod.rs             Index
    schema.rs          DDL and migrations
    scan.rs            refresh
    query.rs           read queries
  eval/
    mod.rs             evaluate(), Target
    datastore.rs       DataStore, VarOp
    expand.rs          ${VAR} expansion
    overrides.rs       finalisation
    replay.rs          file order
    providers.rs       PROVIDES / PREFERRED_PROVIDER / PREFERRED_VERSION selection
  bitbake/
    mod.rs
    env.rs             run a command in the build environment
    jobs.rs            background jobs
    envdump.rs         parse `bitbake -e`
    logs.rs            log digests
  handles.rs           handle store, digest, slice
  render.rs            Table, budget, pointer formatting
  lint/
    mod.rs             engine, Rule trait, registry
    helpers.rs         shared classification helpers (owned by T4.1)
    rules/mod.rs       registry (`ALL`), complete since M0
    rules/<group>_<name>.rs   one file per rule; id with `/` and `-` replaced by `_`
  scaffold/
    mod.rs             engine (dry run, confinement)
    recipe.rs  append.rs  layer.rs  class.rs  inc_split.rs  placement.rs
  mcp/
    mod.rs             Ctx, tool registry (`TOOLS`), rmcp server
    tools/mod.rs       module declarations
    tools/<name>.rs    one file per tool (plus var_authoritative.rs, filled by T5.5)
  hook.rs              Claude Code hook entry and dispatch
  hook/
    grep_read.rs  bash.rs  edit.rs     one file per handler
tests/
  fixtures/            synthetic layers (see M0)
  *.rs                 integration tests
plugin/                Claude Code plugin files (M7)
```

## Crates

M0 adds all of these. Confirm current versions and APIs from each crate's documentation when
adding them; do not rely on memory, especially for `rmcp`.

| Crate | Use |
|---|---|
| `rmcp` | Official Rust MCP SDK (stdio server, tool macros) |
| `tokio` | Async runtime, process spawning |
| `serde`, `serde_json`, `schemars` | Tool arguments and schemas |
| `rusqlite` (feature `bundled`) | Index |
| `sha2` | File and handle hashing |
| `ignore` | Directory walking |
| `regex` | Search, slices |
| `clap` (derive) | CLI |
| `toml` | Config |
| `thiserror`, `anyhow` | Errors (`thiserror` in the library, `anyhow` only in `main.rs`) |
| `tempfile`, `insta` (dev) | Tests and snapshots |

Pinned by M0 (Cargo.toml): rmcp 3.10 (features `server`, `macros`, `transport-io`), tokio 1.54
(`rt-multi-thread`, `macros`, `process`, `io-util`, `io-std`, `time`, `sync`, `signal`), serde 1
(`derive`), serde_json 1, schemars 1.2 (same major as rmcp's, so `JsonSchema` derives are
compatible), rusqlite 0.43 (`bundled`), sha2 0.11, ignore 0.4, regex 1.13, clap 4.6 (`derive`),
toml 1.2, thiserror 2, anyhow 1; dev: tempfile 3.27, insta 1.49.

## Shared types (`src/types.rs`)

```rust
/// 1-based, inclusive.
pub struct LineSpan { pub start: u32, pub end: u32 }

/// A pointer. `path` is relative to the layer root. Display: "layer:path:A-B" ("A" if A == B).
pub struct Loc { pub layer: String, pub path: PathBuf, pub lines: LineSpan }

pub enum FileKind { Recipe, Append, Include, Class, Conf }   // .bb .bbappend .inc .bbclass .conf

pub enum AssignOp {
    Set,          // =
    Default,      // ?=
    WeakDefault,  // ??=
    Immediate,    // :=
    Append,       // +=   (adds a space)
    Prepend,      // =+   (adds a space)
    AppendDot,    // .=   (no space)
    PrependDot,   // =.   (no space)
}

/// The left-hand side of an assignment or a function name, split.
/// "FILES:${PN}-dev:append" -> base "FILES", overrides ["${PN}-dev", "append"].
pub struct VarRef {
    pub base: String,
    pub overrides: Vec<String>,     // in source order, including append/prepend/remove
    pub flag: Option<String>,       // VAR[flag]
    pub legacy_syntax: bool,        // written with "_" override syntax
}
```

Helpers (M0): `LineSpan::{new, line}` and `Display`/`FromStr` (`A` or `A-B`);
`Loc: Display + FromStr`; `FileKind::{from_path, as_str}` (`as_str` is the index `kind`
text: `bb`, `bbappend`, `inc`, `bbclass`, `conf`) and `FromStr`; `AssignOp::{as_str, ALL}`,
`Display` and `FromStr` (operator text both ways). All derive `Debug, Clone, PartialEq, Eq,
Serialize`.

## Parser contract (`src/parse`)

```rust
pub fn parse(src: &str) -> ParsedFile;

pub struct ParsedFile {
    pub statements: Vec<Statement>,
    pub diagnostics: Vec<ParseDiagnostic>,   // never an Err: a bad line becomes Unknown + diagnostic
}

pub struct Statement {
    pub kind: StatementKind,
    pub span: LineSpan,              // whole statement, including continuation lines and body
    pub bytes: std::ops::Range<usize>,
}

pub enum StatementKind {
    Assign   { var: VarRef, op: AssignOp, value: String, export: bool },
    Export   { name: String },                       // bare `export VAR`
    Unset    { name: String, flag: Option<String> },
    Function { name: Option<VarRef>, lang: Lang, fakeroot: bool, body: LineSpan },  // None = anonymous python
    PyDef    { name: String, body: LineSpan },       // top-level `def`
    Inherit  { classes: String, deferred: bool },    // unexpanded text; may contain ${VAR}
    Include  { path: String, mode: IncludeMode },    // Include | Require | IncludeAll
    AddTask  { name: String, before: Vec<String>, after: Vec<String> },
    DelTask  { name: String },
    AddHandler { name: String },
    ExportFunctions { names: Vec<String> },
    Unknown  { text: String },
}

pub enum Lang { Shell, Python }
pub enum IncludeMode { Include, Require, IncludeAll }
pub struct ParseDiagnostic { pub span: LineSpan, pub message: String }
```

The types are in `parse/ast.rs` and re-exported from `parse`.

`value` is the raw text between the quotes with continuation lines joined; no expansion.

## Workspace contract (`src/workspace`)

```rust
pub struct Workspace {
    pub build_dir: Option<PathBuf>,
    pub layers: Vec<Layer>,              // in BBLAYERS order
}

pub struct Layer {
    pub name: String,                    // directory name, unique within the workspace
    pub root: PathBuf,                   // absolute
    pub collections: Vec<String>,        // BBFILE_COLLECTIONS
    pub priority: i32,                   // BBFILE_PRIORITY
    pub compat: Vec<String>,             // LAYERSERIES_COMPAT
    pub depends: Vec<String>,            // LAYERDEPENDS
    pub bbfiles: Vec<String>,            // BBFILES globs
    pub writable: bool,                  // from sous-chef.toml
}

impl Workspace {
    pub fn discover(cfg: &Config) -> Result<Workspace>;
    pub fn loc_for(&self, abs: &Path, lines: LineSpan) -> Option<Loc>;
    pub fn abs_path(&self, loc: &Loc) -> Option<PathBuf>;
    pub fn confine(&self, path: &Path) -> Result<PathBuf>;   // resolves symlinks; Err(Confined)
    pub fn layer(&self, name: &str) -> Option<&Layer>;
}

// resolve.rs
pub fn find_in_bbpath(ws: &Workspace, rel: &str, from: &Path) -> Option<PathBuf>;   // include/require
pub fn find_class(ws: &Workspace, name: &str, global: bool) -> Option<PathBuf>;
pub fn append_matches(append_file: &Path, recipe_file: &Path) -> bool;             // handles %
```

## Index contract (`src/index`)

Location: `$XDG_CACHE_HOME/sous-chef/<sha256 of workspace roots, 16 hex>/index.sqlite`.
The index is derived: any schema change bumps `user_version` and rebuilds from scratch.

```sql
layers(id INTEGER PK, name TEXT UNIQUE, root TEXT, priority INT, collections TEXT,
       compat TEXT, depends TEXT, writable INT)

files(id INTEGER PK, layer_id INT, path TEXT,        -- relative to layer root
      kind TEXT,                                      -- bb | bbappend | inc | bbclass | conf
      mtime INT, size INT, sha256 TEXT,
      pn TEXT, pv TEXT,                               -- from the file name, bb and bbappend only
      UNIQUE(layer_id, path))

statements(id INTEGER PK, file_id INT, ord INT,      -- order within the file
      kind TEXT,                                      -- assign | function | pydef | inherit | include |
                                                      -- addtask | deltask | addhandler | export_functions |
                                                      -- export | unset | unknown
      name TEXT,                                      -- full left-hand side as written
      base TEXT,                                      -- VarRef.base
      overrides TEXT,                                 -- ":"-joined
      flag TEXT, op TEXT, legacy INT,
      value TEXT,                                     -- full raw value (assign) or NULL
      start_line INT, end_line INT,
      snippet TEXT)                                   -- first line, trimmed to 120 chars
  INDEX(base), INDEX(file_id, ord), INDEX(kind, name)

edges(id INTEGER PK, src_file INT, kind TEXT,        -- require | include | include_all | inherit | bbappend
      target_text TEXT,                               -- as written
      target_file INT NULL,                           -- resolved, NULL if unresolved or variable
      line INT)
  INDEX(src_file), INDEX(target_file)

tasks(file_id INT, name TEXT, before TEXT, after TEXT, line INT)
provides(file_id INT, name TEXT)                     -- PN plus PROVIDES words that need no expansion
depends(file_id INT, kind TEXT,                      -- DEPENDS | RDEPENDS | PACKAGECONFIG
        pkg TEXT NULL, target TEXT, line INT)
statements_fts(name, snippet)                        -- FTS5, content = statements
handles(id TEXT PK,                                  -- "h:" + 10 hex
        sha256 TEXT,                                 -- blob at <cache dir>/handles/<2 hex>/<sha256>
        kind TEXT,                                   -- log | env | build | value | text
        command TEXT, bytes INT,
        created INT)                                 -- seconds since the Unix epoch
  INDEX(created)
```

As implemented in `index/schema.rs` (T2.1), which is the authority for the exact DDL:

- **Foreign keys are on.** `files.layer_id` and every `file_id` / `src_file` cascade on
  delete; `edges.target_file` is set to NULL. Deleting a `files` row removes its statements,
  edges, tasks, provides and depends, and leaves edges that pointed at it unresolved. A child
  row whose parent does not exist is rejected.
- **`statements_fts` is kept in sync by triggers.** Writers insert, update and delete
  `statements` only and never touch the FTS table.
- Extra indexes: `files(pn)`, `provides(name)`, and the `file_id` of tasks, provides, depends.
- Pragmas: WAL, `busy_timeout` 30 s, `synchronous=NORMAL`. `SCHEMA_VERSION` is stored in
  `user_version`; a mismatch, or a file that is not a database, deletes and recreates it.
- **Cache key:** first 16 hex of the sha256 over the build directory (if any) and every layer
  root, sorted and deduplicated, each followed by `\n`. Cache root: `$XDG_CACHE_HOME` if set
  and absolute, else `$HOME/.cache`. `index::cache_dir(ws)` is the single definition;
  `mcp::cache_dir` calls it.
- **`handles` rows do not survive a rebuild** (the blobs do); the handle store's retention
  sweeps blobs without a row. Open the handle store after `Index::open`, in
  `index.cache_dir().join("handles")`.

```rust
pub struct Index { /* rusqlite::Connection */ }
impl Index {
    pub fn open(ws: &Workspace) -> Result<Index>;
    pub fn open_in(cache_root: &Path, ws: &Workspace) -> Result<Index>;   // tests: explicit cache root
    pub fn open_at(path: &Path) -> Result<Index>;                         // tests: exact file
    pub fn refresh(&mut self, ws: &Workspace) -> Result<RefreshReport>;   // call at the start of every tool
    pub fn conn(&self) -> &rusqlite::Connection;                          // reads (query.rs)
    pub fn write<T>(&mut self, f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>) -> Result<T>;
                                                                          // BEGIN IMMEDIATE; the only write path
    pub fn path(&self) -> &Path;                                          // the database file
    pub fn cache_dir(&self) -> &Path;                                     // its directory
}
pub struct RefreshReport { pub scanned: u32, pub reindexed: Vec<Loc>, pub removed: u32, pub millis: u64 }
```

Read queries live in `index/query.rs` as plain functions returning row structs. Tools never
write SQL themselves.

## Evaluator contract (`src/eval`)

```rust
pub struct Target {
    pub recipe: Option<String>,      // None = global configuration only
    pub version: Option<String>,
    pub machine: Option<String>,     // None = as set in local.conf
    pub distro: Option<String>,
}

pub fn evaluate(index: &Index, ws: &Workspace, target: &Target) -> Result<DataStore>;

pub struct VarOp {
    pub var: VarRef, pub op: AssignOp, pub value: String,
    pub loc: Loc,
    pub via: Vec<Loc>,               // the include/inherit/bbappend statements that led here
}

pub enum OpStatus { Applied, InactiveOverride, Shadowed, Removed }

impl DataStore {
    pub fn history(&self, base: &str) -> Vec<(VarOp, OpStatus)>;
    pub fn value(&self, name: &str) -> Option<Value>;      // finalised and expanded
    pub fn overrides(&self) -> Vec<String>;                 // lowest priority first
    pub fn files(&self) -> &[FileVisit];                    // replay order, for `recipe`
}
pub struct Value { pub text: String, pub unevaluated_python: bool, pub unresolved: Vec<String> }
pub struct FileVisit { pub loc: Loc, pub kind: FileKind, pub via: Vec<Loc> }
```

`VarOp`, `OpStatus` and `DataStore` live in `eval/datastore.rs` (T3.1: `history`, `files`,
`fork`); `impl DataStore { value, overrides }` lives in `eval/overrides.rs` (T3.3). Fields of
`DataStore` are `pub(crate)` so both files can reach them.

## Rendering contract (`src/render.rs`)

All tool output goes through this module so every tool looks the same.

```rust
pub struct Table { /* header + rows of cells */ }
impl Table {
    pub fn new(header: &[&str]) -> Table;
    pub fn row(&mut self, cells: &[&str]);
    pub fn render(&self, budget_tokens: usize) -> String;
}
pub fn estimate_tokens(s: &str) -> usize;        // bytes / 4, rounded up
pub fn freshness_line(r: &RefreshReport) -> Option<String>;

#[derive(Deserialize, JsonSchema, clap::ValueEnum)] #[serde(rename_all = "lowercase")]
pub enum Format { Text, Json }                   // the `format` argument of every tool
pub const DEFAULT_BUDGET_TOKENS: usize = 600;
```

Output rules:

- Plain text, tab-separated, header row first. `format: "json"` on any tool returns the same
  data as JSON.
- Default `budget_tokens` is 600. Rows that do not fit are dropped from the end and replaced
  by one line: `+N more over budget_tokens` or `+N more (limit=L)`.
- A snippet is one line, at most 120 characters.
- Freshness is reported only when something changed:
  `reindexed 2 files: meta-foo:recipes-bar/baz/baz_1.0.bb, ...`.
- Static results that depend on python end with:
  `note: N values depend on python and were not evaluated; use authoritative=true`.
- No tool returns a whole file unless asked with an explicit line range covering it.

## Tool and rule stubs

M0 creates one file per tool in `src/mcp/tools/` and one per lint rule in
`src/lint/rules/`, each returning "not implemented", and registers all of them. Later tasks
fill their own file and touch nothing else in the registry.

Tool implementation shape:

```rust
// src/mcp/tools/<name>.rs
#[derive(Deserialize, JsonSchema)]
pub struct Args { /* ... */ pub budget_tokens: Option<usize>, pub format: Option<Format> }
pub async fn run(ctx: &Ctx, args: Args) -> Result<String>;
```

`Ctx` holds the `Workspace`, the `Index` (behind a mutex), the `Config`, the handle store and
the job table:

```rust
// src/mcp/mod.rs
pub struct Ctx {
    pub config: Config,
    pub workspace: Workspace,
    pub index: tokio::sync::Mutex<Index>,    // `ctx.index.lock().await`; may be held across .await
    pub handles: HandleStore,                // handles.rs, methods take &self
    pub jobs: JobTable,                      // bitbake/jobs.rs, methods take &self
}

pub enum Loading { Always, Deferred }
pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;
pub type ToolFn = for<'a> fn(&'a Ctx, JsonObject) -> ToolFuture<'a>;   // JsonObject = rmcp::model::JsonObject
pub struct ToolEntry {
    pub name: &'static str,
    pub description: &'static str,
    pub loading: Loading,
    pub schema: fn() -> Arc<JsonObject>,     // rmcp::handler::server::common::schema_for_type::<Args>
    pub call: ToolFn,                        // parse_args::<Args>, then tools::<name>::run
}
pub static TOOLS: &[ToolEntry];              // one `tool!(name, Always|Deferred, "desc")` row per tool
pub fn find_tool(name: &str) -> Option<&'static ToolEntry>;
pub fn parse_args<A: DeserializeOwned>(args: JsonObject) -> Result<A>;
pub struct Server;                           // impl rmcp::ServerHandler: list_tools / call_tool over TOOLS
pub async fn serve(config: Config) -> Result<()>;   // stdio
pub fn index_command(config: &Config) -> Result<String>;
```

The server does not use rmcp's `#[tool]` / `#[tool_router]` macros: tool files depend only
on `serde`/`schemars`, and the table is served by implementing `ServerHandler::list_tools`
and `call_tool` directly. The future returned by `run` must be `Send`.

As implemented in T2.5:

- **Deferred tools** are still listed by `tools/list`, with a one-line description, and every
  tool carries `_meta["sous-chef/loading"] = "always" | "deferred"`. MCP and rmcp have no
  standard field for this; T7.5 may add a client-specific one.
- **The server does not exit on a startup failure.** If the configuration or the workspace
  cannot be loaded it serves degraded (`serve_degraded`): `status` reports
  `state: "degraded"` with `startup_error`, and every other tool returns a one-line error
  naming the failed step. A `Config::validate` failure is logged and reported by `status` as
  `config_problem`, not fatal.
- Tool failures are `Ok(CallToolResult::error("<tool>: <message>; <hint>"))`. Unknown tools
  and unknown resource URIs are protocol errors.
- Resources `sous-chef://help` (generated from `TOOLS` and the argument schemas) and
  `sous-chef://rules` (generated from `docs/lint-rules.md`).
- The protocol version exercised by the integration test is `2025-06-18`.

Parser additions (T1.1/T1.2): `parse::parse_bytes(&[u8])` (lossy decode, then `parse`),
`parse::split_var(name, flag) -> VarRef`, and the public items of `parse::lexer`. An empty
function or `def` body has `body.end < body.start`; treat it as an empty range.
`AddTask.name`, `DelTask.name` and `AddHandler.name` hold every name given, joined by single
spaces, as written (no `do_` prefix added). `parse()` does not know the file kind and applies
the `.bb` grammar to `.conf` files too.

Workspace additions (T1.3/T1.5): `Layer.root` and `Workspace.build_dir` are canonical paths.
`workspace::BUILD_LAYER` (`"build"`) is the pseudo-layer for the build directory.
`layers::read_bblayers` returns the layers plus the `BBLAYERS` entries it could not resolve.
A `--layer` flag replaces the `layers` list of `sous-chef.toml`; `Config::load` returns
absolute paths.

Rendering additions (T2.4): `Table::{limited, note, len, is_empty, render_as, to_json,
render_json}`, `JsonTable`, `escape_cell` / `unescape_cell`, `snippet`, `truncate_value`,
`python_note`. Tools produce `format: "json"` output with `Table::render_as`. Notes and the
header always appear and count against the budget before any row. JSON output keeps the rows
the text layout keeps and is not itself budget-limited.

Handle store (T5.1): `HandleStore::open(dir)` (uses `<dir>/../index.sqlite`),
`open_with(dir, db, retention)`, `from_connection(dir, conn, retention)`; `put`, `put_file`,
`get`, `meta`, `list`, `total_bytes`, `apply_retention`. Retention limits are not read from
`sous-chef.toml` yet.

Lint (`src/lint/mod.rs`): `Scope`, `Severity` (ordered `Info < Warning < Error`,
deserialises lowercase), `Finding`, `Rule` and `all_rules()` as in M0 T0.3, plus:

```rust
pub struct LintContext<'a> {
    pub file: Option<&'a ParsedFile>,     // File and Recipe scope
    pub source: Option<&'a str>,          // raw text of that file
    pub kind: Option<FileKind>,
    pub data: Option<&'a DataStore>,      // Recipe scope
    pub layer: Option<&'a str>,           // Layer scope
    pub index: &'a Index,
    pub ws: &'a Workspace,
    pub loc: Loc,
}
#[derive(clap::Args)] pub struct LintCliArgs { target, rules (--rule), severity, format, fail_on, all }
pub fn cli(cfg: &Config, args: &LintCliArgs) -> Result<i32>;   // exit code
```

## Errors

`error::Error` (thiserror) with variants `Io(io::Error)`, `Sqlite(rusqlite::Error)`,
`Config { msg }`, `NotFound { what }`, `Ambiguous { what, candidates }`, `Confined { path }`,
`BitbakeUnavailable`, `Invalid { msg }`; `From` for `io::Error`, `rusqlite::Error` and
`toml::de::Error` (to `Config`). `Error::not_implemented()` is the M0 stub error.
`pub type Result<T, E = Error>`.
A tool error is returned to the client as an MCP tool error with a one-line message that says
what to do next (for example, list the candidates for an ambiguous recipe name).

## Configuration (`sous-chef.toml`, optional)

Looked up in the working directory, then given by `--config`.

```toml
build_dir = "build"                       # where conf/bblayers.conf lives; optional
layers = ["../meta-extra"]                # extra layer roots, used when there is no build dir
writable_layers = ["meta-mycompany"]      # scaffold may write only here
init_script = "poky/oe-init-build-env"    # sourced before any bitbake command

[bitbake]
enabled = true
allow_destructive = false                 # cleansstate, cleanall, devtool reset

[lint]
disable = ["style/variable-order"]

[output]
budget_tokens = 600
```

In code (`src/config.rs`): `Config { build_dir, layers, writable_layers, init_script,
bitbake: BitbakeConfig { enabled, allow_destructive }, lint: LintConfig { disable },
output: OutputConfig { budget_tokens }, work_dir }` (`work_dir` is not read from the file),
`Config::load(&CliOverrides) -> Result<Config>`, `Config::from_toml(text, origin)`,
`Config::validate(&Workspace)`. `CliOverrides { build_dir, config, layers, work_dir }` carries
the global flags `--build-dir`, `--config`, `--layer` (repeatable).

With no config and no `--build-dir`, discovery walks up from the working directory looking
for `conf/bblayers.conf`, then falls back to treating directories containing `conf/layer.conf`
under the working directory as layers.

## Conventions

- Rust edition 2024. `cargo fmt`, `clippy -D warnings`.
- No `unwrap`/`expect` outside tests. No panics on bad input files: the parser and scanner
  must survive any bytes.
- Paths stored in the index are relative to the layer root; convert at the edges.
- All file access goes through a confinement check: the resolved path (after symlinks) must be
  inside a layer root or the build directory.
- Tests use `tests/fixtures/`; never a real poky checkout except in the opt-in tests in M8.
- Logging goes to stderr only. stdout belongs to the MCP transport.

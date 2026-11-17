# Architecture and shared contracts

Everything here is a contract between agents. M0 creates it as compiling code. Change it only
with an explicit note in your hand-off.

## Module layout

```
src/
  main.rs              CLI entry (clap): serve | index | lint | hook; each calls one library
                       function (mcp::serve, mcp::index_command, lint::cli, hook::run)
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

## Release contract (`src/release.rs`, task T1.6)

Decided 2026-11-17 (see overview.md): Wrynose primary, Scarthgap fully supported, Kirkstone
the floor. Nothing outside `src/release.rs` compares codenames or version numbers; code asks
for a `Feature`.

```rust
/// Ordered oldest to newest, so `Release::Scarthgap < Release::Wrynose`.
pub enum Release { Kirkstone, Langdale, Mickledore, Nanbield, Scarthgap, Styhead, Walnascar,
                   Whinlatter, Wrynose }

impl Release {
    pub const FLOOR: Release = Release::Kirkstone;
    pub const DEFAULT: Release = Release::Wrynose;     // assumed when nothing can be detected
    pub fn from_codename(s: &str) -> Option<Release>;
    pub fn codename(self) -> &'static str;
    pub fn version(self) -> &'static str;              // "5.0"
    pub fn is_lts(self) -> bool;
    pub fn has(self, f: Feature) -> bool;
}

/// One variant per behaviour that differs between supported releases. Each has a row in the
/// "Releases" table of bitbake-semantics.md: first release, last release, source checked.
pub enum Feature { /* e.g. SplitClassDirs, InheritDefer, IncludeAll, UnpackDir,
                      WorkdirAsSourceDir, PrInWorkdir */ }

pub struct ReleaseInfo { pub release: Release, pub source: ReleaseSource, pub note: Option<String> }
pub enum ReleaseSource { Config, CoreNames, LayerCompat, Assumed }
```

`Workspace` gains `pub release: ReleaseInfo`, filled by `Workspace::discover` in this order:

1. `release = "<codename>"` in `sous-chef.toml` (`Config.release`);
2. `LAYERSERIES_CORENAMES` of the core layer's `conf/layer.conf` (the newest known name in it);
3. the newest supported release named in the `LAYERSERIES_COMPAT` of the writable layers
   (all layers if none is writable), for a layer repository checked out without OE-core;
4. `Release::DEFAULT`, with `source: Assumed`.

A codename newer than the table is treated as the newest known release, and one older than
the floor as the floor; both set `note`. `status` prints `release: <codename> (<source>)` and
the note.

As implemented (T1.6): `release::detect(configured, corenames, layers) -> Result<ReleaseInfo>`;
a configured name that is neither supported nor a known older release is a config error
listing the supported names. `Release::{ALL, NEWEST, is_older_than_floor}`,
`Feature::{ALL, first, last, verified}`, `ReleaseSource::as_str`, `ReleaseInfo::describe`
(what `status` prints, as one string in JSON too), `release::lint_releases(layer, release)`.
`LAYERSERIES_CORENAMES` is read from the shared datastore once every `layer.conf` has been
read (`layers::read_layer_confs_with_corenames`, `read_corenames`). The features are
`SplitClassDirs` (from Langdale), `InheritDefer` (from Scarthgap), `IncludeAll` (from
Walnascar), `UnpackDir` (from Styhead), `WorkdirAsSourceDir` (until Scarthgap) and
`PrInWorkdir` (until Mickledore); all are verified, with sources in the "Releases" section
of bitbake-semantics.md. `find_class` / `class_candidates` honour `SplitClassDirs`.
`Config.release: Option<String>`, `LintConfig.severity: BTreeMap<String, Severity>`.

`Layer::lint_releases(&self, ws: &Workspace) -> Vec<Release>`: the supported releases in the
layer's `compat`, plus `ws.release.release`, sorted and de-duplicated.

Which layers get the full list (decided 2026-11-17, overview.md "Lint and multi-release
layers"): a writable layer; a layer that is the lint target or contains it (a named layer,
a file or a recipe in it); every layer when the workspace has no writable layer. A layer
that is not writable and is linted only because a whole-workspace run has `--all` is
checked against the workspace release alone, and so is a file outside every layer. Scaffold
uses the full list for its target layer, which is always writable.

Who reads it:

- **Lint.** `Rule` gains `fn releases(&self) -> RangeInclusive<Release>` (default: all).
  The engine skips a rule for a unit when none of its releases is in the range, and
  `LintContext` gains `releases: &'a [Release]` so a rule can say which of them a finding
  applies to ("`inherit_defer` is not available in kirkstone, which this layer claims").
  Rule ids, the catalogue and `[lint] disable` are unchanged.
- **Parser.** Not release-aware: it accepts the union of the syntax of all supported
  releases. Syntax a release lacks is reported by lint.
- **Workspace, evaluator, logs.** Class directory search, the `WORKDIR` layout and similar
  go through `Feature`.
- **Scaffold.** Generated files follow `ws.release` (the `LAYERSERIES_COMPAT` value, `S` /
  `UNPACKDIR` conventions) and must lint clean for it.
- **BitBake backend.** How the build environment is initialised may differ per release
  (*verify* for Wrynose).

## Index contract (`src/index`)

Location: `$XDG_CACHE_HOME/sous-chef/<sha256 of workspace roots, 16 hex>/index.sqlite`.
The index is derived: any schema change bumps `user_version` and rebuilds from scratch. The
index also records the workspace release, and is rebuilt when the release changes (a new
`release` in `sous-chef.toml`), so what depends on it is recomputed.

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
    pub releases: &'a [Release],          // added by T4.1; see "As implemented in wave 3"
}
#[derive(clap::Args)] pub struct LintCliArgs { target, rules (--rule), severity, format, fail_on, all }
pub fn cli(cfg: &Config, args: &LintCliArgs) -> Result<i32>;   // exit code
```

### As implemented in wave 2

The signatures in the sections above are the contract; these are the additions each stream
made. The module docs in the named files are the detailed reference.

Path resolution (T1.4, `workspace::resolve`): `bbpath(ws)` reads the configured `BBPATH`
(it does not assume layer order). `Resolver` (`new`, `with_bbpath`, `with_known_files`,
`find_in_bbpath`, `include_all`, `find_class`, `class_candidates` with the winner first,
`resolve_with(ws, RefKind, text, from, vars) -> Option<Vec<Target>>`), `RefKind`, `Target`,
`expand_with(text, vars)`, `class_candidates(ws, name, global)`, `recipe_files(ws)` in
BitBake collection order. `layers::read_bbpath(build_dir, roots)`. Layer confs are read
with `parse()`; `include` / `require` / `include_all` inside them are followed when the
argument expands statically and the file is inside the build directory or a layer.

Scanner (T2.2, `index::scan`): `Index::refresh` calls `scan::refresh`. Column conventions:
`statements.op` holds the include mode, `inherit` / `inherit_defer`, or `shell` / `python`
(with a `fakeroot ` prefix) for functions; anonymous python is named `__anonymous`;
`bbappend` edges have `line` NULL and `target_text` set to the matching `.bb` name; every
edge is recomputed after any change. The build pseudo-layer row is `build`, priority 0.
`depends` holds unconditional `DEPENDS` / `RDEPENDS:<pkg>` and the build-dependency field of
`PACKAGECONFIG`. Helpers `pn_pv`, `written_name`.

Query layer (T2.3, `index::query`; every function takes `&Index`): rows `FileRow`,
`RecipeRow`, `StatementRow` (= `ItemRow`), `EdgeRow`, `TreeNode`, `UserRow`, `ClassFileRow`,
`LayerStats`, `ShadowRow`, filter `StatementFilter`. Functions `file`, `file_by_path`,
`file_by_abs`, `find_recipes`, `preferred_recipe`, `version_matches`, `appends_for`
(application order), `orphan_appends`, `statements_in`, `statement_by_name` (a `Vec`),
`statements_by_base`, `enclosing_item`, `search_names`, `search_fts`, `edges_from`,
`edges_to`, `tree`, `class_files(index, ws, name, global)`, `inheritors`, `layer_stats`,
`shadowed(index, ws)`, `vercmp`. `status` takes its file counts from `layer_stats`.

Evaluator core (T3.1 to T3.3, `eval::{datastore, expand, overrides}`):
- Building: `DataStore::new`, `fork` (copy-on-write), `record_visit(FileVisit)`,
  `apply(VarOp)`, `apply_with(VarOp, expander)`, `mark_exported(name)`,
  `unset(name, flag, loc, via)`, `expand_keys() -> Vec<String>` (BitBake's `expandKeys`;
  **the replay must call it once at the end of a recipe, after deferred inherits**).
- Reading: `value(name)`, `history(name)`, `flag_history(name, flag)`,
  `unsets(base) -> Vec<UnsetRecord>`, `overrides()`, `overrides_value()`, `expand(text)`,
  `flag`, `raw`, `raw_flag`, `flag_names`, `names`. `expand::{expand, Expander, Source,
  has_python}`.
- `unset` is not part of `history()` (`VarOp.op` is an `AssignOp`); a tool that shows
  history must also read `unsets()`.
- `OpStatus`: `InactiveOverride` (a condition is missing from `OVERRIDES`; legacy `_`
  syntax ops always get this), `Shadowed` (conditions hold, but the op does not contribute),
  `Removed` (applied, then every word it added was taken out by a `:remove`).
- Differences from BitBake, on purpose: a reference cycle is left in place and listed in
  `unresolved` (BitBake raises); an `OVERRIDES` that does not reach a fixed point uses the
  second result and adds `OVERRIDES` to `unresolved` (BitBake stops).

Environment runner (T5.3, `bitbake`): `BbEnv::detect`, `detect_in(cfg, ws, &HostEnv)`,
`probe(..) -> Result<BbEnv, String>` (the reason when nothing is found), `mode()`
(`init-script` or `initialised`), `command(program, args)`, `output(program, args, timeout)
-> CommandOutput`, `Supervised` (`spawn`, `child_mut`, `group`, `wait(timeout, cancel) ->
Finish { Exited, TimedOut, Cancelled }`), `environment_state(cfg, ws) -> EnvState`.
`validate_arg` accepts a name or a plain option; **callers must run `validate_name` on a
user-supplied target themselves**, because `command()` cannot tell a target from a flag.
`validate_name` rejects `:`, so a variable name like `FILES:${PN}` cannot be passed yet.
Unix only. The fake BitBake for tests is `tests/fixtures/fakebb/` (`FAKEBB_*` variables).

`slice` (T5.2): one selector per call; the cap is 8 KB or `budget_tokens * 4` when smaller;
`failure_blocks` takes its blocks from `bitbake::logs::detect`.

Log digests (T5.6, `bitbake::logs`): `detect(text) -> Vec<Failure>` (`Failure { kind, rank,
line, span, message, detail, frames }`), `first_error`, `failure_logfiles(output)`,
`failed_tasks(output)`, `digest(&DigestInput) -> Digest` and `Digest::render`,
`meta_from_text`, `meta_from_path`, `shell_trace`, `failing_function`,
`t_dir_from_bitbake(&BbEnv, recipe)`, `find_task_logs`, `logs_in_t_dir`,
`newest_cooker_log`, `tmp_dirs`, `normalize_task`, `check_name`,
`task_definition(index, ws, recipe, task) -> Option<Loc>` (recipe and bbappends first, then
included files, then a class default `<class>_<task>`).

Scaffold engine (T6.1, `scaffold`): `Kind` lives here (the tool re-exports it).
`Plan { files, notes, new_layer: Option<PathBuf> }`, `FileAction::as_str`.

```rust
pub trait Generator: Sync {
    fn kind(&self) -> Kind;
    fn options(&self) -> &'static [&'static str];        // any other option key is refused
    fn plan(&self, cx: &GenContext<'_>) -> Result<Plan>; // must not write
}
pub struct GenContext<'a> { ws, config, index: &Index, request: &Request }  // target_layer(), opt_str(key)
pub struct Request { kind, layer, name, version, options }
pub fn execute(ws, config, &mut index, &request, write, generator_override, &dyn PostWriteLint) -> Result<Outcome>;
```

A generator task registers itself by replacing `None` with `Some(&<file>::<Struct>)` in
its kind's arm of `scaffold::generator()` (an edit in `src/scaffold/mod.rs`, outside the
generator's own file). Paths must be absolute, pass `Workspace::confine` and lie in a
writable layer; a plan with `new_layer` may also modify `<build>/conf/bblayers.conf`.
`RuleLint` calls `lint::run` on exactly the written files that are BitBake metadata
(patches and other files a bbappend adds are skipped; default options, so
`[lint] disable`, `[lint.severity]` and inline suppression apply); files of a new layer are
skipped because they are not in the workspace until `bblayers.conf` lists it. `sha256_hex`, `file_sha256`, `line_count`; test seam `tools::scaffold::run_with`.

### As implemented in wave 3

As for wave 2: the additions each stream made; the module docs in the named files are the
detailed reference, and docs/mcp-tools.md describes the tools' behaviour.

Lookup tools (T2.6, T2.7, `mcp::tools::{recipe, item, search, class, layers}`): helpers the
other tools share: `recipe::resolve_recipe(index, name, version) -> RecipeRow` (a `pn`, a
`pn_pv`, or a pointer to a `.bb` or `.bbappend`; no exact name is `Ambiguous` with up to 20
candidates), `recipe::walk_recipe(index, &RecipeRow) -> Walk` (`Walk { nodes, statements,
files }`, `Node`, `NodeState`: the recipe tree in parse order, static), `recipe::file_ref`,
`file_label`, `outline_kind`; `item::matching(stmts, name)`, `read_confined(ws, loc)`,
`hex12`, `CAP_BYTES`; `search::item_label`, `DEFAULT_LIMIT`. **The walk is static: an
`include` or `inherit` whose argument needs evaluation is unresolved, and `base.bbclass` and
the `INHERIT` classes are not part of it.** `recipe` summary values were read from direct
statements in wave 3; since wave 4 they are evaluated (see "As implemented in wave 4").

Replay (T3.4, `eval::replay`): `evaluate(index, ws, target)` uses a process-wide
`ConfigCache` (`global_cache()`); `evaluate_with(&ConfigCache, index, ws, target)` takes an
explicit one; `ConfigCache::{new, passes, clear}`. **The caller must refresh the index
first.** `Target.recipe` is a recipe name (with `version`, `%` allowed, else the one
provider selection picks: see `eval::providers` in "As implemented in wave 4"), an
absolute path to a `.bb`, or `layer:path` ending in `.bb`; `None` evaluates the
configuration only; an unknown recipe is `NotFound`. `machine` / `distro` in the target are
set first and set again after every plain assignment to them in the configuration pass, so
they win over the configured values. A cache entry is keyed by (index file, workspace key, machine, distro)
and reused while every `.conf` keeps its sha256, the set of `.bbclass` paths is the same,
and every file the configuration pass visited keeps its sha256. Problems in the metadata
never fail the evaluation: `DataStore::replay() -> &ReplayNotes` has `recipe`, `appends`,
`layer_name` (`FILE_LAYERNAME`), `anonymous: Vec<AnonPython { loc, via, name, sets, opaque
}>`, `python_vars`, `references: Vec<RefIssue { loc, via, kind, text, problem: RefProblem
}>` (`Python`, `Undefined`, `NotFound`, `Cycle`) and `incomplete_keys`. A variable in
`python_vars` has `unevaluated_python` set on its value. **Tools should warn when
`anonymous` is not empty, and especially for an entry with `opaque: true` (it changes
variables through a computed name or hands `d` to other code); this is binding on wave 4's
`var`, `overrides` and `deps`.** Functions are stored like variables, with flags `func`,
`python`, `fakeroot`; the stored value is one pointer line per piece (`layer:path:A-B`, or
`layer:path:A-B -> class_do_x` for an `EXPORT_FUNCTIONS` stub); `DataStore::function(name)
-> Option<Vec<FunctionPiece { loc, calls }>>` gives the composed pieces, `is_function(name)`.
Anonymous python is stored under BitBake's name `__anon_<line>_<file>`. In the recipe pass
`PN`, `PV`, `BPN`, `BP`, `FILE_DIRNAME` and `THISDIR` are computed from `FILE` when their
stored value is python or absent. `via` chains: an included or inherited file gets its
parent's chain plus the statement; a bbappend's statements get the recipe file (line 1); a
`layer.conf` gets the chain of the last applied `BBLAYERS` operation plus that operation; an
`INHERIT` class gets the chain of the first applied `INHERIT` operation naming it plus that
operation; `conf/bblayers.conf`, `conf/bitbake.conf` and `base` get none. A `FileVisit.loc` spans line 1 to the
end of the file's last statement. Not replayed: `addtask`, `deltask`, `addhandler`; event
handlers and anonymous python never run.

Lint engine (T4.1, `lint`): `Rule` keeps `id`, `scope`, `default_severity`, `check` and
gains two methods with defaults: `releases() -> RangeInclusive<Release>` (default: every
supported release) and `default_enabled() -> bool` (default true; false means the rule runs
only when selected by its exact id or an alias of it); T4.4 made `hygiene/variable-order`
return false. `LintContext.releases` is oldest first and never empty: the releases the
unit's layer is checked against (see "Release contract"), or the workspace release alone
for a file outside every layer. `lint::run(ws, index, config, &Target, &Options) ->
Result<Report>` is the contract (the milestone's `run(target, options) -> Vec<Finding>` is
superseded): `Target { Workspace, Layer(name), Recipe { name, version }, Files { files,
lines } }`, `Options { rules, min_severity, all }`, `Report { findings, notes, files, layers
}`; `resolve_target(ws, index, text, work_dir)`; `run_with(.., rules, &dyn Evaluate)` (test
seam; `Replay` is the real evaluator); `check_text(rule, ws, index, loc, text, kind,
releases, data)` runs one file-scope rule on text outside the workspace, for rule fixture
tests; `applies`, `sort_findings`, `effective_severity`, `releases_for`, `allow_comment`,
`entry_matches`, `is_disabled`, `group_of`, `GROUPS`, `ALIASES` (`style/variable-order` ->
`hygiene/variable-order`), `summary_line`, `render_report`, `JsonReport`, `exit_code`,
`cli_with`. Helpers for rule authors in `lint/helpers.rs`: `finding`, `loc_at`,
`statement_text`; `release_list`, `releases_lacking`, `releases_having`, `feature_range`;
`ListKind`, `list_kind`, `list_kind_of`, `is_space_list`; `PACKAGE_ONLY_VARS`,
`PACKAGE_VARS`, `is_package_only`, `is_package_var`; `KnownOverrides { machines, distros,
classes, declared }` (`from_index`, `contains`, `is_machine_or_distro`). Selection,
configuration, suppression and exit codes are described in lint-rules.md ("Running lint").

Lint tool (T4.5, `mcp::tools::lint`): a wrapper over `lint::run` and `render_report`;
`rules_resource(config)` generates `sous-chef://rules` from the rule registry and the Flags
column of lint-rules.md (so a row there must keep four cells).

Jobs and the `bitbake` tool (T5.4, `bitbake::jobs`, `mcp::tools::bitbake`): `JobTable`
(`new`, `start(&BbEnv, JobSpec, dir) -> Result<Started>`, `get`, `running`, `last`, `list`,
`cancel`, `wait(id, timeout)`, `process_group`, `settle(&HandleStore)`), `JobInfo`,
`JobSpec { op, target, program, args, kind }` (`command_line`), `JobState { Running,
Succeeded, Failed, Cancelled }`, `Started { Started, AlreadyRunning }` (these five are
re-exported from `bitbake`), `tail`, `tail_of`. One job of any kind runs at a time. A job's output goes to
`<cache dir>/jobs/<pid>-<n>.out`; **a finished job's output becomes a handle only when
`settle` is next called, which the `bitbake` tool does on every call and `log` does with
`job`.** Cancellation terminates the process group (SIGTERM, SIGKILL after `KILL_GRACE`).
Tool side: `Op`, `plan(config, ws, &Args) -> Plan { spec, destructive }` (the allowlisted
argv; validates every name with `validate_name`), `check_destructive`, `status::job_line`.
`logs::meta_from_path_in(path, ws)` reads the version directory of a work directory for the
workspace's release (`Feature::PrInWorkdir`); `meta_from_path` returns it as it is.

Hooks (T7.1, T7.2, `hook`): `sous-chef hook` reads one Claude Code hook event on stdin and
writes nothing or one `hookSpecificOutput` object; the Grep / Read and edit handlers add
`additionalContext`, and since T7.3 the Bash handler replaces the output instead (see
"As implemented in wave 4"). The wire format it was built against is recorded in
`plugin/README.md`. `Handler` (`name`, `events`, `tools`, `handle(&mut Ctx, &Event) ->
Result<Option<Response>>`), `HANDLERS` and `handler_for` (`grep_read::GrepRead` for Grep and
Read; `bash::BashDigest` for Bash and `edit::EditFollowUp` for Edit, Write and MultiEdit,
stubs in wave 3 that T7.3 and T7.4 filled in), `Event` (`parse`, `input_str`, `abs_path`),
`Response` (`context` caps the text at `CONTEXT_CAP`, 9,000 bytes; `to_json`), `Ctx` (`env`,
`elapsed`, `over_budget`, `check_budget`, `note`), `Env { ws, index }` with
`fresh_file(abs)` (the index row only for a Yocto file inside the workspace whose bytes on
disk still match the indexed sha256), `Options`, `run(event_json, &Options)` (fail open: any
error, panic or overrun gives no output), `BUDGET` (150 ms, `SOUS_CHEF_HOOK_BUDGET_MS`
overrides), `WATCHDOG` (2 s, `arm_watchdog`, `begin_output`), the debug log `hook.log` in the
cache directory. `Index::open_read_only(path, busy)` opens an existing database read-only:
it never creates, rebuilds or writes, and a missing file or another schema version is an
error. A file edited since the last index refresh gets no Grep / Read annotation until the
index is refreshed. `Index::refresh_file(ws, abs)`, which reindexes one file, was added in
wave 4 for the edit hook.

### As implemented in wave 4

As for wave 3: the additions each stream made; the module docs in the named files are the
detailed reference, docs/mcp-tools.md describes the tools and docs/lint-rules.md the rules.

Provider selection (T3.7, `eval::providers`): `Providers::new(index, ws, machine, distro)`
(evaluates the configuration through the process-wide cache), `Providers::with_config(cache,
index, ws, target, config)`, `best_version(pn, version) -> Option<Selection>`,
`providers(name) -> ProviderSet`, `evaluate(&RecipeRow)`, `target_for(&RecipeRow)`,
`setting(name) -> Option<Setting>`, `config()`; `Selection { recipe, reason: VersionReason
{ Requested, Required, Preferred, Latest }, setting, default_preference, notes }` with
`describe()`; `ProviderSet { name, providers, reason: ProviderReason { None, Only,
PreferredProvider, SameName, FirstOfSeveral }, preferred_provider, excluded, notes }` with
`selected()`; `Provider { pn, selection, provides_loc, python }`; `Setting { name, value, loc,
python }`; `preferred_recipe(index, ws, pn, version, machine, distro)`;
`recipes_using(index, &FileRow)` (the recipes a `.bb`, bbappend, include or class is part
of). **A recipe name in `Target` is now resolved through provider selection after the
configuration pass, so `REQUIRED_VERSION`, `PREFERRED_VERSION` and `DEFAULT_PREFERENCE` are
honoured; a path target is resolved before it. The doc comment of `eval::evaluate` still
says "preferred by layer priority and version" and is out of date.** `query::preferred_recipe`
(layer priority and version only, no configuration) is still what `item`, `search`, the
`bbappend` generator and `bitbake::logs` use. Not handled by provider selection:
`BBCLASSEXTEND` variants (`-native` names find no provider), `BBMASK`, `SKIP_RECIPE` and
`COMPATIBLE_*`.

Query layer (T3.7, `index::query`): `DependRow { file, kind, pkg, target, line }`,
`files_providing(index, name)`, `provides_of(index, file_id)`, `depends_on(index, target)`
(rows of the `provides` and `depends` tables).

Variable tools (T3.5, T3.6, T3.7, `mcp::tools::{var, overrides, deps}`): helpers shared by
the three in `var`: `recipe_target(index, recipe, machine, distro) -> Target` (an exact `pn`
is passed to the evaluator as a name; a `pn_pv`, a pointer or an inexact name goes through
`recipe::resolve_recipe` and becomes `layer:path`), `replay_notes(ds, name)` (the warnings
about anonymous python and unfollowed references), `status_str`, `op_snippet`,
`override_str`, `MAX_FINAL_CHARS` (600). **All three warn about anonymous python through
`replay_notes`, as wave 3 required.** `recipe` summary key values come from `eval::evaluate`
of the chosen file, and a bare `pn` is resolved with `providers::preferred_recipe`.
`overrides` with `show: dead` evaluates the configuration once per machine conf and once
per distro conf of the workspace (cached by `ConfigCache`). `deps` has no machine or distro
argument; `reverse` is static (index `depends` table).

Lint rules (T4.2, T4.3, T4.4, `lint::rules`): the 38 rules are filled in; no rule id was
added or removed. Public items besides the rule structs: `org_upstream_edit::{TIMEOUT,
MAX_FINDINGS}` (5 s, 20). `hygiene/variable-order` returns false from `default_enabled`.
Engine changes: an alias group (`style`) selected with `--rule` counts as naming its rules
by exact id, so it runs rules that are off by default or disabled; a recipe target is
resolved with `providers::preferred_recipe` (the file BitBake uses). Recipe-scope rules run
once per `.bb` of the target, so `override/dead` and `override/shadowed` report only when
the `.bb` is linted, and their findings can be in another layer's bbappend. **The releases
a layer is checked against follow the decided rule in "Release contract" (a layer that is
the lint target, or contains it, gets its full list even when not writable).**

Index (T7.4, `index`): `Index::refresh_file(ws, abs) -> FileRefresh` (`Unchanged`,
`Reindexed(Loc)`, `Removed(Loc)`, `NotIndexed`) reindexes one file without walking the layers:
stat and hash check, reparse, its rows replaced in one transaction. When the file was
already indexed and is not a `conf/layer.conf`, only its own reference edges are
recomputed (its `bbappend` edges and the edges pointing at it are kept); a new file, a
deleted one, or a changed `conf/layer.conf` recomputes every edge as the full refresh does.
A path the full walk would skip, or a file that is not BitBake metadata, is `NotIndexed`.
**`refresh_file` does not sync the `layers` table: a layer the index does not know is an
error, and an edit to a `layer.conf` (priority, collections, compat) is not reflected in
`layers` until the next full `refresh`.** `Index::open_existing(path, busy)` opens an
existing database read-write (never creates or rebuilds it; another schema version is an
error; a lock held longer than `busy` makes a write fail).

Hooks (T7.3, T7.4, `hook::{bash, edit}`): `hook::Ctx::config()` (the configuration the
workspace was discovered with); `HandleStore::without_retention(dir, conn)` (opens the
store on an existing connection without the retention sweep, so a short-lived process never
removes a blob the server is writing).

- **Bash** (`BashDigest`, `runs_bitbake(command)`, `MIN_BYTES` 3 KiB, `TAIL_LINES` 10):
  for a `bitbake`, `devtool` or `bitbake-layers` command (the first word of any command in
  the line, after variable assignments and `sudo`, `time`, `env`, `nice`, `exec`,
  `command`, `nohup`) whose stdout plus stderr is over 3 KiB, the hook stores the full output
  under a `build` handle and **replaces** the output with `updatedToolOutput`: `stdout`
  becomes one line naming the handle and size, then the log digest (`bitbake::logs`) when a
  failure is detected, else the last 10 lines, then a `slice` hint; `stderr` is emptied; the
  other fields of `tool_response` are kept. Other commands, smaller outputs and image
  output (`isImage`) are left untouched. A missing index, a locked database (50 ms busy timeout) or any write error
  gives no output, so the original output stays.
- **Edit** (`EditFollowUp`, `MAX_FINDINGS` 5, `WRITE_BUSY` 50 ms): for a BitBake file in
  the workspace, reindexes it with `refresh_file` (opened with `open_existing`), runs the
  file-scope rules that are enabled by default and not disabled through `lint::run` on that
  file, and adds as `additionalContext` at most five findings at warning level or above
  (after `[lint.severity]` and suppression), then `+N more`; plus one line when the file is
  in a layer that is not writable. Nothing when there are no findings and the layer is
  writable. A file the index does not hold, or a locked database, gives no output. The tool
  output is never replaced.

Scaffold (T6.2, T6.3, `scaffold::{recipe, append, layer, class}`): the four generators
are registered in `scaffold::generator()` (`RecipeGen`, `AppendGen`, `LayerGen`,
`ClassGen`), each with its `OPTIONS`; `class::{DEFAULT_AFTER, DEFAULT_BEFORE}` (`do_install`,
`do_build`). `inc_split` and `where` are still `None` (wave 5). The shared helpers
(`opt_words`, `quote`, `check_parses`, `assign_list`...) are `pub(crate)` in `recipe.rs`.
**`execute` refreshes the index before a generator plans, so `GenContext.index` is
current.** The post-write lint skips files that are not BitBake metadata. Generators read
the target layer's releases with `Layer::lint_releases`. A layer created by `layer` is not
part of the running server's workspace until it discovers the workspace again (restart),
so its files are not linted after the write and cannot be a scaffold target until then.

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
release = "scarthgap"                     # optional; overrides detection

[bitbake]
enabled = true
allow_destructive = false                 # cleansstate, cleanall, devtool reset, devtool finish

[lint]
disable = ["style/variable-order"]

[lint.severity]                           # optional override by rule id or group
"hygiene/workdir-s" = "error"

[output]
budget_tokens = 600
```

In code (`src/config.rs`): `Config { build_dir, layers, writable_layers, init_script,
bitbake: BitbakeConfig { enabled, allow_destructive }, lint: LintConfig { disable, severity },
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

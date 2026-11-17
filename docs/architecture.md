# Architecture and shared contracts

Everything here is a contract between agents. M0 creates it as compiling code. Change it only
with an explicit note in your hand-off.

## Module layout

```
src/
  main.rs              CLI entry (clap): serve | index | lint | hook
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
    rules/<id>.rs      one file per rule
  scaffold/
    mod.rs             engine (dry run, confinement)
    recipe.rs  append.rs  layer.rs  class.rs  inc_split.rs  placement.rs
  mcp/
    mod.rs             server and tool registry
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
```

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
```

```rust
pub struct Index { /* rusqlite::Connection */ }
impl Index {
    pub fn open(ws: &Workspace) -> Result<Index>;
    pub fn refresh(&mut self, ws: &Workspace) -> Result<RefreshReport>;   // call at the start of every tool
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
```

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
the job table.

## Errors

`error::Error` (thiserror) with variants `Io`, `Sqlite`, `Config`, `NotFound { what }`,
`Ambiguous { what, candidates }`, `Confined { path }`, `BitbakeUnavailable`, `Invalid { msg }`.
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

# M2: Index and pointer tools

The persistent index, the MCP server, and the first six tools. After M2 an agent can find and
read recipe items without Grep or Read.

Read first: index and rendering contracts in [architecture.md](../architecture.md),
[mcp-tools.md](../mcp-tools.md).

Parallelism: **T2.4** and **T2.5** need only M0 and can start in wave 1. **T2.1 -> T2.2 ->
T2.3** is one stream after M1. **T2.6** and **T2.7** can be split between two agents once
T2.3, T2.4 and T2.5 are merged.

---

## T2.1 Schema

**Depends on:** M0. **Owns:** `src/index/schema.rs`, `src/index/mod.rs`

- Create the tables and indexes in architecture.md. WAL mode, `busy_timeout` 30 s,
  foreign keys on.
- `Index::open`: compute the cache path from the workspace roots, create directories, check
  `user_version`; on mismatch delete and recreate (the index is derived).
- Writes use `BEGIN IMMEDIATE` so two server processes on one workspace do not corrupt it.

**Done when:** tests open a fresh index, reopen it, and force a version mismatch rebuild.

## T2.2 Scanner and freshness

**Depends on:** T2.1, T1.2, T1.4. **Owns:** `src/index/scan.rs`

`Index::refresh`:

1. Sync the `layers` table with the workspace.
2. Walk each layer (`ignore` crate; skip `.git`, build output directories and anything not
   `.bb`, `.bbappend`, `.inc`, `.bbclass`, `.conf`). Also index `build/conf/*.conf`, under a
   pseudo-layer named `build`.
3. For each file compare mtime and size with its row. If different, hash it; if the hash
   differs, reparse and replace its rows in one transaction.
4. Delete rows for files that disappeared.
5. Fill `statements` (with full `value`), `tasks`, `edges` (resolving targets with T1.4 where
   no evaluation is needed, otherwise `target_file = NULL`), `provides`, `depends` (split
   `DEPENDS`, `RDEPENDS:<pkg>` and `PACKAGECONFIG[...]` values that need no expansion), and
   the FTS table. Derive `pn` / `pv` from recipe and append file names.
6. After any change, re-resolve edges whose target was unresolved or pointed at a changed or
   removed file, and recompute `bbappend` edges.

Return a `RefreshReport`. Parsing changed files may run on a thread pool; database writes are
single-threaded.

**Done when:**
- Tests: first scan of the fixture workspace yields expected row counts; a second scan
  reindexes nothing; touching a file without changing it reindexes nothing (hash equal);
  editing, adding and deleting a file each update exactly that file's rows; a new `.inc`
  resolves a previously unresolved edge.
- A no-change refresh of the fixture workspace takes under 20 ms (assert generously).

## T2.3 Query layer

**Depends on:** T2.1 (schema), T2.2 for tests. **Owns:** `src/index/query.rs`

Plain functions returning row structs. Needed by M2 tools:

- `find_recipes(name) -> Vec<RecipeRow>` (all versions, all layers), `appends_for(recipe)`.
- `statements_in(file, filter)`, `statement_by_name(file, name)`,
  `statements_by_base(base, scope)`.
- `edges_from(file)`, `edges_to(file)`, `tree(root_file) -> Vec<TreeNode>` (include/inherit
  closure in statement order, cycle safe, static resolution only).
- `class_files(name)`, `inheritors(class, transitive)`.
- `search_names(query, filters, limit)`, `search_fts(query, filters, limit)`.
- `enclosing_item(file, line) -> Option<ItemRow>` (used by `search` and by hooks in M7).
- `layer_stats()`, `shadowed()`, `orphan_appends()`.

Preferred-version selection lives here as `preferred_recipe(name, version)`, using layer
priority and version comparison only; M3 refines it with `PREFERRED_VERSION`.

**Done when:** each function has a fixture test; `tree` handles a cyclic include without
looping; version comparison orders `1.10` after `1.9` and handles `git`/`+` suffixes.

## T2.4 Rendering

**Depends on:** M0. **Owns:** `src/render.rs`

Implement `Table`, `estimate_tokens`, `freshness_line`, the JSON alternative, snippet
trimming, value truncation with a marker, and the truncation lines described in
architecture.md.

**Done when:** tests cover budget cut-off (the marker line itself fits in the budget), limit
cut-off, empty table, tabs and newlines inside cells (escaped), and JSON output parity.

## T2.5 MCP server

**Depends on:** M0. **Owns:** `src/mcp/mod.rs`, `src/mcp/tools/status.rs`, the `serve` and
`index` branches in `src/main.rs`

- Stdio MCP server with `rmcp`. Read its current documentation and examples first.
- `Ctx` as described in architecture.md; build it once at startup.
- Register the 14 tools from the stubs. Six are described fully; the eight deferred tools get
  a one-line description. Server instructions (a short paragraph) tell the client to prefer
  `recipe` / `var` / `item` / `search` over Grep and Read for Yocto files.
- Map `error::Error` to MCP tool errors with one-line actionable messages.
- Expose resources `sous-chef://help` and `sous-chef://rules`.
- Implement `status`.
- `sous-chef index` runs a refresh and prints the report (useful for warm-up and debugging).
- Logging to stderr only.

**Done when:** an integration test spawns `sous-chef serve` on the fixture workspace, performs
`initialize`, `tools/list` (14 tools) and a `status` call over stdio, and checks the reply.

## T2.6 Tools: `recipe`, `item`

**Depends on:** T2.3, T2.4, T2.5. **Owns:** `src/mcp/tools/recipe.rs`, `src/mcp/tools/item.rs`

Implement per mcp-tools.md.

- `recipe`: name resolution with helpful ambiguity errors; `summary`, `tree`, `outline`.
  Until M3 exists, key values in `summary` come from direct statements only and say so.
- `item`: by pointer, by `recipe` + `name` (search the tree; if several statements match,
  e.g. `do_install` and `do_install:append`, list them as pointers instead of guessing), by
  `file` + `name`. Text is returned byte-exact. Header carries the file hash prefix.

**Done when:** snapshot tests for each mode on fixture recipes; `item` output for a function
equals the source lines exactly; the 8 KB cap and its marker are tested.

## T2.7 Tools: `search`, `class`, `layers`

**Depends on:** T2.3, T2.4, T2.5. **Owns:** `src/mcp/tools/search.rs`,
`src/mcp/tools/class.rs`, `src/mcp/tools/layers.rs`

Implement per mcp-tools.md.

- `search`: three modes; regex mode reads files (confined) and annotates each hit with
  `enclosing_item`; filters; limit and budget.
- `class`: summary (with shadowing across layers and the `?=` interface), users (direct and
  transitive), outline.
- `layers`: `list`, `shadowed`, `appends`.

**Done when:** snapshot tests for each mode; a regex hit inside `do_install:append` shows that
item and its range.

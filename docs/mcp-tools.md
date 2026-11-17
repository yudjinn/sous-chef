# MCP tools

Common to every tool:

- `budget_tokens` (default 600) and `format` (`"text"` default, or `"json"`).
- Output is a tab-separated table through `render.rs`; see the output rules in
  [architecture.md](architecture.md).
- Each call starts with `Index::refresh`. Freshness is reported only if something changed.
- Recipe arguments accept `pn`, `pn_pv` or a pointer. An ambiguous name is an error that
  lists the candidates.
- `machine` and `distro` default to the values in `local.conf` when a build directory exists.

The first six are always loaded. The rest are deferred (short descriptions, loaded on demand).

## Always loaded

### `recipe`
Resolve a recipe and show its shape.

| Arg | Type | Notes |
|---|---|---|
| `name` | string | required |
| `version` | string? | default: preferred version |
| `show` | `"summary"` \| `"tree"` \| `"outline"` | default `summary` |

- `summary`: the recipe file, layer, all versions and which is preferred, each bbappend with
  its layer and priority (in application order), classes inherited, key values (`SUMMARY`,
  `LICENSE`, `SRC_URI` count, `DEPENDS` count).
- `tree`: the include/inherit tree in parse order, one row per file with the pointer of the
  statement that pulled it in. Unresolved targets are shown as such.
- `outline`: every task and function across the tree: name, pointer, defining file kind.

### `var`
History and final value of a variable. The main tool.

| Arg | Type | Notes |
|---|---|---|
| `name` | string | base name, e.g. `SRC_URI`; `FILES:${PN}` allowed |
| `recipe` | string? | omit for global configuration |
| `machine`, `distro` | string? | |
| `authoritative` | bool | default false; true runs `bitbake -e` (M5) |
| `flags` | bool | also show `VAR[flag]` operations |

Output: one row per operation in parse order: `status` (`applied`, `inactive`, `shadowed`,
`removed`), `op`, `override`, pointer, snippet. Then `final:` with the value (truncated to
600 characters, with a handle if longer) and, if relevant, the python note.

### `overrides`
| Arg | Type | Notes |
|---|---|---|
| `recipe` | string? | |
| `machine`, `distro` | string? | |
| `show` | `"chain"` \| `"conditional"` \| `"dead"` | default `chain` |

- `chain`: `OVERRIDES` entries, lowest to highest priority, each with where it comes from.
- `conditional`: every override-conditional statement in the recipe tree, marked active or
  inactive.
- `dead`: statements whose override matches no known machine, distro, class or package
  (likely typos).

### `item`
Expand a pointer or fetch a named item. This is how text enters context.

| Arg | Type | Notes |
|---|---|---|
| `loc` | string? | `layer:path:A-B` |
| `recipe` + `name` | string? | e.g. `recipe: "busybox", name: "do_install:append"`; searches the recipe tree |
| `file` + `name` | string? | a single file |
| `context` | int | extra lines around, default 0 |

Returns a header line `layer:path:A-B sha256=<12 hex>` then the exact lines, unmodified, so
they can be used as Edit strings. Capped at 8 KB with a `+N more lines (lines=A-B)` marker.

### `search`
| Arg | Type | Notes |
|---|---|---|
| `query` | string | required |
| `mode` | `"name"` \| `"regex"` \| `"fts"` | default `name` (statement names, substring) |
| `kind` | list? | `assign`, `function`, `task`, `inherit`, `include`, ... |
| `layer`, `file_kind`, `recipe` | filters | `recipe` restricts to that recipe's tree |
| `limit` | int | default 30 |

Each hit: pointer, kind, enclosing item with its range, snippet. `regex` mode searches file
content line by line; other modes use the index.

### `class`
| Arg | Type | Notes |
|---|---|---|
| `name` | string | required |
| `show` | `"summary"` \| `"users"` \| `"outline"` | default `summary` |

- `summary`: the file(s) (a class can exist in several layers; show which one wins), classes
  it inherits, tasks added, `EXPORT_FUNCTIONS`, variables it sets with `?=` (its interface).
- `users`: recipes and classes that inherit it, direct and transitive, with pointers.
- `outline`: functions and tasks with pointers.

## Deferred

### `layers`
`show`: `"list"` (name, priority, collections, compat, depends, writable, file counts),
`"shadowed"` (same recipe or class provided by more than one layer, and which wins),
`"appends"` (bbappends and what they match, flagging those that match nothing).

### `deps`
| Arg | Notes |
|---|---|
| `recipe` | required |
| `kind` | `"build"` (DEPENDS), `"runtime"` (RDEPENDS per package), `"packageconfig"`, `"reverse"`, `"providers"` |
| `depth` | default 1 |

`providers` takes a name such as `virtual/kernel` and lists providers and the selected one
with the pointer of the `PREFERRED_PROVIDER` assignment.

### `lint`
| Arg | Notes |
|---|---|
| `target` | a recipe, a file pointer, or a layer name |
| `rules` | optional list of ids or groups to run |
| `severity` | minimum: `info` \| `warning` \| `error` |

Rows: severity, rule id, pointer, message. See [lint-rules.md](lint-rules.md).

### `scaffold`
| Arg | Notes |
|---|---|
| `kind` | `recipe` \| `bbappend` \| `layer` \| `class` \| `inc_split` \| `where` |
| `layer` | target layer (must be writable) |
| `name`, `version` | as relevant |
| `options` | kind-specific (source URL, build system, variables to set, files to add) |
| `write` | default false: dry run returning the paths and full content |

`where` writes nothing: given a recipe and a variable or intent, it recommends where the
change belongs. See M6.

### `bitbake`
| Arg | Notes |
|---|---|
| `op` | `build` \| `task` \| `env` \| `clean` \| `devtool` \| `layers` \| `status` \| `cancel` |
| `target` | recipe or image |
| `task` | for `op: task`, e.g. `compile` |
| `args` | op-specific, validated against an allowlist |
| `confirm` | required true for destructive operations |

Starts a background job and returns `job:<id>` immediately. `op: status` returns the state
and, when finished, a digest plus a handle. One build job at a time.

### `log`
| Arg | Notes |
|---|---|
| `recipe` | optional; default: the most recent failure |
| `task` | optional |
| `job` | optional job id |

Digest: failing recipe and task, exit status, the error lines with a few lines of context,
the function in `run.do_<task>` that failed if identifiable, a pointer to the task's
definition in the layers, and a handle to the full log.

### `slice`
| Arg | Notes |
|---|---|
| `handle` | `h:<id>` required |
| one of | `lines: "A-B"`, `grep: "RE"` (+ `context`), `var: "NAME"` (env dumps), `failures: true`, `page: K` |

Capped at 8 KB; says what it cut.

### `status`
No arguments. Workspace roots, layer count, file counts by kind, index path and age, whether
a build directory and a usable bitbake environment were found, running job if any.

## MCP resources

- `sous-chef://help`: this document, condensed.
- `sous-chef://rules`: the lint rule catalogue.

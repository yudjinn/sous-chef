# MCP tools

Common to every tool:

- `budget_tokens` (default 600) and `format` (`"text"` default, or `"json"`).
- Output is a tab-separated table through `render.rs`; see the output rules in
  [architecture.md](architecture.md). Notes follow the table.
- Each call that reads the index starts with `Index::refresh` (`bitbake` does not read it;
  `log` refreshes only to point at the task definition). Freshness is reported only if
  something changed.
- Recipe arguments accept `pn`, `pn_pv` or a pointer. A name that is no recipe's exact `pn`
  is an error listing up to 20 recipe names that contain it.
- `machine` and `distro` default to the values in `local.conf` when a build directory exists.

The first six are always loaded. The rest are deferred (short descriptions, loaded on demand).

## Always loaded

### `recipe`
Resolve a recipe and show its shape.

| Arg | Type | Notes |
|---|---|---|
| `name` | string | required: a `pn`, a `pn_pv`, or a pointer to a `.bb` or a `.bbappend` (a bbappend pointer means the recipe it applies to) |
| `version` | string? | default: the version preferred by layer priority and version; `%` matches any rest |
| `show` | `"summary"` \| `"tree"` \| `"outline"` | default `summary` |

An unknown version is an error listing the versions that exist. A `pn_pv` together with a
different `version` is an error. A bbappend that applies to several recipes is an error
listing them.

- `summary`: rows `field value loc`: the recipe file, its layer and priority, every version
  (the chosen one marked "preferred by layer priority and version" or "selected by
  version=V"), each bbappend with its layer and priority in application order, the classes
  inherited (`(deferred)`, `(unresolved)` marked), and key values. `SUMMARY` and `LICENSE`
  come from unconditional direct statements of the recipe tree (the last `=` / `:=`, else the
  first `?=`, else the last `??=`); `SRC_URI` and `DEPENDS` are word counts over the same
  statements, with `+=` and the like, `:append`, `:prepend` and `:remove` applied.
  Statements with an override condition are reported as not counted (not applied). A note
  says the values are not evaluated and that `base.bbclass` and the `INHERIT` classes are
  not included.
- `tree`: the files of the recipe in parse order, statically: the `.bb` and what it pulls in
  where it pulls it in, then each bbappend in application order the same way, then deferred
  inherits. Rows `depth kind name file via state`; `state` is `-`, `unresolved` (missing, or
  the argument needs evaluation), `cycle` or `already inherited`. `base.bbclass` and the
  `INHERIT` classes are not shown; a note says so.
- `outline`: every function and task statement across that tree: `name kind file_kind loc`.
  `kind` is `shell`, `python`, `fakeroot shell`, `def`, `addtask`, `deltask` or
  `addhandler`; anonymous python is named `__anonymous`.

```
field	value	loc
recipe	hello 1.1	meta-core:recipes-core/hello/hello_1.1.bb
layer	meta-core (priority 5)	-
version	1.1 (preferred by layer priority and version)	meta-core:recipes-core/hello/hello_1.1.bb
version	1.0	meta-core:recipes-core/hello/hello_1.0.bb
bbappend	meta-vendor (priority 6)	meta-vendor:recipes-core/hello/hello_%.bbappend
bbappend	meta-mine (priority 10)	meta-mine:recipes-core/hello/hello_1.1.bbappend
inherit	autotoolsish	meta-core:recipes-core/hello/hello.inc:11
SUMMARY	GNU Hello stand-in	meta-core:recipes-core/hello/hello.inc:1
LICENSE	MIT	meta-core:recipes-core/hello/hello.inc:5
SRC_URI	4 entries in 5 statements; 1 conditional statement not counted	-
DEPENDS	3 entries in 2 statements	-
note: key values come from direct statements only, not evaluated (no overrides, expansion or python; base.bbclass and INHERIT classes not included); use var for the full history
```

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
| `recipe` + `name` | string? | e.g. `recipe: "busybox", name: "do_install:append"`; searches the recipe tree, bbappends included. There is no `version` argument: give a `pn_pv` |
| `file` + `name` | string? | a single file, `layer:path` |
| `context` | int | extra lines around, default 0, at most 200 |

Exactly one of `loc`, `recipe` + `name`, `file` + `name`. A name matches a statement with the
same name, or the same name followed by more overrides: `do_install` matches `do_install`
and `do_install:append`. `VAR[flag]` matches only that flag; a name without a flag does not
match flag statements. When several statements match, the result is a table
`loc kind name snippet` with a note, and no text:

```
loc	kind	name	snippet
meta-core:recipes-core/hello/hello.inc:15-18	function	do_install:append	do_install:append() {
meta-mine:recipes-core/hello/hello_1.1.bbappend:8-10	function	do_install:append	do_install:append() {
2 statements match do_install:append; call item with loc=<one of the pointers>
```

Otherwise it returns a header line `layer:path:A-B sha256=<12 hex>` then the exact lines,
unmodified (line endings included), so they can be used as Edit strings. The header range
is the range actually returned, context included; the hash is of the file as read. Capped
at 8 KB (or `budget_tokens * 4` when that is smaller) with a `+N more lines (lines=A-B)`
marker. JSON: `{loc, sha256, text, more_lines, rest, notes}`.

```
meta-mine:recipes-app/app/app_0.1.bb:25-29 sha256=8c263137e2cb
fakeroot do_install() {
	install -d ${D}${bindir}
	install -m 0755 app ${D}${bindir}/app
	chown root:root ${D}${bindir}/app
}
```

### `search`
| Arg | Type | Notes |
|---|---|---|
| `query` | string | required |
| `mode` | `"name"` \| `"regex"` \| `"fts"` | default `name`: statement names containing `query`, case-insensitive, exact names first. `fts`: SQLite FTS5 over names and snippets |
| `kind` | list? | `assign`, `function`, `pydef`, `inherit`, `include`, `addtask`, `deltask`, `addhandler`, `export_functions`, `export`, `unset`, `unknown`; aliases `task` (`addtask`) and `def` (`pydef`) |
| `layer`, `file_kind`, `recipe` | filters | `recipe` restricts to that recipe's tree (the `.bb`, its includes and classes, its bbappends) |
| `limit` | int | default 30, at most 1000 |

Each hit is a row `loc kind item snippet`; `item` is `[name A-B]`, the enclosing item and
its range. `regex` mode reads file content line by line, through the confinement check;
`kind` is the enclosing statement's kind (`-` outside any statement), and the `kind` filter
applies to the enclosing statement, so lines outside any statement are dropped. Files that
cannot be read are skipped and counted in a note. The other modes use the index. `limit`
cuts with a `+N more (limit=L)` marker; in `name` and `fts` mode it counts up to 5000 index
hits, beyond which `N` is a lower bound.

```
loc	kind	item	snippet
meta-core:recipes-core/hello/hello.inc:16	function	[do_install:append 15-18]	install -d ${D}${sysconfdir}
meta-core:recipes-core/hello/hello.inc:17	function	[do_install:append 15-18]	echo "greeting=hello" > ${D}${sysconfdir}/hello.conf
meta-core:recipes-core/hello/hello.inc:20	assign	[FILES:${PN} 20]	FILES:${PN} += "${sysconfdir}/hello.conf"
```

### `class`
| Arg | Type | Notes |
|---|---|---|
| `name` | string | required; without directory or `.bbclass` |
| `show` | `"summary"` \| `"users"` \| `"outline"` | default `summary` |

The class is looked up as a recipe `inherit` finds it (`classes-recipe/` along `BBPATH`,
then `classes/`). A class found only in `classes-global/` is ranked the way `INHERIT` finds
it (`classes-global/`, then `classes/`), with a note. A name that is no class's exact name
is an error listing up to 20 class names that contain it.

- `summary`: one `file` row per indexed `<name>.bbclass`, the one BitBake uses first
  (`wins`, then `shadowed`, or `not on the ... search path`); then, for the winning file,
  the classes it inherits, tasks (`addtask`, `deltask`, `addhandler`), `EXPORT_FUNCTIONS`,
  and its interface: variables set with `?=` or `??=`, without override or flag.
- `users`: rows `depth kind file via`: files whose `INHERIT` assignment names the class
  (kind `INHERIT`), then files that inherit it directly (`depth` 1) and transitively
  (through an include, another class or a bbappend).
- `outline`: functions and tasks of the winning file, with pointers.

```
field	value	loc
file	wins (INHERIT), meta-core priority 5	meta-core:classes-global/sanityish.bbclass
interface	SANITY_REQUIRED_UTILITIES ?= "patch diffstat git"	meta-core:classes-global/sanityish.bbclass:3
handler	addhandler sanityish_check	meta-core:classes-global/sanityish.bbclass:5
note: global class: ranked as INHERIT finds it (classes-global/, then classes/)
```

## Deferred

### `layers`
`show`:

- `"list"` (default): one row per indexed layer, the build directory pseudo-layer included:
  name, priority, collections, compat, depends, writable, file counts by kind. A `release:`
  note gives the workspace release.
- `"shadowed"`: one row per file of every recipe or class file name that more than one layer
  provides (`kind name status layer priority file`), the winner first.
- `"appends"`: one row per bbappend and recipe it applies to (`bbappend priority matches
  status`), status `applies`; a bbappend that matches nothing is one row with `matches` `-`
  and status `no-match`, and a note.

```
bbappend	priority	matches	status
meta-vendor:recipes-core/hello/hello_%.bbappend	6	meta-core:recipes-core/hello/hello_1.0.bb	applies
meta-vendor:recipes-core/hello/hello_%.bbappend	6	meta-core:recipes-core/hello/hello_1.1.bb	applies
meta-mine:recipes-app/orphan/orphan_9.%.bbappend	10	-	no-match
meta-mine:recipes-core/hello/hello_1.1.bbappend	10	meta-core:recipes-core/hello/hello_1.1.bb	applies
note: 1 bbappend matches no recipe; BitBake stops with a fatal error on these (cooker.py show_appends_with_no_recipes)
```

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
| `target` | a recipe (`pn` or `pn_pv`), a file pointer `layer:path[:A-B]`, a layer name (`build` for the build directory), or a path relative to the working directory; default: the writable layers (every layer if none is writable) and the build directory's configuration files |
| `rules` | optional list of ids or groups to run; an unknown one is an error |
| `severity` | minimum: `info` \| `warning` \| `error` |

The tool has no `all` argument (the CLI's `--all`). A pointer with a line range keeps only
the findings in that range. Output: a summary line first (counts by severity and by rule,
so a table cut by the budget still gives the totals), then the table
`severity rule pointer message`, then the notes. JSON:
`{summary, errors, warnings, infos, by_rule, findings}`, where `findings` is the table.
See [lint-rules.md](lint-rules.md) for the engine.

```
4 findings in 6 files: 3 error, 1 warning, 0 info; by rule: hygiene/parse-error 4
severity	rule	pointer	message
error	hygiene/parse-error	meta-mine:conf/distro/mine.conf:1	`inherit` in a .conf file: BitBake's configuration parser rejects it as an unparsed line; move it to a class or recipe (use INHERIT += for a global class)
error	hygiene/parse-error	meta-mine:recipes-app/bad/bad_1.0.bb:3	unparsed line; fix the line so BitBake can parse it, or remove it
warning	hygiene/parse-error	meta-mine:recipes-app/bad/bad_1.0.bb:4	addtask: `before`/`after` groups split by the other keyword are concatenated by BitBake without a space, joining two task names; write all `after` tasks in one group and all `before` tasks in another
error	hygiene/parse-error	meta-mine:recipes-app/bad/bad_1.0.bb:5-6	function do_broken is never closed: no `}` in column 0 before end of file; close the function with `}` in column 0
```

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
| `task` | for `op: task`, e.g. `compile` (a `do_` prefix is dropped) |
| `args` | op-specific, validated against an allowlist (below) |
| `confirm` | required true for destructive operations |
| `job` | for `status` and `cancel`: `job:<n>` or `<n>`; default: the running job, else the last one |

| `op` | Runs | `args` | Handle kind |
|---|---|---|---|
| `build` | `bitbake [-k] <target>` | `continue` (`-k`) | `build` |
| `task` | `bitbake -c <task> [-f] <target>` | `force` (`-f`) | `build` |
| `env` | `bitbake -e [<target>]` | none | `env` |
| `clean` | `bitbake -c <which> <target>` | one of `clean` (default), `cleansstate`, `cleanall` | `build` |
| `devtool` | `devtool <subcommand> ...` | the subcommand first: `modify`, `status`, `finish`, `reset`, `build` | `build` |
| `layers` | `bitbake-layers <subcommand> [<target>]` | one of `show-layers` (default), `show-appends`, `show-recipes`, `show-overlayed`; `target` only with `show-recipes` and `show-appends` | `text` |

`devtool status` takes no target; `modify`, `reset` and `build` take a target and nothing
else; `finish` takes a target and, as the second `args` entry, a writable layer of the
workspace. Known gaps: `devtool modify` never passes a source tree; `devtool reset` requires
a target; `devtool finish` accepts only a writable layer name.

Destructive operations (`cleansstate`, `cleanall`, `devtool reset`, `devtool finish`) need
`confirm: true` and `allow_destructive = true` under `[bitbake]` in `sous-chef.toml`; the
error names the setting. Every user-supplied name (target, task, layer) is validated before
anything runs: letters, digits and `_ . + - /` only, not starting with `-`, so `;`, `$(...)`,
backticks, spaces and `:` are rejected.

Every operation except `status` and `cancel` starts a background job and returns at once
with `job:<n>`. One job of any kind runs at a time: a second request of any operation starts
nothing and returns the running job with "already running". There are no MCP progress
notifications; poll with `op: status`.

`status` reports `job`, `op`, `target`, `command`, `state` (`running`, `succeeded`,
`failed`, `cancelled`), `elapsed`, `exit` and `handle`. While running it shows the last 5
output lines; a finished job that did not fail shows the last 3 and a `slice` hint; a
failed job is followed by a digest of the console output and, for operations that run
tasks, a `log job=` hint. JSON: `{job, tail, digest, notes}`. `cancel` terminates the job's
process group (SIGTERM, then SIGKILL after 2 s) and reports the job. A finished job's output
becomes a handle the next time the `bitbake` tool, or `log` with `job`, is called.

### `log`
| Arg | Notes |
|---|---|
| `recipe` | optional; default: the most recent failure |
| `task` | optional |
| `job` | optional job id from `bitbake` |

Digest: failing recipe and task, exit status, the error lines with a few lines of context,
the function in `run.do_<task>` that failed if identifiable, a pointer to the task's
definition in the layers, and a handle to the full log.

With `job`: the log named in the job's output (the first failed task's
`Logfile of failure stored in:`, then any other log it names), filtered by `recipe` and
`task`. If none is readable it digests the job's console output, with a note. A running job
is an error.

### `slice`
| Arg | Notes |
|---|---|
| `handle` | `h:<id>` required |
| one of | `lines: "A-B"`, `grep: "RE"` (+ `context`), `var: "NAME"` (env dumps), `failures: true`, `page: K` |

Capped at 8 KB; says what it cut.

### `status`
No arguments. Workspace roots, the release (`release`, e.g. `wrynose (LAYERSERIES_CORENAMES)`),
layer count, file counts by kind, index path and age, whether a build directory and a usable
bitbake environment were found, and the running job if any (`job`, e.g.
`job:1 task hello running 1s`).

## MCP resources

- `sous-chef://help`: this document, condensed.
- `sous-chef://rules`: the lint rule catalogue, rows `id severity scope flags`. `severity`
  is the effective one (`[lint.severity]` applied); `flags` is the description from
  lint-rules.md, prefixed with `[releases A to B]` for a rule limited to some releases and
  `[off by default]` for a rule that runs only when selected. A last line lists
  `[lint] disable`, if set.

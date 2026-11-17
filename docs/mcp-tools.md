# MCP tools

Common to every tool:

- `budget_tokens` (default 600) and `format` (`"text"` default, or `"json"`).
- Output is a tab-separated table through `render.rs`; see the output rules in
  [architecture.md](architecture.md). Notes follow the table.
- Each call that reads the index starts with `Index::refresh` (`bitbake` does not read it;
  `log` refreshes only to point at the task definition). Freshness is reported only if
  something changed.
- Recipe arguments accept `pn`, `pn_pv` or a pointer. A name that is no recipe's exact `pn`
  is an error listing up to 20 recipe names that contain it. In `recipe`, `var`,
  `overrides` and `deps` a bare `pn` names the file BitBake would use, through provider
  selection (`PREFERRED_VERSION`, `DEFAULT_PREFERENCE`, layer priority, version; see
  "Providers and versions" in bitbake-semantics.md); `item` and `search` pick by layer
  priority and version only.
- `machine` and `distro`, where a tool takes them, default to the values in `local.conf`
  when a build directory exists.

The first six are always loaded. The rest are deferred (short descriptions, loaded on demand).

## Always loaded

### `recipe`
Resolve a recipe and show its shape.

| Arg | Type | Notes |
|---|---|---|
| `name` | string | required: a `pn`, a `pn_pv`, or a pointer to a `.bb` or a `.bbappend` (a bbappend pointer means the recipe it applies to) |
| `version` | string? | default: the version BitBake would use (provider selection for the default machine and distro); `%` matches any rest |
| `show` | `"summary"` \| `"tree"` \| `"outline"` | default `summary` |

An unknown version is an error listing the versions that exist. A `pn_pv` together with a
different `version` is an error. A bbappend that applies to several recipes is an error
listing them.

- `summary`: rows `field value loc`: the recipe file, its layer and priority, every version,
  each bbappend with its layer and priority in application order, the classes inherited
  (static walk; `(deferred)`, `(unresolved)` marked), and key values. The chosen version
  says why: `selected by version=V`, the setting that chose it
  (`1.0 (PREFERRED_VERSION_hello = "1.0")`), `preferred by layer priority,
  DEFAULT_PREFERENCE N and version`, or `preferred by layer priority and version`. The key
  values are evaluated (`eval::evaluate` of the chosen file for the default machine and
  distro: configuration, classes, recipe and bbappends): `SUMMARY` and `LICENSE` with the
  last applied operation as `loc` (`(not set)`, `(empty)`, or `(depends on python, not
  evaluated)` when so); `SRC_URI` and `DEPENDS` as the number of words of the final value
  and the number of applied and inactive operations. A note names the machine and distro;
  another warns when the recipe has anonymous python, which is not run.
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
SRC_URI	4 entries from 4 applied operations; 1 inactive operation (override not active)	-
DEPENDS	3 entries from 2 applied operations	-
note: key values are evaluated for MACHINE=qemux86 DISTRO=testdistro (static replay of the configuration, classes, recipe and bbappends; python not run); use var for the full history
```

### `var`
History and final value of a variable. The main tool.

| Arg | Type | Notes |
|---|---|---|
| `name` | string | base name, e.g. `SRC_URI`; `FILES:${PN}` allowed (expanded for the recipe first); `VAR[flag]` shows that flag only |
| `recipe` | string? | omit for global configuration |
| `machine`, `distro` | string? | |
| `authoritative` | bool | default false; true runs `bitbake -e` (M5) |
| `flags` | bool | also show `VAR[flag]` operations, per flag, after the value's |

Static mode replays the configuration, and the recipe when given (`eval::evaluate`).
Output: one row per operation on the variable and its override variants, in parse order:
`status op override loc snippet`.

- `status`: `applied`, `inactive` (an override condition is not in `OVERRIDES`),
  `shadowed` (overwritten, lost to another variant, or a default that did not take effect)
  or `removed` (every word it added was removed again).
- `op`: the operator; `unset` statements are rows with op `unset` (status `applied`).
- `override`: the overrides as written (`append:boardx`), `-` for none; a flag operation
  shows `[flag]` (`append[flag]` with overrides).
- A function is a variable too: each definition is a row with snippet `name() { ... }`, and
  `final:` lists its pieces in order as pointers (`layer:path:A-B`, with `-> class_do_x` for
  an `EXPORT_FUNCTIONS` stub).

Then the notes: `final:` with the value in quotes (`final[flag]:` for flags; `final: X is
not set`), cut at 600 characters with the full text in a `value` handle; references left
unexpanded; a warning per anonymous python function that changes the variable by name, and
one for each that is opaque (changes variables through computed names or hands `d` to other
code), since anonymous python is never run; a count of the other anonymous functions;
`note: global configuration only (no recipe)` without a recipe; the python note when the
value depends on python. `authoritative: true` returns the error `bitbake is not
available: no initialised build environment was found` until T5.5 implements it.

`EXTRA_OECONF` of `hello` with `machine: boardx`:

```
status	op	override	loc	snippet
shadowed	?=	-	meta-core:conf/bitbake.conf:42	EXTRA_OECONF ?= ""
applied	=	append:pn-hello	build:conf/local.conf:9	EXTRA_OECONF:append:pn-hello = " --enable-local-debug"
shadowed	?=	-	meta-core:classes-recipe/autotoolsish.bbclass:7	EXTRA_OECONF ?= ""
applied	=	-	meta-core:recipes-core/hello/hello.inc:13	EXTRA_OECONF = "--disable-nls"
applied	=	append:boardx	meta-vendor:recipes-core/hello/hello_%.bbappend:7	EXTRA_OECONF:append:boardx = " --enable-boardx"
inactive	=	append:boradx	meta-vendor:recipes-core/hello/hello_%.bbappend:9	EXTRA_OECONF:append:boradx = " --enable-typo"
applied	=	append	meta-mine:recipes-core/hello/hello_1.1.bbappend:6	EXTRA_OECONF:append = " --with-mine"
final: "--disable-nls --enable-local-debug --enable-boardx --with-mine"
```

With python (recipe `app`, variable `EXTRA_OEMAKE`):

```
status	op	override	loc	snippet
shadowed	?=	-	meta-core:conf/bitbake.conf:43	EXTRA_OEMAKE ?= ""
applied	=	-	meta-mine:recipes-app/app/app_0.1.bb:9	EXTRA_OEMAKE = "GREETING=${APP_GREETING}"
final: "GREETING=${@'hi' if d.getVar('MACHINE') == 'boardx' else 'hello'}"
warning: anonymous python at meta-mine:recipes-app/app/app_0.1.bb:13-16 changes EXTRA_OEMAKE; it is not run, so the value shown is the one before it
note: 1 value depends on python and was not evaluated; use authoritative=true
```

A function (`do_install` of `hello`):

```
status	op	override	loc	snippet
shadowed	=	-	meta-core:classes/base.bbclass:54	do_install() { ... }
applied	=	-	meta-core:classes-recipe/autotoolsish.bbclass:30	do_install() { ... }
applied	=	append	meta-core:recipes-core/hello/hello.inc:15-18	do_install:append() { ... }
applied	=	append	meta-mine:recipes-core/hello/hello_1.1.bbappend:8-10	do_install:append() { ... }
final: function do_install, 3 pieces in order: meta-core:classes-recipe/autotoolsish.bbclass:30 -> autotoolsish_do_install, meta-core:recipes-core/hello/hello.inc:15-18, meta-mine:recipes-core/hello/hello_1.1.bbappend:8-10
```

### `overrides`
| Arg | Type | Notes |
|---|---|---|
| `recipe` | string? | |
| `machine`, `distro` | string? | |
| `show` | `"chain"` \| `"conditional"` \| `"dead"` | default `chain` |

- `chain`: rows `priority entry from loc`: the entries of the evaluated `OVERRIDES`,
  lowest priority first, each with the variables it came through
  (`OVERRIDES <- MACHINEOVERRIDES <- MACHINE`) and the deepest assignment that contributes
  it. Entries are attributed by expanding the applied assignments piece by piece; a piece
  that is python is unknown and said so.
- `conditional`: rows `state condition name loc snippet`: every operation whose name
  carries an override condition, in parse order, `active` or `inactive` for this machine
  and distro. With a recipe: the operations from the recipe's own files (recipe, includes,
  classes, bbappends) and `.conf` operations conditional on `pn-<pn>`; without, the
  configuration's operations. Package-name overrides (`FILES:${PN}-dev`,
  `RDEPENDS:libfoo-dev`) are not conditions and are left out.
- `dead`: rows `override closest name loc snippet`: the conditional operations with an
  override element known nowhere: not in `OVERRIDES` for any machine or distro conf of the
  workspace, not a machine, distro or declared override, not a class, `pn-` or `layer-`
  form, not a package of the recipe. `closest` is the nearest known name by edit distance.
  It evaluates the configuration once per machine conf (with the default distro) and once
  per distro conf (with the default machine); the passes are cached.

A warning follows when `OVERRIDES` contains python or unresolved references (active and
inactive may then be wrong), and one for each opaque anonymous python function. `chain` of
`hello` with `machine: boardx`:

```
priority	entry	from	loc
0	linux	OVERRIDES <- TARGET_OS	meta-core:conf/bitbake.conf:32
1	arm	OVERRIDES <- TRANSLATED_TARGET_ARCH <- TARGET_ARCH	meta-core:conf/machine/boardx.conf:9
2	pn-hello	OVERRIDES	meta-core:conf/bitbake.conf:39
3	layer-core	OVERRIDES	meta-core:conf/bitbake.conf:39
4	socx	OVERRIDES <- MACHINEOVERRIDES	meta-core:conf/machine/boardx.conf:7
5	boardx	OVERRIDES <- MACHINEOVERRIDES <- MACHINE (machine argument)	meta-core:conf/bitbake.conf:35
6	testdistro	OVERRIDES <- DISTROOVERRIDES <- DISTRO	meta-core:conf/distro/testdistro.conf:1
7	class-target	OVERRIDES <- CLASSOVERRIDE	meta-core:conf/bitbake.conf:37
8	libc-glibc	OVERRIDES <- LIBCOVERRIDE	meta-core:conf/bitbake.conf:38
9	forcevariable	OVERRIDES	meta-core:conf/bitbake.conf:39
note: entries to the right (higher priority) win; loc is the deepest assignment that contributes the entry
```

`conditional` and `dead` of `hello` (default machine `qemux86`):

```
state	condition	name	loc	snippet
active	pn-hello	EXTRA_OECONF:append:pn-hello	build:conf/local.conf:9	EXTRA_OECONF:append:pn-hello = " --enable-local-debug"
inactive	boardx	SRC_URI:append:boardx	meta-vendor:recipes-core/hello/hello_%.bbappend:5	SRC_URI:append:boardx = " file://boardx.cfg"
inactive	boardx	EXTRA_OECONF:append:boardx	meta-vendor:recipes-core/hello/hello_%.bbappend:7	EXTRA_OECONF:append:boardx = " --enable-boardx"
inactive	boradx	EXTRA_OECONF:append:boradx	meta-vendor:recipes-core/hello/hello_%.bbappend:9	EXTRA_OECONF:append:boradx = " --enable-typo"
note: 1 active, 3 inactive; scope: the recipe's files, and .conf statements conditional on pn-<recipe>
```

```
override	closest	name	loc	snippet
boradx	boardx (distance 2)	EXTRA_OECONF:append:boradx	meta-vendor:recipes-core/hello/hello_%.bbappend:9	EXTRA_OECONF:append:boradx = " --enable-typo"
note: known overrides: OVERRIDES of 3 machine/distro configurations, machine and distro names, declared MACHINEOVERRIDES/DISTROOVERRIDES words, class-/pn-/layer- forms, and the recipe's packages
```

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
| `recipe` | required; for `providers`, a provided name such as `virtual/kernel` |
| `kind` | `"build"` (default), `"runtime"`, `"packageconfig"`, `"reverse"`, `"providers"` |
| `depth` | default 1, at most 5; used by `build` and `reverse` only (a note says so otherwise) |

There is no `machine` or `distro` argument: the recipe is evaluated for the values in
`local.conf`. `PACKAGECONFIG` additions to `DEPENDS` and `RDEPENDS:${PN}` are made by
OE-core's `base.bbclass` with anonymous python, which is never run, so they are listed from
the flags of the selected options, with a note.

- `build`: rows `depth dep provider source loc`: the words of the evaluated `DEPENDS`, each
  with the assignment that adds it (a class's `DEPENDS:prepend` points into the class) and
  the recipe that provides it (provider selection; `-` when no recipe of the workspace
  does: `BBCLASSEXTEND` variants such as `-native` are not resolved), then the build
  dependencies of the selected `PACKAGECONFIG` options. `depth` follows the selected
  providers.
- `runtime`: rows `package dep source loc`: per package of `PACKAGES`, the words of
  `RDEPENDS:<package>`, then the runtime dependencies of the selected `PACKAGECONFIG`
  options (on `${PN}`).
- `packageconfig`: rows `option selected enable disable build runtime loc`, one per
  `PACKAGECONFIG[option]` flag (fields 1 to 4), and a note with the value of
  `PACKAGECONFIG` and where it is set. Fields 5 and 6 are not shown.
- `reverse`: rows `depth recipe kind target loc status`: recipes whose `DEPENDS`,
  `RDEPENDS:<pkg>` or `PACKAGECONFIG` build field names the recipe (`by name`) or
  something it provides (`via <name>`, with the selected provider of that name). Static:
  read from the index, unconditional statements as written; names with `${...}` and
  conditional statements are not seen.
- `providers`: rows `name version status loc`: every recipe that provides the name, the
  selected one first, with the reason for its version and for the choice; then the
  `PREFERRED_PROVIDER` / `PREFERRED_VERSION` assignments that decided it. Candidates are
  recipes whose `PN` or static `PROVIDES` names it, confirmed by evaluating each.

```
depth	dep	provider	source	loc
1	libfoo	libfoo 2.0	DEPENDS	meta-mine:recipes-app/app/app_0.1.bb:4
2	autoconf-native	-	DEPENDS	meta-core:classes-recipe/autotoolsish.bbclass:11
2	automake-native	-	DEPENDS	meta-core:classes-recipe/autotoolsish.bbclass:11
2	zlib	-	PACKAGECONFIG[zlib]	meta-core:recipes-core/libfoo/libfoo_2.0.bb:15
note: PACKAGECONFIG rows: OE-Core's base.bbclass adds the build (field 3) and runtime (field 4) dependencies of the selected options with anonymous python, which is not run; they are listed from the flags
note: provider - means no recipe in the workspace provides the name (BBCLASSEXTEND variants such as -native are not resolved)
note: 1 anonymous python function was not run (none changes DEPENDS by name)
```

```
option	selected	enable	disable	build	runtime	loc
ssl	no	--with-ssl	--without-ssl	openssl	-	meta-core:recipes-core/libfoo/libfoo_2.0.bb:16
zlib	yes	--with-zlib	--without-zlib	zlib	zlib	meta-core:recipes-core/libfoo/libfoo_2.0.bb:15
note: PACKAGECONFIG = "zlib" (meta-core:recipes-core/libfoo/libfoo_2.0.bb:14)
```

```
depth	recipe	kind	target	loc	status
1	app 0.1	DEPENDS	libfoo	meta-mine:recipes-app/app/app_0.1.bb:4	by name
1	app 0.1	RDEPENDS:${PN}	libfoo	meta-mine:recipes-app/app/app_0.1.bb:11	by name
1	hello 1.0	DEPENDS	virtual/foo	meta-core:recipes-core/hello/hello.inc:7	via virtual/foo (selected provider is libfoo-alt)
1	hello 1.1	DEPENDS	virtual/foo	meta-core:recipes-core/hello/hello.inc:7	via virtual/foo (selected provider is libfoo-alt)
note: static: unconditional DEPENDS / RDEPENDS:<package> statements and PACKAGECONFIG build fields as written in the index (names with ${...} and conditional statements are not seen)
```

```
name	version	status	loc
libfoo-alt	1.0 (PREFERRED_VERSION_libfoo-alt = "1.%")	selected by PREFERRED_PROVIDER_virtual/foo	meta-vendor:recipes-vendor/libfoo-alt/libfoo-alt_1.0.bb:6
libfoo	2.0 (preferred by layer priority and version)	not selected	meta-core:recipes-core/libfoo/libfoo_2.0.bb:6
PREFERRED_PROVIDER_virtual/foo	libfoo-alt	applied	meta-core:conf/distro/testdistro.conf:8
PREFERRED_VERSION_libfoo-alt	1.%	applied	meta-core:conf/distro/testdistro.conf:9
note: candidates are recipes whose PN or static PROVIDES names it (index), confirmed by evaluating each; a PROVIDES that needs expansion to name it is not found
```

The samples are `build` of `app` with `depth: 2`, `packageconfig` and `reverse` of
`libfoo`, and `providers` of `virtual/foo`.

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
| `name`, `version` | as relevant to `kind` (below); not options |
| `options` | an object of kind-specific options (below); any other key is refused, listing the valid ones |
| `write` | default false: dry run returning the paths and full content |

`inc_split` and `where` are not implemented yet (T6.4, T6.5, wave 5): they return an error
and write nothing. `where` will write nothing: given a recipe and a variable or intent, it
will recommend where the change belongs. See M6.

Before a generator plans, the index is refreshed. The plan is checked in both modes:
absolute paths that pass the confinement check and lie in a writable layer, `create`
targets that do not exist, `modify` targets unchanged since they were read. Dry run (text):
a table `action path lines`, a `dry run: nothing written` note, the plan's notes, then each
file's content after a `--- <action> <path>` line; when the whole output passes 8 KB the
contents go to a `text` handle and a note names it. Write: the same table with a `result`
column, then the notes, the index refresh, `lint: N findings on the N written files` and,
if there are any, a table `severity rule loc message`. The post-write lint runs the
default rule set on the written BitBake files only (not patches or other files a bbappend
adds); `[lint] disable`, `[lint.severity]` and suppression comments apply. JSON:
`{kind, written, files, contents, content_handle, lint}`.

Generated files follow the releases the target layer is checked against (see "Releases" in
lint-rules.md) and are meant to lint clean for them.

`recipe`: `name` is `PN`, `version` is `PV` (no `_`, `/`, quotes or spaces); both required.
Writes `recipes-<category>/<pn>/<pn>_<pv>.bb`.

| Option | Notes |
|---|---|
| `category` | `app` or `recipes-app`; default: the directory of a version of the recipe in this layer, else its category in another layer, else the layer's only `recipes-*` directory (several is an error listing them) |
| `source` | one remote URL (git or tarball) plus any local files (`file://x` or a bare name); more than one remote is an error |
| `branch` | git branch; required for a git source unless the URL has `branch=` or `nobranch=1` |
| `srcrev` | git commit id |
| `sha256sum` | tarball checksum, 64 hex digits |
| `build` | `autotools`, `cmake`, `meson`, `cargo`, `setuptools`, `setuptools_build_meta`, `flit`, `poetry`, `hatchling`, `go`, `kernel-module` or `none` (default) |
| `license` | required: an SPDX expression or `CLOSED` |
| `lic_files_chksum` | the `LIC_FILES_CHKSUM` value |
| `depends`, `rdepends` | words for `DEPENDS` and `RDEPENDS:${PN}` |
| `summary`, `homepage` | `SUMMARY` (default: the recipe name, with a note) and `HOMEPAGE` |
| `go_import` | `GO_IMPORT` for `build: go`; default: taken from the git URL |

The file is in style-guide order: description block, license, dependencies, `SRC_URI` with
its revision or checksum, `S`, `inherit`, packaging. A fact that cannot be defaulted
(`LIC_FILES_CHKSUM` unless `CLOSED`, `SRCREV`, `SRC_URI[sha256sum]`) is left out, marked
with a `# sous-chef: supply ...` comment and listed in the notes. Sources, per release:

- git: `git://` with `protocol=` for another scheme, `branch=`, and `destsuffix=${BP}` with
  no `S` line, which works from Kirkstone to Wrynose.
- tarball: `${BP}` or `${PV}` put into the URL; the default `S`, with a note that it
  assumes the tarball unpacks into `<pn>-<pv>/`.
- local files only: `S = "${UNPACKDIR}"` when every release of the layer has `UNPACKDIR`
  (Styhead and later), `S = "${WORKDIR}"` when every one accepts it (up to Scarthgap),
  otherwise `subdir=${BP}` on each entry and the default `S`, with a note. A note says
  where to put the files.
- go: `GO_IMPORT` and, from Styhead, `destsuffix=${GO_SRCURI_DESTSUFFIX}`; when the layer
  claims releases on both sides of Styhead, no `SRC_URI` serves both, so the workspace
  release decides and a note names the releases it will not work on.

`build` maps to the OE-core class (`setuptools` is `setuptools3`, `flit`
`python_flit_core`, `poetry` `python_poetry_core`, `hatchling` `python_hatchling`,
`kernel-module` `module`); a note says when the class is not in the indexed layers.
`kernel-module` adds `RPROVIDES:${PN} += "kernel-module-<pn>"`; `cargo`, `hatchling` (not
in Kirkstone) and `kernel-module` add a note. When another version of the recipe requires
or includes an `.inc`, the new file requires it and sets only version data
(`LIC_FILES_CHKSUM`, `DEPENDS +=`, `SRCREV` or the checksum, `RDEPENDS`); options the
`.inc` covers are ignored, with a note.

```
--- create meta-mine:recipes-app/tool/tool_0.1.bb
SUMMARY = "Local tool"
LICENSE = "CLOSED"

SRC_URI = "file://tool.c;subdir=${BP} \
           file://Makefile;subdir=${BP} \
           "
note: the layer claims releases with UNPACKDIR (wrynose) and without it (kirkstone scarthgap): the local files unpack into ${BP} (subdir=${BP}), which is the default S on all of them
note: put the local files (tool.c, Makefile) in meta-mine:recipes-app/tool/files/ (or in tool/ or tool-0.1/ beside it)
note: build `none`: no class is inherited; add do_compile / do_install as needed
```

(The planned content and notes as `tests/snapshots/scaffold_recipe__recipe_local_mixed.snap`
records them; the tool prints the table and notes before the content.)

`bbappend`: the recipe is the `recipe` option or `name`; the tool's `version` picks one of
its versions (`%` allowed), else the one preferred by layer priority and version. The file
goes in `recipes-<category>/<pn>/` like the recipe's. When the layer already has a bbappend
with the chosen name, or (without the `version` option) any bbappend that applies to the
recipe, it is modified instead, and entries it already has are not repeated.

| Option | Notes |
|---|---|
| `recipe` | the recipe to append to (default: `name`) |
| `version` | `exact` (`<pn>_<pv>.bbappend`), `wildcard` (`<pn>_%.bbappend`) or a prefix ending in `%` (`1.%`); default: the layer's existing bbappend, else `exact` when patches are added and `wildcard` otherwise |
| `set` | `{ "<left-hand side>": "<value>" }` (the left side may carry overrides and an operator: `"EXTRA_OECONF:append"`, `"PACKAGECONFIG ?="`) or an array of whole assignment lines; each must parse as one assignment. Distro, machine and image policy (`DISTRO_FEATURES`, `MACHINE_FEATURES`, `IMAGE_INSTALL`, `PREFERRED_PROVIDER*`, `PREFERRED_VERSION*`) is refused |
| `files` | patch or file names, or `{ "name", "content" }` objects whose content is written too: added with `SRC_URI:append = " file://..."`, under `<pn>/` beside the bbappend, with `FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"`; a note says where to copy a file without content |
| `machine` | every change becomes conditional on that machine (`:append:<machine>`, `VAR:<machine>`, never `VAR:<machine> +=`); a note when the machine has no conf in the indexed layers |

Notes also name other bbappends that apply to the recipe, and say when the recipe is in the
target layer itself (editing it may be simpler).

```
--- create meta-mine:recipes-core/libfoo/libfoo_2.0.bbappend
FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"

SRC_URI:append = " file://0001-fix-build.patch file://defaults.cfg"
--- create meta-mine:recipes-core/libfoo/libfoo/defaults.cfg
key=value
note: version: exact (patches usually fit one version); pass `version` to choose exact, wildcard or a prefix such as 1.%
note: copy 0001-fix-build.patch to meta-mine:recipes-core/libfoo/libfoo/0001-fix-build.patch
```

`layer`: `name` is the layer directory (`meta-<name>`, lower-case letters, digits and
`-`), which is also the collection name, as `bitbake-layers create-layer` makes it; a name
or collection already in use is refused. Creates `<path>/<name>/conf/layer.conf` and
`README.md`, and adds the layer to `BBLAYERS` in `conf/bblayers.conf` of the build directory
as a second change of the same plan (as `${TOPDIR}/../<dir>` when the layer is beside the
build directory's parent). The tool's `layer` argument is not used (if given, it must
still name a writable layer).

| Option | Notes |
|---|---|
| `path` | the directory to create the layer in (absolute, or relative to the working directory); default: the directory holding the first writable layer, else the first layer. It must already hold a workspace layer and lie outside every layer and the build directory |
| `priority` | `BBFILE_PRIORITY`, default 6 |
| `depends` | `LAYERDEPENDS` collections; default `core` when a layer provides it; a note for a collection no layer provides |
| `compat` | `LAYERSERIES_COMPAT`; default the core layer's own `LAYERSERIES_COMPAT`, else the workspace release (with a note); a note for a name sous-chef does not know |
| `copying` | also write `COPYING.MIT` |
| `example` | also create `recipes-example/` (as `recipes-example/.gitkeep`) |

The new layer's files are not linted after the write, and a running server does not see the
layer until it discovers the workspace again (restart it); to scaffold into it, list it in
`writable_layers`. The notes say so. Without a build directory a note says to add the path
to `BBLAYERS` or to `layers` in `sous-chef.toml`.

`class`: `name` is the class name; creates `<name>.bbclass` in the target layer, in
`classes-recipe/` (or `classes-global/` for `scope: global`) when every release the layer
is checked against has the split class directories, else in `classes/` (a layer that
claims Kirkstone), with a note.

| Option | Notes |
|---|---|
| `scope` | `recipe` (default; used with `inherit`) or `global` (used with `INHERIT +=`) |
| `tasks` | task names (`configure`, `do_deploy_docs`), strings such as `"deploy_docs after do_install before do_build"`, or `{ "name", "after", "before" }` objects; each becomes `<class>_do_<task>` exported with `EXPORT_FUNCTIONS`. A task OE-core already has (fetch, unpack, patch, configure, compile, install) gets no `addtask`; any other gets `addtask` (default after `do_install`, before `do_build`) |
| `variables` | the interface: `{ "NAME": "default" }` or `["NAME=default", "NAME"]`, written with `?=` |
| `description` | one line for the header comment |

A class name with `-` is refused when it has tasks (BitBake refuses `EXPORT_FUNCTIONS` of
shell tasks from such a class). Notes say how to use the class and name an existing class
of the same name.

```
--- create meta-mine:classes/widget_docs.bbclass
# widget_docs.bbclass
#
# Builds and deploys the widget documentation.
#
# Use: inherit widget_docs
#
# Interface (weak defaults, ?=; set them in the recipe to override):
#   WIDGET_DOCS_DIR     default "${datadir}/doc/${PN}"
#   WIDGET_DOCS_FORMAT  default "html"
#
# Tasks (exported with EXPORT_FUNCTIONS; a recipe's own do_<task> replaces them):
#   do_configure: replaces the default implementation
#   do_install: replaces the default implementation
#   do_deploy_docs: new task, after do_install before do_build
#   do_check_docs: new task, after do_compile before do_build

WIDGET_DOCS_DIR ?= "${datadir}/doc/${PN}"
WIDGET_DOCS_FORMAT ?= "html"

widget_docs_do_configure() {
	:
}

widget_docs_do_install() {
	install -d ${D}
}

widget_docs_do_deploy_docs() {
	:
}
addtask deploy_docs after do_install before do_build

widget_docs_do_check_docs() {
	:
}
addtask check_docs after do_compile before do_build

EXPORT_FUNCTIONS do_configure do_install do_deploy_docs do_check_docs
note: classes/ because meta-mine claims kirkstone, which does not search classes-recipe/ or classes-global/ (BitBake 2.0); classes/ is searched by every release
note: use it with `inherit widget_docs` in a recipe (meta-mine:classes/widget_docs.bbclass)
```

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

In a kas project (and only then) it adds `kas` (the files as kas takes them and how they
were found, e.g. `kas/kiss.yaml:kas/dogbonedark.yaml (from kas in sous-chef.toml)`),
`kas_defaults` (`machine dogbonedark, distro kiss, target kiss-image`), `kas_layers` (how
many layers the kas files name, and those not checked out yet), `kas_lock` (lock files
read), `kas_mismatch` (layers in the kas files but not in `bblayers.conf`, and the reverse:
the kas files changed since the last checkout) and one `kas_problem` row per problem; in
JSON, a `kas` object with the same fields. The `bitbake` row's mode is `init-script`,
`bitbake-setup`, `kas` or `initialised`.

## MCP resources

- `sous-chef://help`: this document, condensed.
- `sous-chef://rules`: the lint rule catalogue, rows `id severity scope flags`. `severity`
  is the effective one (`[lint.severity]` applied); `flags` is the description from
  lint-rules.md, prefixed with `[releases A to B]` for a rule limited to some releases and
  `[off by default]` for a rule that runs only when selected. A last line lists
  `[lint] disable`, if set.

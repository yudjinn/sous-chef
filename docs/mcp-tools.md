# MCP tools

Common to every tool:

- `budget_tokens` (default 600) and `format` (`"text"` default, or `"json"`).
- Output is a tab-separated table through `render.rs`; see the output rules in
  [architecture.md](architecture.md). Notes follow the table.
- Each call that reads the index starts with `Index::refresh` (`bitbake` and `slice` do not
  read it; `log` refreshes only to point at the task definition; `scaffold` refreshes before
  it plans). A refresh with nothing changed stats every file: 19 ms on the 1784-file live
  workspace (performance.md), which is most of a lookup tool's time. Freshness is reported
  only if something changed.
- Files BitBake never parses stay indexed but are marked: a `.bb` / `.bbappend` that no
  layer's `BBFILES` glob matches is no recipe, no provider and no bbappend, and a tool that
  shows one says so in a note; a data file (a `.conf` / `.inc` / `.bbclass` a recipe
  installs, bitbake-semantics.md "Data files") is shown by `item` with a note and left out of
  `search`, file listings and lint.
- Recipe arguments accept `pn`, `pn_pv` or a pointer. A name that is no recipe's exact `pn`
  is an error listing up to 20 recipe names that contain it. In `recipe`, `var`,
  `overrides`, `deps`, `item`, `search` and the `scaffold` `bbappend` kind a bare `pn`
  names the file BitBake would use, through provider selection (`PREFERRED_VERSION`,
  `DEFAULT_PREFERENCE`, layer priority, version; see "Providers and versions" in
  bitbake-semantics.md).
- `machine` and `distro`, where a tool takes them, default to what the evaluated
  configuration sets (`local.conf`, and OE configuration fragments such as bitbake-setup's
  `OE_FRAGMENTS` in `conf/toolcfg.conf`). In a kas project whose build directory does not
  exist yet, the kas files' `machine` / `distro` (else kas's defaults) are used, with a note
  naming where they came from. When nothing sets `MACHINE`, every evaluated view ends with
  `note: no MACHINE is set (...)`: machine-specific values are not applied.

The first six are always loaded. The rest are deferred (short descriptions, loaded on demand).

## Always loaded

### `recipe`
Resolve a recipe and show its shape.

| Arg | Type | Notes |
|---|---|---|
| `name` | string | required: a `pn`, a `pn_pv`, or a pointer to a `.bb` or a `.bbappend` (a bbappend pointer means the recipe it applies to) |
| `version` | string? | default: the version BitBake would use (provider selection for the default machine and distro); `%` matches any rest |
| `show` | `"summary"` \| `"tree"` \| `"outline"` \| `"sources"` \| `"tasks"` \| `"packages"` | default `summary` |

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

Every view notes a file that no layer's `BBFILES` glob matches (only a pointer reaches one):
`note: <file> is not matched by any layer's BBFILES: BitBake does not collect it, so no
recipe name or provider resolves to it and no bbappend applies`. The evaluated views
(`sources`, `tasks`, `packages`, and `var`, `overrides`, `deps`) also print the
evaluation's notes on where the machine and distro came from (a kas project without a build
directory) and `no MACHINE is set`; `summary` names only the machine and distro it used.
- `tree`: the files of the recipe in parse order, statically: the `.bb` and what it pulls in
  where it pulls it in, then each bbappend in application order the same way, then deferred
  inherits. Rows `depth kind name file via state`; `state` is `-`, `unresolved` (missing, or
  the argument needs evaluation), `cycle` or `already inherited`. `base.bbclass` and the
  `INHERIT` classes are not shown; a note says so.
- `outline`: every function and task statement across that tree: `name kind file_kind loc`.
  `kind` is `shell`, `python`, `fakeroot shell`, `def`, `addtask`, `deltask` or
  `addhandler`; anonymous python is named `__anonymous`.

`sources`, `tasks` and `packages` are evaluated (`eval::evaluate` of the chosen file for
the default machine and distro). Each ends with a note naming the machine and distro and
the warnings about anonymous python that every evaluated view gives (stronger for one that
changes variables through computed names).

- `sources`: rows `entry type state file status loc`, one per word of the evaluated
  `SRC_URI` (`state` `active`) with the operation that contributed it (`loc`, whose layer
  is the contributing layer), then the words of inactive operations
  (`inactive (override boardx not active)`) and of operations whose words were all removed
  (`removed by :remove`). `type` is the URL scheme, with ` (patch)` for a patch as OE-Core
  decides it (`apply=` parameter, else a `.patch` / `.diff` extension). A `file://` entry's
  `file` is the first existing `<dir>/<path>` along `FILESPATH` (a directory is marked and is
  never a patch); `status` is a local patch's `Upstream-Status` header as OE-Core's QA
  check reads it (`malformed: <line>` or `missing Upstream-Status` otherwise). An active
  entry that is not found is followed by its search order, one `searched` row per
  directory. `FILESPATH` is python in OE-Core (`base_set_filespath`), which is not run: the
  search order is reconstructed (see "Files and patches" in bitbake-semantics.md) and a
  note says so. Files are read through the confinement check.
- `tasks`: rows `task state after pieces loc`: the tasks of the recipe, dependencies first
  (ties in the order they were added), from the `addtask` / `deltask` statements of the
  configuration's classes, the recipe, its includes and classes and its bbappends, in parse
  order (read from the index; the replay does not run them). `after` is the task's
  dependencies as `addtask ... after` and `before` leave them; `pieces` the composed
  function as pointers, an `EXPORT_FUNCTIONS` stub followed by the class function it calls
  and that function's pieces; `state` `noexec` (the flag) or `deleted` (with the `deltask`
  as `loc`); `loc` the `addtask`. Not accounted for, and said so in a note: tasks added or
  deleted by anonymous python, `do_x[deps]` set directly (a warning names any such
  assignment) and dependencies on other recipes' tasks.
- `packages`: rows `package var value loc`: `PACKAGES` (and `PACKAGES_DYNAMIC` when set),
  then per package `FILES:<pkg>` (always) and `RDEPENDS:<pkg>`, `RPROVIDES:<pkg>` (where
  set), evaluated, with the applied operations as `loc`. A value that depends on python is
  flagged, not guessed; a `PACKAGES` word that does not expand is counted in a note; what
  `PACKAGES_DYNAMIC` and `do_package` add is not listed, and a note says so.

`sources` of `hello` on a copy of the fixture with a missing file, patches with a malformed
and no `Upstream-Status`, a directory and a `:remove` (search rows cut here):

```
entry	type	state	file	status	loc
https://example.org/releases/hello-1.1.tar.gz	https	active	-	-	meta-core:recipes-core/hello/hello.inc:9
file://hello.patch	file (patch)	active	meta-core:recipes-core/hello/files/hello.patch	Upstream-Status: Pending	meta-core:recipes-core/hello/hello_1.1.bb:5
file://bad.patch	file (patch)	active	meta-mine:recipes-core/hello/hello/bad.patch	malformed: upstream status: maybe	meta-mine:recipes-core/hello/hello_1.1.bbappend:11
file://none.diff	file (patch)	active	meta-mine:recipes-core/hello/hello/none.diff	missing Upstream-Status	meta-mine:recipes-core/hello/hello_1.1.bbappend:11
file://cfgdir	file	active	meta-mine:recipes-core/hello/hello/cfgdir (directory)	-	meta-mine:recipes-core/hello/hello_1.1.bbappend:11
file://nowhere.cfg	file	active	not found (10 paths searched, below)	-	meta-mine:recipes-core/hello/hello_1.1.bbappend:11
file://keep.patch;apply=no	file	active	not found (10 paths searched, below)	-	meta-mine:recipes-core/hello/hello_1.1.bbappend:11
git://example.org/x.git;branch=main	git	active	-	-	meta-mine:recipes-core/hello/hello_1.1.bbappend:11
file://mine.patch	file (patch)	active	meta-mine:recipes-core/hello/hello/mine.patch	Upstream-Status: Submitted [https://example.org/hello/pull/1]	meta-mine:recipes-core/hello/hello_1.1.bbappend:4
file://vendor.patch	file (patch)	removed by :remove	meta-vendor:recipes-core/hello/hello/vendor.patch	Upstream-Status: Inappropriate [vendor specific]	meta-vendor:recipes-core/hello/hello_%.bbappend:4
file://boardx.cfg	file	inactive (override boardx not active)	meta-vendor:recipes-core/hello/hello/boardx.cfg	-	meta-vendor:recipes-core/hello/hello_%.bbappend:5
file://nowhere.cfg	searched	#1	meta-mine:recipes-core/hello/hello/	missing	-
file://nowhere.cfg	searched	#2	meta-vendor:recipes-core/hello/hello/	missing	-
file://nowhere.cfg	searched	#3	meta-core:recipes-core/hello/hello-1.1/	missing	-
...
```

`tasks` of `hello`:

```
task	state	after	pieces	loc
do_fetch	-	-	meta-core:classes/base.bbclass:54 -> base_do_fetch (meta-core:classes/base.bbclass:15-20)	meta-core:classes/base.bbclass:13
do_unpack	-	do_fetch	meta-core:classes/base.bbclass:54 -> base_do_unpack (meta-core:classes/base.bbclass:23-26)	meta-core:classes/base.bbclass:22
do_patch	-	do_unpack	meta-core:classes/base.bbclass:54 -> base_do_patch (meta-core:classes/base.bbclass:29-31)	meta-core:classes/base.bbclass:28
do_autoreconf	-	do_patch	meta-core:classes-recipe/autotoolsish.bbclass:30 -> autotoolsish_do_autoreconf (meta-core:classes-recipe/autotoolsish.bbclass:13-15)	meta-core:classes-recipe/autotoolsish.bbclass:16
do_configure	-	do_autoreconf do_patch	meta-core:classes-recipe/autotoolsish.bbclass:30 -> autotoolsish_do_configure (meta-core:classes-recipe/autotoolsish.bbclass:18-20)	meta-core:classes/base.bbclass:33
do_compile	-	do_configure	meta-core:classes-recipe/autotoolsish.bbclass:30 -> autotoolsish_do_compile (meta-core:classes-recipe/autotoolsish.bbclass:22-24)	meta-core:classes/base.bbclass:38
do_install	-	do_compile	meta-core:classes-recipe/autotoolsish.bbclass:30 -> autotoolsish_do_install (meta-core:classes-recipe/autotoolsish.bbclass:26-28), meta-core:recipes-core/hello/hello.inc:15-18, meta-mine:recipes-core/hello/hello_1.1.bbappend:8-10	meta-core:classes/base.bbclass:45
do_build	noexec	do_install	-	meta-core:classes/base.bbclass:51
note: order from the addtask / deltask statements in parse order (the index: configuration classes, recipe, includes and classes, bbappends, deferred inherits; the replay does not run them); dependencies first
note: not accounted for: tasks added or deleted by anonymous python (bb.build.addtask / deltask), do_x[deps] set directly, and dependencies on other recipes' tasks ([depends], [deptask], [rdeptask], [recrdeptask])
note: evaluated for MACHINE=qemux86 DISTRO=testdistro (static replay of the configuration, classes, recipe and bbappends; python not run)
```

`packages` of `libfoo`:

```
package	var	value	loc
-	PACKAGES	libfoo-dbg libfoo-dev libfoo-doc libfoo	meta-core:conf/bitbake.conf:26
libfoo-dbg	FILES	(not set)	-
libfoo-dev	FILES	/usr/include /usr/lib/lib*.so	meta-core:conf/bitbake.conf:28
libfoo-dev	RDEPENDS	libfoo	meta-core:recipes-core/libfoo/libfoo_2.0.bb:18
libfoo-doc	FILES	/usr/share/doc	meta-core:conf/bitbake.conf:29
libfoo	FILES	/usr/bin/* /usr/sbin/* /usr/lib/lib*.so.*	meta-core:conf/bitbake.conf:27
note: values of the evaluated datastore (PACKAGES and FILES defaults come from bitbake.conf); RDEPENDS and RPROVIDES rows only where set
note: not included: what OE-Core adds while packaging (do_package: shared-library and pkg-config dependencies, packages split by python such as do_split_packages) and the RDEPENDS:${PN} that base.bbclass adds for PACKAGECONFIG with anonymous python (see deps kind=runtime)
note: evaluated for MACHINE=qemux86 DISTRO=testdistro (static replay of the configuration, classes, recipe and bbappends; python not run)
```

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
  BitBake's rewrite of the virtual providers in `DEPENDS` at the end of parsing
  (`BB_RECIPE_VIRTUAL_PROVIDERS`) is a row with op `rewrite` (status `applied`, located at
  the `PREFERRED_PROVIDER_*` used, snippet `DEPENDS rewritten after parsing ... to "..."`);
  it replaces nothing, so the rows before it keep their status.
- `override`: the overrides as written (`append:boardx`), `-` for none; a flag operation
  shows `[flag]` (`append[flag]` with overrides).
- A function is a variable too: each definition is a row with snippet `name() { ... }`, and
  `final:` lists its pieces in order as pointers (`layer:path:A-B`, with `-> class_do_x` for
  an `EXPORT_FUNCTIONS` stub).

Then the notes: `final:` with the value in quotes (`final[flag]:` for flags; `final: X is
not set`), cut at 600 characters with the full text in a `value` handle; references left
unexpanded (`(unresolved, left as written: X)`, only for a `${X}` still in the text); `(may
differ: an override decision it depends on is uncertain because OVERRIDES has unresolved
X)` when the text is complete but an override variant or a conditional `:append` /
`:prepend` / `:remove` of the variable, or of one it references, was judged inactive while
part of `OVERRIDES` is unknown (a value with no conditional operation in its history never
carries it), or `(may differ: depends on unresolved X)` when a statement the replay could
not apply may set it (`MACHINE` when the `addfragments` list does not expand); the
evaluation's notes (see `recipe`: an uncollected file, the machine's source, no `MACHINE`);
a count of references not followed (python, undefined variables, missing files); a note for
each reference followed although its value is uncertain (``note: `inherit_defer
toolchain/${TOOLCHAIN}` ... was followed as "toolchain/gcc", but it depends on python or on
overrides that are not all known; values its files set are flagged``); a warning per
anonymous python function that changes the variable by name, and
one for each that is opaque (changes variables through computed names or hands `d` to other
code), since anonymous python is never run; a count of the other anonymous functions;
`note: global configuration only (no recipe)` without a recipe; the python note when the
value depends on python.

**Authoritative mode** (`authoritative: true`) answers from `bitbake -e [<recipe>]`
instead of the static evaluator:

- Without a usable BitBake environment the error is `bitbake is not available: ...`.
  `machine` and `distro` are refused (BitBake uses the build configuration's). `recipe`
  must be a name BitBake accepts (letters, digits and `_ . + - /`; not a pointer).
- **Names.** Only the recipe name is passed to BitBake, validated like every target. The
  variable name is never put on a command line: it is looked up in the parsed output, so
  `FILES:${PN}` (expanded with BitBake's own `PN`), `SRC_URI:append` or `T:task-clean`
  need no relaxed validation.
- **Running.** `bitbake -e` runs as a background job (operation `env`, visible to
  `bitbake op=status` / `op=cancel`) and the tool waits up to 120 s for it (about 15 s on a
  real environment). One job runs at a time because BitBake allows one client per build
  directory: while another job (a build) runs, the tool answers from a cached dump if the
  tree is unchanged, and otherwise returns an error naming the running job and suggesting
  static mode. If its own job is still running after 120 s it returns the job id; the next
  call for the same recipe waits for that job instead of starting another. A failed run is
  an error with the first `ERROR:` lines and the output's handle.
- **Cache.** The output is stored as an `env` handle, one per recipe (and one for the
  global configuration), keyed by the index state of the recipe's `.bb` files and
  bbappends and of every `.conf`, `.bbclass` and `.inc` in the workspace; an unchanged
  tree reuses it, also after a server restart while the handle is kept. The source line
  says `reused` then.
- **Output**: the static table (`status op override loc snippet`), one row per operation
  in BitBake's history, in BitBake's order (the variable's own operations as they ran,
  then those of its override variants). `status` is `inactive` when an override the
  operation depends on is not in BitBake's final `OVERRIDES` and `active` otherwise:
  BitBake records that an operation ran, not whether it contributed, so there is no
  `applied` / `shadowed`. `op` is the statement's operator (`??=` included); for an
  operation recorded by python code it is BitBake's own name (`set`, `append`, `rename`)
  and `loc` is BitBake's `file.py:line`. `loc` is a pointer when the file is in the
  workspace, else the absolute path with `..` resolved. Flag operations follow with
  `flags: true` (`final[flag]:` only when the history gives the value without guessing:
  `bitbake -e` prints no flag values). A function is one row (`shell` with its
  `# line:` location, or `python`), with a `final:` line giving its length and the
  `slice` call for its body.
- Notes: `final:` as in static mode (cut at 600 characters, full text in a `value`
  handle), `final: X is not set` when BitBake printed no value, `final: not printed
  (unexported)` with the pre-expansion value for an unexported variable (`bitbake -e`
  prints `unset X`), `note: exported ...` for an exported one, then
  `source: BitBake (bitbake -e curl), handle h:...` with the `slice` call for the raw
  block, the status note, and a count of dump lines the parser did not recognise.

`FILE_LAYERNAME` of `curl` (the `bitbake -e curl` dump recorded on the live environment,
BitBake 2.18.0, `tests/fixtures/outputs/envdump/`, as `tests/var_authoritative.rs` checks it;
the handle id differs per run):

```
status	op	override	loc	snippet
active	??=	-	meta:conf/bitbake.conf:801	FILE_LAYERNAME ??= "config"
active	set	-	cookerdata.py:513	_parse_recipe: FILE_LAYERNAME set "core"
final: "core"
source: BitBake (bitbake -e curl), handle h:0bf91ad514; slice handle=h:0bf91ad514 var=FILE_LAYERNAME shows the raw block
note: status is inactive when an override the operation needs is not in BitBake's final OVERRIDES, else active (BitBake records that an operation ran, not whether it contributes)
```

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
  that is python is unknown and said so. `OVERRIDES` is split on `:` with `${...}` kept
  whole (bitbake-semantics.md, "Splitting list values"): an entry that holds python is one
  row, abbreviated (`${@...}dogbonedark`, `linux${@...}eabi`), from `${@...} (python, not
  evaluated): stands for entries that are not known`; it is never split into fragments and
  never counted as an override name.
- `conditional`: rows `state condition name loc snippet`: every operation whose name
  carries an override condition, in parse order, `active` or `inactive` for this machine
  and distro. With a recipe: the operations from the recipe's own files (recipe, includes,
  classes, bbappends: the files read in the recipe pass, not an `.inc` the configuration
  includes) and `.conf` operations conditional on `pn-<pn>`; without, the
  configuration's operations. Package-name overrides (`FILES:${PN}-dev`,
  `RDEPENDS:libfoo-dev`) are not conditions and are left out.
- `dead`: rows `override closest name loc snippet`: the conditional operations with an
  override element that **matches no known override**, by the lint rule `override/dead`'s
  definition: not in the evaluated `OVERRIDES`, not a name `KnownOverrides` accepts
  (machine and distro conf names, declared and built-in overrides, `class-` / `pn-` /
  `layer-` / ... forms), not a class name, not a word of any `*OVERRIDES` assignment in the
  workspace (python included), not a package of the recipe. Not listed, and counted in
  notes instead: operations whose overrides are known but inactive for this machine and
  distro; operations on a variable that BitBake reads by name (some value holds
  `${VAR:<prefix>${...}}`, `'VAR:<prefix>' +` or `"VAR:<prefix>%s"`: OE-core's
  `TUNE_FEATURES:tune-<tune>`, read as `${TUNE_FEATURES:tune-${DEFAULTTUNE}}`); a name in
  the package position of a package variable, or of any variable while `PACKAGES` holds
  python (a package python adds). `closest` is the nearest known name by edit distance.

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
note: dead = an override element that matches no known override: not in OVERRIDES, not a machine, distro or class name, not a declared or built-in override, not a word of any *OVERRIDES assignment in the workspace (python included), not a package of the recipe; the same definition as the lint rule override/dead
note: 2 inactive conditional operations use known overrides: inactive for this machine and distro, not dead (show=conditional lists them)
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
marker. JSON: `{loc, sha256, text, more_lines, rest, notes}`. A note follows the text when
BitBake does not parse the file as indexed: a `.bb` / `.bbappend` no `BBFILES` glob matches
(as in `recipe`), or a data file (`is not BitBake metadata: a data file ...`).

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
hits, beyond which `N` is a lower bound. Data files (a `.conf` / `.inc` that is not BitBake
metadata, bitbake-semantics.md "Data files") are not searched in any mode. A hit in a `.bb`
/ `.bbappend` that no `BBFILES` glob collects is followed by a note per file: `note:
<layer>:<path> is not matched by any layer's BBFILES: BitBake does not collect it, so it is
no recipe and no bbappend applies`.

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
  status`), status `applies`; a bbappend that `BBFILES` collects and that matches nothing is
  one row with `matches` `-` and status `no-match`, and a note (BitBake stops on these). A
  collection comes from the index, which folds the layers' active `BBFILES_DYNAMIC` entries
  (`{!}<collection>:<glob>`, added to `BBFILES` when the collection is present, or absent
  with `!`) into `BBFILES`: a bbappend only an active entry collects is matched and
  evaluated like any other, its status followed by `(BBFILES_DYNAMIC)` (`applies
  (BBFILES_DYNAMIC)`, `no-match (BBFILES_DYNAMIC)`); one only an inactive entry matches is
  `not collected (dynamic layer `<name>` not present)` (or `(BBFILES_DYNAMIC `!<name>`:
  layer `<name>` is present)`); one `BBMASK` masks is `not collected (masked by BBMASK)`;
  one no glob matches, `not collected (no BBFILES glob)`. A note counts the bbappends
  BitBake never reads.
- `"compat"`: rows `layer check result detail loc`, per layer in `BBLAYERS` order. First a
  `compat` row: what `LAYERSERIES_COMPAT_<c>` claims against the workspace release and the
  core layer's `LAYERSERIES_CORENAMES` (`result` `ok`, or the worst severity of the
  layer's compat problems). Then one row per problem `org/layer-conf` finds in the compat
  series (`compat`) and in `LAYERDEPENDS` (`depends`: a collection no layer adds, or a class
  inherited from a layer that `LAYERDEPENDS` does not reach), with the rule's severity and
  message: the view calls the rule's checks, so the two agree. Last, `order` rows: a layer
  listed in `BBLAYERS` before a layer whose collection its `LAYERDEPENDS` names. BitBake does
  not need dependencies first (`LAYERDEPENDS` is checked against every enabled layer and
  priority does not come from the order); the row says what the order changes: when
  `BBPATH` lists the dependent layer first, the classes, `.inc` and conf files both layers
  have at the same path (`warning`, listed), or none (`info`). Without a build directory
  there is no `BBLAYERS` and no order row.

```
layer	check	result	detail	loc
meta-mine	compat	ok	LAYERSERIES_COMPAT_mine = "kirkstone scarthgap wrynose": names wrynose (the workspace release); shares wrynose with LAYERSERIES_CORENAMES "wrynose"	meta-mine:conf/layer.conf:11
meta-vendor	compat	error	LAYERSERIES_COMPAT_vendor = "scarthgap": does not name wrynose (the workspace release); shares no name with LAYERSERIES_CORENAMES "wrynose"	meta-vendor:conf/layer.conf:11
meta-vendor	compat	error	LAYERSERIES_COMPAT_vendor = "scarthgap" names none of the core layer's releases (LAYERSERIES_CORENAMES "wrynose"), so BitBake stops with "Layer vendor is not compatible with the core layer"; add wrynose once the layer works with it	meta-vendor:conf/layer.conf:11
meta-vendor	depends	warning	meta-vendor:recipes-vendor/libfoo-alt/libfoo-alt_1.0.bb:11 inherits a class from meta-mine (collection `mine`), but LAYERDEPENDS_vendor does not name `mine`; add it: LAYERDEPENDS_vendor += "mine"	meta-vendor:conf/layer.conf:10
meta-core	compat	ok	LAYERSERIES_COMPAT_core = "wrynose": names wrynose (the workspace release); shares wrynose with LAYERSERIES_CORENAMES "wrynose"	meta-core:conf/layer.conf:13
meta-core	depends	warning	meta-core:recipes-core/hello/hello.inc:11 inherits a class from meta-mine (collection `mine`), but LAYERDEPENDS_core does not name `mine`; add it: LAYERDEPENDS_core += "mine"	meta-core:conf/layer.conf:1
meta-mine	order	warning	meta-mine comes before meta-core in BBLAYERS, though LAYERDEPENDS_mine names `core` (added by meta-core). BitBake does not need dependencies first: LAYERDEPENDS is checked against every enabled layer and priority does not come from the order; BBPATH lists meta-mine first, so its classes-recipe/autotoolsish.bbclass is found instead of meta-core's by include/require, inherit and conf lookups; list meta-core first if that is not intended	build:conf/bblayers.conf:10-14
meta-vendor	order	info	meta-vendor comes before meta-core in BBLAYERS, though LAYERDEPENDS_vendor names `core` (added by meta-core). BitBake does not need dependencies first: LAYERDEPENDS is checked against every enabled layer and priority does not come from the order; BBPATH lists meta-vendor first, but no class, include or conf file is at the same path in both layers, so no lookup is affected today	build:conf/bblayers.conf:10-14
note: 1 error, 3 warning, 1 info; the compat and depends checks are org/layer-conf's (lint shows the same findings)
release: wrynose (LAYERSERIES_CORENAMES)
```

The sample is a copy of the fixture with `meta-mine` listed first in `BBLAYERS`, a copy of
`autotoolsish.bbclass` in `meta-mine`, and `meta-vendor` claiming only `scarthgap`
(`tests/layers_compat.rs`); the class copy is what moves hello's and libfoo-alt's inherit
into `meta-mine`. On the fixture itself every layer is `ok`.

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

There is no `machine` or `distro` argument: the recipe is evaluated for the configuration's
machine and distro (see the common section). `PACKAGECONFIG` additions to `DEPENDS` and `RDEPENDS:${PN}` are made by
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
  `PACKAGECONFIG` and where it is set. Fields 5 and 6 are not shown. BitBake's own flags
  (`doc`, `vardepvalue`, `vardeps`, ...; OE-core sets `PACKAGECONFIG[doc]` and
  `PACKAGECONFIG[vardepvalue]`) are not options and are named in a note, unless
  `base.bbclass` would really use one (bitbake-semantics.md, `PACKAGECONFIG`).
- Lists are split with `${...}` kept whole (bitbake-semantics.md, "Splitting list
  values"): a word of `DEPENDS`, `RDEPENDS:<pkg>` or `PACKAGECONFIG` that holds python is
  one row, abbreviated (`${@bb.utils.contains(...)}`), with provider / selection
  `unknown: not evaluated ...`; it is never resolved as a name. A python word of `PACKAGES`
  is named in a note (its packages are not listed).
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

`where` plans no files (it gives advice in the notes); the other kinds plan files and write
them with `write: true`.

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
Writes `recipes-<category>/<pn>/<pn>_<pv>.bb`. When recipes of that `PN` exist in other
layers, a note (never a refusal) names their files and layer priorities and which file
BitBake would then build (highest layer priority, then highest version, as
`findBestProvider` without `PREFERRED_VERSION`; equal ones: the first in `BBFILES` order),
and suggests `scaffold kind=bbappend` (or `kind=where`) to change the existing recipe.

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
build directory's parent). In a kas project the second change is to the kas file instead
(one inserted entry under the project's own repo; `bblayers.conf`, which kas generates, is
left alone), shown relative to the kas project root; the engine accepts that `Modify` only
for a file kas read, not a lock file, inside the project root. The tool's `layer` argument
is not used (if given, it must still name a writable layer).

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

`inc_split`: moves what every version of a recipe shares into `<pn>.inc` beside the `.bb`
files and makes each `.bb` `require` it (the `.inc` is created, each `.bb` modified). The
recipe is the `recipe` option or `name`; its versions are its `.bb` files in the target
layer, so the recipe must be in a writable layer.

| Option | Notes |
|---|---|
| `recipe` | the recipe (default: `name`) |
| `versions` | the versions to split (default: all versions in the layer); at least two. An unknown one is an error listing the versions there |

What moves: the statements common to all chosen versions (same name, operator and value for
an assignment, same lines for a function, same normalised text otherwise), in the same
relative order (a longest common subsequence, newest version first). Never moved: version
data (`PV`, `PR`, `PE`, `SRCREV*`, `LIC_FILES_CHKSUM`, `SRC_URI` checksum flags) and
`require` / `include` lines. A comment block directly above a moved statement moves with
it. Each `.bb` keeps the rest in its order, with `require <pn>.inc` where the first moved
statement was. Notes list what each version keeps and name other versions of the recipe
that are not changed.

The plan is refused (no files; the notes say why) when the move could change what BitBake
parses: a statement that stays in a `.bb` would come after a moved one it does not commute
with (they set a common variable or flag, or one expands immediately, with `:=` or in
`inherit` / `include`, what the other sets, the classes and files an `inherit` / `require`
pulls in counted as part of it), or a moved statement expands `FILE`, `PN`, `PV`, `BPN`,
`BP`, `P` or `PF` immediately (inside the `.inc`, `FILE` is the `.inc`). Also refused:
versions in different directories, an existing `<pn>.inc`, a version that does not parse
cleanly, versions with nothing in common. This check is static (the order of statements and
what they reference); the result is not evaluated to confirm the values are unchanged.

Two versions of `greet` (`tests/snapshots/scaffold_inc_split__inc_split_clean.snap`; the
`.inc` and `greet_1.1.bb` cut here):

```
--- modify meta-mine:recipes-app/greet/greet_1.0.bb
# Copyright example.org
# SPDX-License-Identifier: MIT

require greet.inc

LIC_FILES_CHKSUM = "file://COPYING;md5=0835ade698e0bcf8506ecda2f7b4f302"

SRC_URI[sha256sum] = "1111111111111111111111111111111111111111111111111111111111111111"
note: 11 statements shared by greet_1.1.bb and greet_1.0.bb move into meta-mine:recipes-app/greet/greet.inc; each version requires it
note: greet_1.1.bb keeps LIC_FILES_CHKSUM, SRC_URI, SRC_URI[sha256sum], PACKAGECONFIG and PACKAGECONFIG[greeting]
note: greet_1.0.bb keeps LIC_FILES_CHKSUM and SRC_URI[sha256sum]
```

Refused (`inc_split_refused_order.snap`):

```
note: nothing planned: the split is not safe or not possible
note: the split would change what BitBake parses for greet_1.1.bb and greet_1.0.bb:
note: meta-mine:recipes-app/greet/greet_1.1.bb:20 `GREET_FLAGS +=` would move into the .inc, ahead of meta-mine:recipes-app/greet/greet_1.1.bb:19 `GREET_FLAGS =`, which stays in the .bb: both set GREET_FLAGS
note: meta-mine:recipes-app/greet/greet_1.0.bb:19 `GREET_FLAGS +=` would move into the .inc, ahead of meta-mine:recipes-app/greet/greet_1.0.bb:18 `GREET_FLAGS =`, which stays in the .bb: both set GREET_FLAGS
note: make these statements the same in every version, or reorder them by hand, then re-run
```

`where`: placement advice; plans and writes nothing. Give a recipe (`name` or the `recipe`
option; `version` picks a version) and a `variable`, a `task` or an `intent`; with only a
recipe the advice is for changing that recipe in general. Without an `intent` it is taken
from the variable (`PREFERRED_VERSION_*`, `PREFERRED_PROVIDER_*`, distro policy, machine
features, image content, else a recipe change).

| Option | Notes |
|---|---|
| `recipe` | the recipe (default: `name`) |
| `variable` | the variable to change (overrides allowed); not with `task` |
| `task` | the task to change (`do_install`) |
| `intent` | `add-patch`, `change-config`, `add-package-to-image`, `set-machine-feature`, `pin-version` or `change-provider` |
| `machine` | the change is for one machine |
| `image`, `package` | for `add-package-to-image`: the image recipe (default: the recipe when it is an image) and the package (default: the recipe when it is not) |
| `item` | for `change-provider`: the provided name (`virtual/kernel`); default from a `PREFERRED_PROVIDER_<name>` variable |
| `value` | the value, put into the suggested `scaffold` call |

The answer is in the notes, one line each: `where:` the file (an existing one as a
pointer, or the path to create), `why:` the convention as the `org` rules encode it,
`scaffold:` the call that makes the change, or `by hand:` when no generator writes that
file, then facts with pointers (`now:` where the value is set now, other bbappends that
apply, how the layer was chosen). The conventions: a recipe in a layer that is not writable
is changed through a bbappend in a writable layer (the existing one when there is one); a
recipe in a writable layer is edited directly; hardware values go in the machine conf (or a
`:<machine>` override in the bbappend for one recipe); policy (`DISTRO_FEATURES`,
`PREFERRED_PROVIDER_*`, `PREFERRED_VERSION_*`) in the distro conf (machine conf for the
kernel and bootloader providers); image content in the image recipe; `local.conf` only as a
developer-local trial. The writable layer is the `layer` argument when it is writable, else
the only writable layer, else the one holding related content, else the highest priority
one, with a note.

A variable of an upstream recipe that a bbappend in the writable layer already changes
(`tests/snapshots/scaffold_where__where_table.snap`, first case; `options: {"variable":
"EXTRA_OECONF", "value": " --enable-x"}`):

```
where: meta-mine:recipes-core/hello/hello_1.1.bbappend (the existing bbappend)
why: meta-core:recipes-core/hello/hello_1.1.bb is in meta-core, which is not writable (an update of the layer overwrites edits there, org/upstream-edit); a bbappend in a writable layer changes it from outside, and one exists already: add to it rather than a second one
scaffold: scaffold kind=bbappend name=hello layer=meta-mine options={"set":{"EXTRA_OECONF:append":" --enable-x"}}
note: meta-vendor:recipes-core/hello/hello_%.bbappend also applies to meta-core:recipes-core/hello/hello_1.1.bb
now: EXTRA_OECONF is set last at meta-mine:recipes-core/hello/hello_1.1.bbappend:6 (`EXTRA_OECONF:append =`)
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

Commands run in the build environment, found in this order (`status` prints the mode):
`[bitbake] enabled = false` turns it off; `init_script` from `sous-chef.toml` (`init-script`
mode, or `bitbake-setup` when the file is named `init-build-env`); an environment the server
was started in (`BUILDDIR` or `BBPATH` set and `bitbake` on `PATH`: `initialised`, the
program is run directly); a kas project with `kas` on `PATH` and an existing kas build
directory (`kas`: every command is `kas shell --keep-config-unchanged <files> -c '<command>'`
in kas's work directory, so kas does not rewrite `conf/` first); a build directory holding
bitbake-setup's `init-build-env` (`bitbake-setup`); an `oe-init-build-env` in a layer root,
a layer root's parent or the build directory's parent (`init-script`). The init script is
sourced and the program run by a wrapper that takes every name as argv; in `kas` mode the
command string is made only of validated words, each single-quoted.

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

The exit status is BitBake's `Task (...) failed with exit code`, which only the console
prints: it comes from the log itself when the shell exit trap gives it, else from the
job's output or the newest console log (`tmp/log/cooker/<machine>/`) when that names the
digested log; otherwise the row is missing. For a fetch failure a `cause` row gives the
downloader's first error line (`wget: unable to resolve host address ...`, `ERROR 404: Not
Found.`) and how many more there are (mirrors); the `Fetch command ... failed` line is
treated as a consequence. The definition pointer is the recipe's own function, else one
from its included files and classes, else the default of a global class (`base_do_fetch` in
`base.bbclass`, or an `INHERIT` class); when the recipe is in no layer of the workspace, a
note says so instead.

With `job`: the log named in the job's output (the first failed task's
`Logfile of failure stored in:`, then any other log it names), filtered by `recipe` and
`task`. If none is readable it digests the job's console output, with a note. A running job
is an error.

### `slice`
| Arg | Notes |
|---|---|
| `handle` | `h:<id>` required |
| one of | `lines: "A-B"`, `grep: "RE"` (+ `context`), `var: "NAME"` (env dumps), `failures: true`, `page: K` |

Capped at 8 KB; says what it cut. `var` finds the block with the `bitbake -e` parser: for a
variable, from `# $NAME` to its final assignment (`NAME="..."`, `export NAME="..."`,
`unset NAME`, or the last history line when BitBake printed no value); for a shell
function, from its `# line:` comment to the closing `}`; for a python function, from
`python NAME () {` to the closing `}`. Override variants (`T:task-clean`) are blocks of
their own; names are exact.

### `status`
No arguments. Workspace roots, the release (`release`, e.g. `wrynose (LAYERSERIES_CORENAMES)`),
layer count, file counts by kind, index path and age, whether a build directory and a usable
bitbake environment were found, and the running job if any (`job`, e.g.
`job:1 task hello running 1s`). Layers are named by their directory (`meta`,
`meta-arm-toolchain`) in every tool and pointer; BitBake 2.18's `bitbake-layers
show-layers` prints the collection name instead (`core`, `arm-toolchain`).

In a kas project (and only then) it adds `kas` (the files as kas takes them and how they
were found, e.g. `kas/kiss.yaml:kas/dogbonedark.yaml (from kas in sous-chef.toml)`),
`kas_defaults` (`machine dogbonedark, distro kiss, target kiss-image`), `kas_layers` (how
many layers the kas files name, and those not checked out yet), `kas_lock` (lock files
read), `kas_mismatch` (layers in the kas files but not in `bblayers.conf`, and the reverse:
the kas files changed since the last checkout) and one `kas_problem` row per problem; in
JSON, a `kas` object with the same fields. The `bitbake` row's mode is `init-script`,
`bitbake-setup`, `kas` or `initialised`.

## MCP resources

- `sous-chef://help`: this document condensed, generated from the tool registry and the
  argument schemas (each tool's description and arguments).
- `sous-chef://rules`: the lint rule catalogue, rows `id severity scope flags`. `severity`
  is the effective one (`[lint.severity]` applied); `flags` is the description from
  lint-rules.md, prefixed with `[releases A to B]` for a rule limited to some releases and
  `[off by default]` for a rule that runs only when selected. A last line lists
  `[lint] disable`, if set.

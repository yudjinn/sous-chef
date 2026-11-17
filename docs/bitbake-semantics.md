# BitBake semantics we reproduce

This is the reference for the parser, the workspace code, the evaluator and lint. It is a
working summary, not the authority. **When a case is not covered here or you are unsure,
check the BitBake User Manual ("Syntax and Operators") or the BitBake source
(`lib/bb/parse/`, `lib/bb/data_smart.py`), then add what you learned to this file.** Items
marked *verify* were written from memory and must be confirmed by the task that implements
them.

## File kinds

| Extension | Kind | Notes |
|---|---|---|
| `.bb` | Recipe | Name is `<pn>_<pv>.bb`. No underscore means `PV = "1.0"`. `PN` and `PV` come from the file name. |
| `.bbappend` | Append | Applies to the recipe with the same name. `%` is a wildcard for the rest of the version: `busybox_1.%.bbappend`, `busybox_%.bbappend`. |
| `.inc` | Include | Pulled in by `require` / `include`. Any path, any content. |
| `.bbclass` | Class | Pulled in by `inherit` (recipe) or `INHERIT` (global). |
| `.conf` | Configuration | Only assignments, `include`/`require`, `export`; no functions or tasks. |

## Statements

Lines ending in `\` continue on the next line. `#` starts a comment at the start of a line
(a `#` inside a quoted value is part of the value; a comment line inside a continued
assignment is a known source of warnings).

### Lines (verified, T1.1/T1.2)

Checked against the BitBake source, `master` as of 2026-11 (`lib/bb/parse/parse_py/BBHandler.py`
`get_statements` / `feeder`, `ConfHandler.py` `handle` / `feeder`). This is what `src/parse`
reproduces:

- Files are read in Python text mode (universal newlines: `\n`, `\r\n` and a lone `\r` all
  end a line) and **every physical line is right-stripped** (`str.rstrip()`) before anything
  else. So `\r`, trailing blanks, and **trailing whitespace after a `\`** are removed first:
  `VAR = "a \   ` + newline **does continue** onto the next line. (The milestone text said it
  does not; the source says it does. We still emit a diagnostic because it is misleading.)
- Continuation: a line whose stripped text ends in `\` is joined to the next one with the `\`
  removed and nothing inserted (the next line's leading whitespace is kept). A `\` on the
  last line of the file leaves "unparsed lines": an error.
- Comments: only a `#` in **column 0** starts a comment. An indented `   # x` is *not* a
  comment to BitBake: it reaches the assignment regex and fails with "unparsed line". We treat
  it as a comment and emit a diagnostic. A comment line ending in `\` continues onto the next
  line, which must also be a comment; a comment line inside a continued non-comment statement,
  or a comment continued onto a non-comment line, is a fatal error.
- Leading whitespace before any statement (`  VAR = "x"`) is an "unparsed line" error.
- A UTF-8 BOM is not stripped (the file is opened without `utf-8-sig`), so line 1 of a file
  with a BOM fails to parse in BitBake. We strip it and emit a diagnostic.
- Function bodies are collected before continuation and comment handling: inside a body, `\`
  does not join and `#` is body text. The body ends at the first line that is exactly `}`
  after right-stripping (so `}   ` closes it, `  }` does not). End of file inside a body is
  an error ("Shell function ... is never closed").
- `def` bodies: after a line matching `def\s+(\w+).*:`, every following line that starts
  with whitespace, is empty, **or starts with `#`** belongs to the body; the first other line
  ends it and is parsed normally. (We leave trailing blank and comment lines out of the
  reported body span.)
- Function header regex: `(((python(?=(\s|\()))|(fakeroot(?=\s)))\s*)*([\w.\-+{}$:]+)?\s*\(\s*\)\s*{$`.
  Anonymous functions (no name, or `__anonymous`) are always python (`MethodNode.eval`), so
  `fakeroot () {` is anonymous python too.

Statement match order (first match wins): function header, `def`, `EXPORT_FUNCTIONS`,
`addtask`, `deltask`, `addhandler`, `inherit`, `inherit_defer` (BBHandler), then assignment,
`include`, `require`, `include_all`, `export VAR`, `unset VAR`, `unset VAR[flag]`, `addpylib`
(base configuration only), `addfragments` (ConfHandler); anything else is "unparsed line".
Consequences: `include = "x"` is an assignment, `inherit = "x"` inherits `= "x"`. Every
keyword needs whitespace after it (`inherit_deferred = "1"` is an assignment). `.conf` files
use only the ConfHandler part (no functions, `inherit`, `addtask`...); `parse()` does not know
the file kind and applies the full order to every file.

Assignment regex (`__config_regexp__`, reproduced exactly including backtracking order):
`^(export\s+)?([a-zA-Z0-9\-_+.${}/~:]*?)(\[([a-zA-Z0-9\-_+.][a-zA-Z0-9\-_+.@/]*)\])?\s*(:=|\?\?=|\?=|\+=|=\+|=\.|\.=|=)\s*(?!'[^']*'[^']*'$)(?!"[^"]*"[^"]*"$)(['"])(.*)\7$`.
So the value runs to the **last** character of the line, which must be the opening quote
character (`A = "x" # c` is an error; `A = "a" "b"` has value `a" "b`); a value with exactly
three quotes of one kind is rejected; names cannot contain spaces (so no `${@...}` with spaces
in a name); an empty name is an error. `export VAR` and `unset VAR` names use the same
character set without `:`; an `unset` flag must be at least two characters.

### Assignments

```
VAR = "value"          set
VAR ?= "value"         set if VAR is not set at this point of parsing
VAR ??= "value"        weak default: used at the end of parsing if nothing else set it;
                       the last ??= wins
VAR := "value"         set, expanding the value immediately
VAR += "value"         append with a space, at parse time
VAR =+ "value"         prepend with a space, at parse time
VAR .= "value"         append without a space, at parse time
VAR =. "value"         prepend without a space, at parse time
VAR[flag] = "value"    set a flag (all operators allowed)
export VAR = "value"   also export to task environments
export VAR             export an existing variable
unset VAR              / unset VAR[flag]
```

Values are quoted with `"` or `'`. Variable names may contain `${...}` (e.g.
`FILES:${PN}-dev`).

### Overrides

```
VAR:append = " x"      appended when the variable is expanded; NO space is added
VAR:prepend = "x "     prepended when expanded; NO space is added
VAR:remove = "x"       removes whitespace-separated items when expanded; applied last
VAR:foo = "x"          if "foo" is in OVERRIDES, this replaces VAR
VAR:append:foo = " x"  appended to VAR only if "foo" is in OVERRIDES
VAR:foo:append = " x"  appended to the variable VAR:foo (which then replaces VAR if foo is active)
VAR:foo += "x"         operates on VAR:foo at parse time; it does NOT append to VAR
                       (if VAR:foo was unset, VAR:foo becomes " x", leading space included)
```

`OVERRIDES` is a colon-separated list. **Entries to the right have higher priority.** For a
variable with several active override variants, the one whose override is furthest right in
`OVERRIDES` wins. Multiple overrides can be chained (`VAR:a:b`); all must be active.

Finalisation order for a variable: choose the winning override variant (or the plain value),
then apply every active `:append`, then every active `:prepend`, then expand, then apply every
active `:remove`.

Verified (T3.3) against BitBake `master` 2026-11, `lib/bb/data_smart.py` (`getVarFlag`,
`setVar`, `_setvar_update_overrides`, `need_overrides`, `delVar`, `renameVar`) and
`lib/bb/parse/ast.py` (`DataNode.eval`, `finalize`), `lib/bb/data.py` (`expandKeys`). This is
what `src/eval/overrides.rs` and `src/eval/datastore.rs` reproduce:

- **Storage.** A name containing `:append`, `:prepend` or `:remove` is not a variable. The
  regex `(?P<base>.*?)(?P<keyword>:append|:prepend|:remove)(:(?P<add>[^A-Z]*))?$` splits it:
  `VAR:append:a:b = " x"` adds `(" x", "a:b")` to the append list of `VAR`; `VAR:a:append`
  adds `(" x", none)` to the list of `VAR:a`. Any other name with `:` is an ordinary variable
  that is also registered as a *variant* of each shorter name, from the right, while the
  override text starts with `[a-z0-9]` (`VAR:a:b` is a variant of `VAR:a` with `b` and of
  `VAR` with `a:b`; `VAR:Foo:bar` only of `VAR:Foo`; `FILES:${PN}-dev` of nothing until its
  name is expanded). `VAR:a:append` alone also registers `VAR:a` as a variant of `VAR`, so
  with `a` active it replaces `VAR` even though `VAR:a` has no value of its own (the result
  is just the appended text).
- **Variant selection.** A variant is active if its override is in `OVERRIDES`, or, chained,
  every element is. BitBake then runs this loop over the active ones (`active` maps override
  -> variant name, in registration order):

  ```
  while modified:
      for o in OVERRIDES (left to right):
          for a in active: if a ends with ":"+o: rename a to a.replace(":"+o, "")  (modified)
                           elif a == o: match = active[a]; delete a
  ```

  The last match wins: in practice the override furthest right in `OVERRIDES`, and a chained
  variant (`VAR:a:b`) beats its prefix (`VAR:a`) whatever the order of `a` and `b`. The
  winner's value is computed recursively (its own variants, appends, prepends). If it has no
  value at all, the plain value of `VAR` is used. The plain value falls back to the last `??=`.
- **Append / prepend order (the former *verify* item).** Appends first, each active one
  concatenated at the end in the order recorded; then prepends, each active one concatenated
  at the front in the order recorded, so the **last** `:prepend` ends up leftmost. Both work
  on the unexpanded text. An append / prepend condition (`:append:a:b`) needs every element
  in `OVERRIDES`. `:append` on a variable with no value gives just the appended text.
- **Remove.** Only if the expanded value is non-empty: each active `:remove` value is
  expanded and split on whitespace; the value is split on single whitespace characters
  (keeping them) and every word equal to a remove word is dropped, so `"a x b"` minus `x` is
  `"a  b"` (two spaces). A winning variant's own removes are carried to the end, but only
  those that removed something from the variant's own value.
- **`OVERRIDES`** is expanded once with no overrides active, then again with that result.
  If the two differ, BitBake stops ("could not be expanded into a stable state"). An empty
  entry (`a::b`) is kept by BitBake; it only matters for an empty condition element.
- **Parse time.** `=` (and every other operator) during parsing does **not** clear
  appends, prepends, removes or variants (`setVar(..., parsing=True)`); `VAR:append = " x"`
  before `VAR = "a"` still gives `"a x"`. `VAR:foo:append` with `foo` inactive is ignored;
  `VAR:append:foo` with `foo` inactive too.

### Datastore (parse-time operators)

Verified (T3.1) against `ast.py` `DataNode.eval` and `data_smart.py`, `master` 2026-11:

- `?=`, `+=`, `=+`, `.=`, `=.` read the current value of **exactly the name written**
  (`getVar(key, expand=False, noweakdefault=True, parsing=True)`): no overrides, no appends,
  and a `??=` default does not count. So `VAR:foo += "x"` builds on `VAR:foo` only, and
  `A ??= "w"` then `A += "x"` gives `" x"`.
- `+=` / `=+` always insert the space: on an unset variable `A += "x"` stores `" x"` and
  `A =+ "x"` stores `"x "`. `.=` / `=.` add nothing.
- `?=` on a variable that has a value re-stores the same value (no effect).
- `??=` stores a separate default (`_defaultval`); the last one wins; it is used only when the
  variable has no plain value at the end.
- `:=` expands the value immediately against a copy of the current datastore (overrides and
  appends included). Undefined references stay as `${X}` text and can still expand later.
- `VAR:append += "x"` is legal (with a warning) and appends `" x"`: the current value of the
  name `VAR:append` is always unset.
- Flags (`VAR[f] op "v"`) take every operator; they have no overrides or appends of their own.
  `VAR[f] ??= "v"` is a separate flag default. `export VAR = ...` sets the `export` flag.
- `unset VAR` (`delVar`) clears the value, the default, every flag and the append / prepend /
  remove lists of `VAR`, and drops `VAR`'s variant registrations (`VAR:foo` keeps its value
  but no longer replaces `VAR` unless it is assigned again). `unset VAR[f]` removes the flag
  (and its `??=` default, but only if the flag itself was set).
- **Names with `${...}`** (`FILES:${PN}-dev`) are stored as written. At the end of the recipe
  (`ast.finalize`, after deferred inherits, before anonymous python) `expandKeys` expands
  every such name, in sorted order, and renames the variable: its value replaces any value
  already under the expanded name, its append / prepend / remove lists are added to the
  target's, its other flags are dropped.
- **Expansion** (`expandWithRefs`, T3.2): every `${NAME}` with `NAME` matching
  `[a-zA-Z0-9\-_+./~:]+` is replaced by the finalised value of `NAME` (overrides, appends,
  removes applied); undefined references stay as written. Then `${@...}`
  (`\$\{@(?:\{.*?\}|.)+?\}`) is evaluated as python, and the whole pass repeats until the text
  stops changing (so `${${A}}` works). References inside `${@...}` are substituted before the
  python runs. A variable referencing itself is an error; deeper cycles end in a recursion
  error. sous-chef never runs the python (the value is flagged `unevaluated_python`) and
  leaves a reference that closes a cycle in place, listing it in `unresolved`.
- Legacy `_` override names: any name containing `_append`, `_prepend` or `_remove` is fatal;
  others (`RDEPENDS_${PN}`) are just different variables. sous-chef records them in the
  history (status `InactiveOverride`) and does not apply them.

`OVERRIDES` itself is defined in `conf/bitbake.conf`. In OE-Core it is, in shape:

```
${TARGET_OS}:${TRANSLATED_TARGET_ARCH}:pn-${PN}:layer-${FILE_LAYERNAME}:${MACHINEOVERRIDES}:${DISTROOVERRIDES}:${CLASSOVERRIDE}${LIBCOVERRIDE}:forcevariable
```

Do not hard-code this. Read it from the indexed `bitbake.conf` and expand it. Notable
entries: `pn-<recipe>` (per-recipe settings from conf files), `class-target` /
`class-native` / `class-nativesdk` via `CLASSOVERRIDE`, machine and SoC family names via
`MACHINEOVERRIDES`, `forcevariable` (highest).

### Legacy syntax

Before Yocto 3.4 ("honister") overrides were written with `_`: `VAR_append`, `VAR_foo`,
`RDEPENDS_${PN}`. It is no longer accepted. We recognise it only to report it. Detection must
not produce false positives, because underscores are normal in names (`do_install`,
`SRCREV_machine`, `BBFILE_PRIORITY_core`, `LAYERSERIES_COMPAT_core`, `PACKAGECONFIG_CONFARGS`).
Treat a name as legacy when it ends in, or contains as a `_`-separated element,
`append` / `prepend` / `remove`; or when a known package-scoped variable (`FILES`, `RDEPENDS`,
`RRECOMMENDS`, `RSUGGESTS`, `RPROVIDES`, `RCONFLICTS`, `RREPLACES`, `ALLOW_EMPTY`, `SUMMARY`,
`DESCRIPTION`, `pkg_postinst`, `pkg_prerm`, ...) is followed by `_${PN}` or `_<package name>`;
or when the suffix is a known override (a machine, distro or class override from the index).

What BitBake itself does (`data_smart.py` `setVar`, master 2026-11): any variable name that
contains the substring `_append`, `_prepend` or `_remove` (except `__anon_*`) is fatal ("contains
an operation using the old override syntax"). Other legacy forms (`RDEPENDS_${PN}`,
`VAR_qemux86`) are silently a *different variable* in modern BitBake.

What the parser implements (`split_var` in `src/parse/parser.rs`): it splits the part before
the first `:` on `_` outside `${...}` and takes the first split point `i >= 1` where element
`i` is `append` / `prepend` / `remove`, or where the first `i` elements are a package-scoped
variable (`FILES`, `RDEPENDS`, `RRECOMMENDS`, `RSUGGESTS`, `RPROVIDES`, `RCONFLICTS`,
`RREPLACES`, `ALLOW_EMPTY`, `SUMMARY`, `DESCRIPTION`, `SECTION`, `LICENSE`, `CONFFILES`,
`INSANE_SKIP`, `PKG`, `pkg_preinst`, `pkg_postinst`, `pkg_postinst_ontarget`, `pkg_prerm`,
`pkg_postrm`; the longest matching name wins) and element `i` starts with `${` or a lowercase
letter (OE package names are lowercase; `FILES_SOLIBSDEV` is a real variable). Then `base` is
the text before that point and the remaining `_` elements are the overrides. The third rule
(suffix is a known machine/distro/class override) needs the index and is **not** applied by
the parser; lint does it.

### Functions

```
do_install() {                       shell function / task
    ...
}
do_install:append() { ... }          override syntax applies to function names
python do_foo() { ... }              python function / task
python () { ... }                    anonymous python, run after parsing
python __anonymous() { ... }         same
fakeroot do_bar() { ... }            run under pseudo
fakeroot python do_baz() { ... }
def helper(d):                       top-level python def, body is the indented block
    ...
```

A shell or `python` function body ends at the first line that is exactly `}` in column 0
(trailing whitespace after the `}` is allowed; see "Lines").

### Directives

```
inherit a b c                        inherit classes (names may contain ${VAR})
inherit_defer a                      like inherit, evaluated at the end of parsing
require path/file.inc                include; error if not found
include path/file.inc                include; silently ignored if not found
include_all path/file.inc            include path/file.inc from every BBPATH entry
addtask do_x after do_a before do_b  "do_" optional; several names allowed
deltask do_x
addhandler name
EXPORT_FUNCTIONS do_x do_y           in class foo: do_x defaults to calling foo_do_x
```

Verified (T1.2) against BitBake `master` 2026-11 and the release branches on GitHub
(`openembedded/bitbake`), plus the BitBake User Manual
(`doc/bitbake-user-manual/bitbake-user-manual-metadata.rst`, "inherit_defer Directive",
"include_all Directive"):

- `inherit_defer` (`inherit_defer\s+(.+)`): present in bitbake 2.8 (Yocto 5.0 scarthgap),
  absent in 2.6. The text is stored unexpanded and inherited at the end of parsing, so
  `${VAR}` / `${@...}` see the final values; an empty expansion is a no-op. Classes listed in
  `BB_DEFER_BBCLASSES` are deferred even when written with plain `inherit`. The argument is
  everything after the keyword, `#` included (no comment stripping), for both `inherit` forms.
- `include_all` (`include_all\s+(.+)`, ConfHandler): present in bitbake 2.12 (Yocto 5.2
  walnascar), absent in 2.10. It includes `<entry>/<path>` for every distinct `BBPATH` entry,
  left to right; the including file's directory is **not** searched; missing files are not an
  error.
- `addtask` (`addtask\s+([^#\n]+)`; a `#` ends the arguments): the text is split on the
  literal strings `" before "` and `" after "` (single spaces; a tab does not separate). The
  task names are the text before the first keyword and may be several words (each is added).
  `before` / `after` may come in any order and be repeated; quirk: two `after` (or `before`)
  groups separated by the other keyword are concatenated **without a space**
  (`addtask x after a before b after c` gives after = `ac`); the parser reproduces this and
  emits a diagnostic. `do_` is prepended to the task and every dependency that lacks it
  (`bb.build.ensure_task_prefix`) when the task is added; the AST keeps names as written.
- `deltask` (`deltask\s+([^#\n]+)`) and `addhandler` (`addhandler\s+(.+)`) take several
  space-separated names; the AST `name` field holds them joined with single spaces.
  `deltask` expands its argument; `addhandler` does not.
- `EXPORT_FUNCTIONS` takes space-separated names.

## Finding files

- `BBPATH` is the search path: by convention each `layer.conf` does `BBPATH .= ":${LAYERDIR}"`,
  so it is the layer roots in `BBLAYERS` order (the build directory comes first).
- `require` / `include`: an absolute path is used as is. A relative path is tried relative to
  the including file's directory, then against each `BBPATH` entry.
- Classes: `inherit foo` looks for `classes-recipe/foo.bbclass` then `classes/foo.bbclass` on
  `BBPATH`. Global `INHERIT` looks for `classes-global/foo.bbclass` then `classes/foo.bbclass`.
  The split directories exist since Yocto 4.1; older layers only have `classes/`.
- `base.bbclass` is always inherited globally.
- Recipes and appends: the `BBFILES` globs set in each `layer.conf`, typically
  `${LAYERDIR}/recipes-*/*/*.bb` and `.bbappend`.

Verified (T1.4) against BitBake `master` (commit `5495a547`, 2026-11-16), and reproduced by
`src/workspace/resolve.rs`:

- **`include` / `require` search order** (`parse/parse_py/ConfHandler.py`
  `include_single_file`, which the `IncludeNode` of every file kind calls): the argument is
  expanded and split on whitespace, each word is one file. A file whose name equals the
  including file's name is skipped. A relative name is looked up with `bb.utils.which` along
  `"<dirname of the including file>:" + BBPATH`: the including file's directory first, then
  each `BBPATH` entry left to right; the first path that **exists** (`os.path.exists`, so a
  directory also stops the search, and then fails to parse) wins. Not found: `include` is
  silent, `require` is a parse error ("Could not include required file").
- **Class search order** (`parse/parse_py/BBHandler.py` `inherit`): the whole `BBPATH` is
  searched for `classes-<type>/foo.bbclass` before any entry is searched for
  `classes/foo.bbclass`. So `classes-recipe/foo.bbclass` in the *last* layer beats
  `classes/foo.bbclass` in the first. `<type>` is the datastore's `__bbclasstype`: `global`
  while the base configuration is parsed (so for `INHERIT` and for any `inherit` inside a
  class reached from it), `recipe` for recipes (`cookerdata.py`). A name ending in `.bbclass`
  is looked up on `BBPATH` as given; an absolute name is used as is. The including file's
  directory is not searched. Not found is a parse error. A class already inherited
  (`__inherit_cache`) is not parsed again.
- **Global classes** (`cookerdata.py`): the configuration inherits `["base"] + INHERIT`, in that
  order, after the configuration files, each with `__bbclasstype = "global"`.
- **bbappend matching** (`cooker.py` `get_file_appends`): with `b` the append's file name with
  `.bbappend` replaced by `.bb` and `f` the recipe's file name, the append applies when
  `b == f`, or when `b` contains `%` and `b.startswith(f[:b.index('%')])`. Only file names
  are compared, never directories.
- **`BBFILES` expansion** (`cooker.py` `collect_bbfiles`): the words of `BBFILES` are sorted
  (stably) by the priority of the collection whose `BBFILE_PATTERN` matches the glob text,
  lowest first; each word is used as is when the path exists, otherwise expanded with
  `glob.glob` (not recursive; `*` does not match a leading `.`) and sorted; a directory match
  is walked for `.bb` / `.bbappend`; duplicates are dropped (first kept); `BBMASK` regexes
  are applied afterwards. The `.bbappend` files are kept in this order, which is the order
  `get_file_appends` returns them in, so appends apply **lowest layer priority first** (the
  highest priority layer has the last word), ties in `BBFILES` order.
- **`BBPATH`** is whatever the configuration leaves in the variable: OE layers usually do
  `BBPATH .= ":${LAYERDIR}"` but some prepend (`BBPATH =. "${LAYERDIR}:"`), so sous-chef reads
  the value from `bblayers.conf` and the layer confs instead of assuming the layer order
  (`layers::read_bbpath`). Changes made later in `bitbake.conf` are not seen.

sous-chef differences: only files inside the workspace (layer roots and the build directory)
are ever found; `BBMASK` is not applied by `recipe_files`.

Static expansion of reference arguments (`resolve::expand_with`): the scanner resolves
`${VAR}` in `require` / `include` / `inherit` arguments only for values that follow from the
file's own path. From OE-Core `meta/conf/bitbake.conf` and `meta/conf/layer.conf` (master
2026-11): `PN` and `PV` come from the recipe file name (`bb.parse.vars_from_file`: split on
`_`, at most three parts, `PV` defaults to `1.0`), `BPN` is `PN` with the
`SPECIAL_PKGSUFFIX` suffixes (`-native -cross -initial -intermediate -crosssdk
-cross-canadian`) pruned (plus `MLPREFIX`, which is empty for a plain recipe), `BP` is
`${BPN}-${PV}`, `THISDIR` is the directory of the file being parsed, and `COREBASE` is
`os.path.normpath("${LAYERDIR}/../")` of OE-Core's layer (collection `core`). Anything else
is left to the evaluator.

## Layers

`conf/bblayers.conf` in the build directory sets `BBLAYERS` (a whitespace-separated list of
layer roots; continuation lines and `${TOPDIR}`-style variables are common).

Each layer has `conf/layer.conf`. `${LAYERDIR}` is the layer root. Relevant variables (these
legitimately use `_<collection>` suffixes):

| Variable | Meaning |
|---|---|
| `BBFILE_COLLECTIONS` | Collection name(s) added by this layer |
| `BBFILE_PATTERN_<c>` | Regex identifying the layer's files |
| `BBFILE_PRIORITY_<c>` | Layer priority; higher wins |
| `LAYERDEPENDS_<c>` | Collections this layer needs |
| `LAYERSERIES_COMPAT_<c>` | Release names it supports |
| `LAYERVERSION_<c>` | Version |
| `BBFILES` | Globs for recipes and appends |

Layer priority decides which recipe wins when two layers provide the same `PN` and version,
and the order in which bbappends are applied (lower priority first, so the highest priority
layer has the last word). Verified in the BitBake source (T1.4, see "Finding files"); not
checked against `bitbake-layers show-appends` output.

Checked by T1.3 against BitBake master (`lib/bb/cookerdata.py`, `cooker.py`,
`parse/ast.py`):

- **Finding `bblayers.conf`:** `findConfigFile` looks in `$BBPATH/conf/` first, then walks up
  from the working directory, stopping before `/`. `TOPDIR` is the directory that contains
  `conf/bblayers.conf`. (sous-chef does not consult the `BBPATH` environment variable, and
  does check `/`.)
- **`BBLAYERS` entries:** the value is expanded and split on whitespace; a trailing `/` is
  stripped; entries are not passed through realpath. A missing directory is fatal, and so is
  an empty `BBLAYERS`.
- **`LAYERDIR`:** set per layer; after each `layer.conf` every `${LAYERDIR}` reference in the
  datastore is replaced (`expandVarref`). One datastore is shared by all layer confs.
- **Priority:** `BBFILE_PRIORITY_<c>` must be an integer. An unset or `0` priority becomes
  the highest priority among the layer's dependencies plus one, starting from the lowest
  explicit priority. `LAYERDEPENDS` uses the `explode_dep_versions2` format.
- **Weak defaults:** `?=`, `+=`, `.=` and the other operators read the current value with
  `noweakdefault=True`, so they ignore a `??=` default. The last `??=` wins.

- **`:append` / `:prepend` / `:remove`** (`data_smart.py` `setVar`, `getVarFlag`, checked by
  T1.4): a name matching `(?P<base>.*?)(:append|:prepend|:remove)(:(?P<add>[^A-Z]*))?$` is
  stored as a pending operation on `base` with the condition `add`; parse-time operators
  (`+=`, `?=`...) never see it. On read, the winning override variant (or the value, or the
  `??=` default) gets every active append, then every active prepend; removes apply to the
  expanded value and keep the surrounding whitespace. A plain `setVar` outside parsing (as
  done by `expandVarref('LAYERDIR')`) folds the pending appends into the value and drops them,
  so after each `layer.conf` an `X:append = " ${LAYERDIR}/..."` becomes part of `X`.
  `LAYERDIR_RE` (`re.escape` of the layer path) is set alongside `LAYERDIR`; both are deleted
  after the last layer.

Not reproduced by sous-chef: the fatal `LAYERSERIES_COMPAT` and `LAYERDEPENDS` validation.

## Parse order

Configuration, once:

1. `conf/bblayers.conf`
2. every layer's `conf/layer.conf`, in `BBLAYERS` order
3. `conf/bitbake.conf` (found on `BBPATH`), which itself includes, in the order written in
   that file: `conf/site.conf`, `conf/auto.conf`, `conf/local.conf`, the machine conf
   `conf/machine/${MACHINE}.conf`, the distro conf `conf/distro/${DISTRO}.conf`, and others.
   **Follow the includes as written; do not hard-code the list.**
4. classes named in `INHERIT`, plus `base.bbclass`

Then for each recipe, on top of a copy of the configuration:

5. the `.bb`, statement by statement, following `require` / `include` / `inherit` at the
   point where they appear
6. each matching `.bbappend`, in layer priority order
7. deferred inherits, anonymous python (not evaluated by us), then finalisation

## Providers and versions

- A recipe provides its `PN` and everything in `PROVIDES`.
- `PREFERRED_PROVIDER_<name>` picks among providers of e.g. `virtual/kernel`.
- `PREFERRED_VERSION_<pn>` picks a version (may use `%`). Otherwise the highest version in the
  highest-priority layer wins; `DEFAULT_PREFERENCE = "-1"` lowers a recipe.
- `BBMASK` hides files; `SKIP_RECIPE` / `COMPATIBLE_MACHINE` / `COMPATIBLE_HOST` skip recipes.

## Files and patches

`file://` entries in `SRC_URI` are searched along `FILESPATH`, built from `FILESEXTRAPATHS`
and the directories `${BP}`, `${BPN}` and `files` next to the recipe, each also tried with
override subdirectories. A bbappend that adds files must do:

```
FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"
```

(immediate expansion and the trailing colon both matter).

## Build output (for M5)

- Task logs: `${WORKDIR}/temp/log.do_<task>` (a symlink to `log.do_<task>.<pid>`) and the
  generated script `run.do_<task>`. `WORKDIR` is under `tmp/work/<arch>/<pn>/<pv>/`
  (*verify* per release; the layout changed over time).
- Cooker logs: `tmp/log/cooker/<machine>/`.
- `bitbake -e <recipe>` prints, for each variable, a comment block with its history followed
  by the final assignment:

  ```
  # $VAR [3 operations]
  #   set /path/to/file.conf:12
  #     "value"
  #   append /path/to/recipe.bb:30
  #     "more"
  # pre-expansion value:
  #   "value more"
  VAR="value more"
  ```

  *Verify* the exact format against real output before writing the parser.

## What the static evaluator cannot do

- `${@python expression}` in values
- anonymous python functions and event handlers
- `d.setVar` / `d.appendVar` inside classes (e.g. `PACKAGECONFIG` processing in `base.bbclass`)
- `inherit` / `require` whose argument depends on any of the above

These are reported, never silently guessed. Authoritative mode (M5) covers them.

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
- `inherit_defer` **before bitbake 2.8** (verified, w3 cleanup, against
  `lib/bb/parse/parse_py/BBHandler.py` and `ConfHandler.py` on branch `2.0` (Kirkstone,
  head `a9f09b9e`) and branch `2.6`): there is no such directive. `BBHandler.feeder` tries
  `__inherit_regexp__ = inherit\s+(.+)` with `re.match`, which needs whitespace right after
  `inherit` and so does not match `inherit_defer x`; no other BBHandler pattern matches, and
  the line goes to `ConfHandler.feeder`, where it is not an assignment, `include`, `require`,
  `export` or `unset`, so it raises `ParseError("unparsed line: 'inherit_defer x'")`. The file
  fails to parse. The replay records such a line as a reference issue
  (`RefProblem::NotInRelease(InheritDefer)`), inherits nothing for it and goes on with the
  rest of the file. Lint reports the directive to the user: `hygiene/release-syntax`, for
  a layer checked against a release without it.
- `BB_DEFER_BBCLASSES` is newer still: absent from `BBHandler.py` on branches `2.8`, `2.10`
  and `2.12`, present on `master` (2026-11). The first release that has it is not yet
  recorded (*verify* 2.14 / 2.16); the replay currently honours it on every release.
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

**A bbappend that matches no recipe** (checked by T4.3 in `cooker.py`
`show_appends_with_no_recipes`, master and branches `2.0` and `2.8`): BitBake stops with a
fatal error ("No recipes available for"). On BitBake 2.0 (Kirkstone) and 2.8 (Scarthgap)
`BB_DANGLINGAPPENDS_WARNONLY` set makes it a warning; master has no such variable. The
release that dropped it was not checked (*verify*). The `layers` tool (`show: appends`)
and `org/append-no-match` report these appends.

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
layer has the last word). Verified in the BitBake source (T1.4, see "Finding files"; again
by T3.4, `multi_finalize` parses `__BBAPPEND` in `get_file_appends` order); not checked
against `bitbake-layers show-appends` output.

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

**Layer validation** (checked by T4.3 in `lib/bb/cookerdata.py` `parseConfigurationFiles`
and `lib/bb/cooker.py` `handleCollections`, master and branches `2.0` and `2.8`, which agree):

- `LAYERSERIES_COMPAT_<c>` not set: a warning on every run ("should set
  LAYERSERIES_COMPAT_<c>"); the layer is accepted.
- `LAYERSERIES_COMPAT_<c>` with no name in common with `LAYERSERIES_CORENAMES`: fatal
  ("Layer <c> is not compatible with the core layer"). Only names are compared.
- The same collection added by two layers: fatal ("Found duplicated BBFILE_COLLECTIONS").
- A `LAYERDEPENDS_<c>` entry that no layer adds: an error that stops BitBake ("depends on
  layer '<d>', but this layer is not enabled in your configuration").

The replay does not reproduce these checks (it never stops on them); `org/layer-conf`
reports them.

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

### Replay (verified, T3.4)

Checked against BitBake `master` 2026-11 (`lib/bb/cookerdata.py` `parseConfigurationFiles`,
`_parse_recipe`; `lib/bb/cooker.py` `collect_bbfiles`, `get_file_appends`,
`CookerCollectFiles.get_layername`; `parse/parse_py/ConfHandler.py` `handle`,
`include_single_file`; `parse/parse_py/BBHandler.py` `handle`, `inherit`; `parse/ast.py`
`MethodNode`, `PythonMethodNode`, `ExportFuncsNode`, `multi_finalize`, `finalize`;
`data_smart.py` `expandVarref`) and OE-Core `master` (`meta/conf/bitbake.conf`,
`meta/classes-global/base.bbclass`). This is what `src/eval/replay.rs` reproduces:

- **Configuration order.** `TOPDIR` = the directory above `conf/bblayers.conf`; parse
  `bblayers.conf`; for each `BBLAYERS` entry (trailing `/` stripped, *not* normalised): set
  `LAYERDIR` and `LAYERDIR_RE` (`re.escape`), parse `<layer>/conf/layer.conf`, then
  `expandVarref('LAYERDIR')` and `expandVarref('LAYERDIR_RE')` (every variable whose
  unexpanded finalised value contains `${LAYERDIR}` gets it replaced and is stored again with a
  plain `setVar`); after the last layer both are deleted. Then `conf/bitbake.conf`, found
  along `BBPATH` only, its includes followed as written. Then the classes
  `["base"] + INHERIT.split()`, **`base` first**, each through `BBHandler.inherit` with
  `__bbclasstype = "global"`. (Also done by BitBake and not reproduced: `LAYERSERIES_COMPAT_*`
  and `LAYERSERIES_CORENAMES` are deleted and set again in sorted form; `BBFILES_DYNAMIC`;
  the duplicate-`BBPATH` check.)
- **`FILE`.** ConfHandler sets `FILE` to every `.conf` it parses and restores the previous
  value afterwards if there was one. BBHandler sets it to every `.bb`, `.inc` and `.bbappend`
  (not `.bbclass`) and restores it afterwards when parsed as an include; the top-level
  recipe is parsed with `include == 0` and leaves `FILE` set to the recipe.
- **`PN`, `PV`, `BPN`, `THISDIR`, `FILE_DIRNAME`** are python in OE-Core:
  `PN` / `PV` = `bb.parse.vars_from_file(FILE)` parts 0 / 1, else `defaultpkgname` / `1.0`
  (`vars_from_file` only splits names ending in `.bb` / `.bbappend`, so inside a `.inc` `PN`
  is `defaultpkgname`, and inside `hello_%.bbappend` `PV` is `%`); `BPN` =
  `oe.utils.prune_suffix(PN, SPECIAL_PKGSUFFIX)` (then `MLPREFIX` stripped);
  `FILE_DIRNAME` (bitbake.conf) and `THISDIR` (base.bbclass, "only works properly with
  immediate expansion") = `dirname(FILE)`. `BP = "${BPN}-${PV}"` is plain text.
- **`FILE_LAYERNAME`** is set with `setVar` before the recipe is parsed (so it beats
  OE-Core's `FILE_LAYERNAME ??= "config"`). Its value is the **collection name** (not the
  layer directory name) of the first `BBFILE_PATTERN_<c>` regex that matches the recipe path
  (`re.match`), patterns tried in reverse lexical order of their text.
- **bbappends** are parsed after the whole `.bb` (`multi_finalize`, in `__BBAPPEND` order
  = `get_file_appends` = `BBFILES` collection order: lowest layer priority first), each with
  BBHandler as an include. Confirmed in the source; not checked against
  `bitbake-layers show-appends` output.
- **Finalisation** (`ast.finalize`): deferred inherits (`__BBDEFINHERITS`, first in first
  out, including ones added meanwhile), event handler registration, `expandKeys`, then
  anonymous python (`runAnonFuncs`), then task processing.
- **Inherit** (`BBHandler.inherit`): the argument is expanded and split; a word listed in
  `BB_DEFER_BBCLASSES` is deferred (unless already deferred); a class file in
  `__inherit_cache` is not parsed again. Not found is a parse error.
- **Include** (`include_single_file`): skipped only when the written name equals the
  including file's path text, so `require self.inc` inside `self.inc` recurses (BitBake
  fails eventually).
- **Functions** (`MethodNode.eval`): stored with `setVar(name, body, parsing=True)`, so
  override syntax in the name applies; if the name already has a value its `python` and
  `fakeroot` flags are cleared first; then flags `python`, `fakeroot`, `func`, `filename`,
  `lineno`. Anonymous python becomes `__anon_<line>_<file with /.+-@%&~ replaced by _>`
  and is appended to `__BBANONFUNCS`. A top-level `def` (`PythonMethodNode`) only sets the
  `func` and `python` flags; its body goes to the method pool, not the datastore.
- **`EXPORT_FUNCTIONS`** (`ExportFuncsNode`) in class `c`: for each `f`, skipped if `f`
  already has a value that is not an earlier stub; otherwise `f` gets the `func` / `python`
  flags of `c_f`, `c_f` gets `f`'s `dirs` / `cleandirs` / `fakeroot`, and `f` becomes a stub
  calling `c_f`. A later class's stub replaces an earlier one.

sous-chef differences (in `replay.rs`): `LAYERDIR` is the canonical layer root, not the
`BBLAYERS` text. `expandVarref` is done by replacing the reference inside each stored piece
(value, `??=` default, `:append` / `:prepend` / `:remove` texts) rather than folding the
appends into the value; final values are the same except where an override variant would
have been detached. `PN`, `PV`, `BPN`, `FILE_DIRNAME` and `THISDIR` are computed as above
only in the recipe pass and only while their stored value is python or absent (the python
is assumed to be OE-Core's); `BP` is set to `${BPN}-${PV}` when absent. Target `MACHINE` /
`DISTRO` are set before `bblayers.conf` and again after every configuration assignment to
them. A function's stored value is a pointer per piece instead of the body. A reference
that needs python or an undefined variable is not followed (BitBake would expand the
python, or look for a file with `${X}` in its name) and is recorded; `include` of a missing
file is silent, as in BitBake; a missing `require` / class and an include cycle are recorded
instead of failing the parse; so is an `inherit_defer` on a release without it (see
"Directives"), which BitBake rejects as an unparsed line. `addtask`, `deltask` and `addhandler` do not change values and
are not replayed. Anonymous python and event handlers are never run; the variables an
anonymous function changes by a literal name are flagged `unevaluated_python`.

## Providers and versions

- A recipe provides its `PN` and everything in `PROVIDES`.
- `PREFERRED_PROVIDER_<name>` picks among providers of e.g. `virtual/kernel`.
- `PREFERRED_VERSION_<pn>` picks a version (may use `%`). Otherwise the highest version in the
  highest-priority layer wins; `DEFAULT_PREFERENCE = "-1"` lowers a recipe.
- `BBMASK` hides files; `SKIP_RECIPE` / `COMPATIBLE_MACHINE` / `COMPATIBLE_HOST` skip recipes.

Verified (T3.7) against BitBake `master` 2026-11, `lib/bb/providers.py`
(`versionVariableMatch`, `preferredVersionMatch`, `sortPriorities`,
`findPreferredProvider`, `findLatestProvider`, `findBestProvider`, `_filterProviders`,
`filterProviders`). This is what `src/eval/providers.rs` reproduces:

- **Version variables** are read from the configuration datastore, first set wins:
  `<K>_VERSION:pn-<pn>`, then `<K>_VERSION_<pn>`, then `<K>_VERSION` (`OVERRIDES` are not
  used for the `pn`, because a `pn` may contain `_`). `K` is `REQUIRED` (takes precedence;
  no match is an error, no provider) and `PREFERRED` (no match: a note, then the latest).
- **Matching** (`preferredVersionMatch`): the value is split with
  `(\d+:)*(.*)(_.*)*`: a leading `N:` is the epoch, compared with `PE`; the middle group is
  greedy, so a `_rN` suffix stays in the version and the revision never takes part. The
  version matches when equal to `PV`, or when it ends in `%` and `PV` starts with the rest.
- **Order of files of one `PN`** (`sortPriorities`): buckets by layer priority, highest
  first; inside a bucket by `DEFAULT_PREFERENCE` ascending, then the order the files were
  collected. `findPreferredProvider` takes the first match in that order, bucket by bucket,
  so with a pattern such as `1.%` a file with a *lower* `DEFAULT_PREFERENCE` in the same
  layer priority is found first.
- **Latest** (`findLatestProvider`) looks only at the highest priority bucket: a file
  replaces the current pick when its `DEFAULT_PREFERENCE` is higher, or equal with a higher
  `(PE, PV, PR)` (`bb.utils.vercmp`: epoch as an integer, then `PV`, then `PR`). So
  `DEFAULT_PREFERENCE` outranks the version but never a layer priority.
- **Providers of a name** (`_filterProviders`): every file that provides it (`PN` or a
  `PROVIDES` word), grouped by `PN` in sorted order; per `PN` the preferred-version file
  (if a `PREFERRED_VERSION` matches) goes to a first list, the latest file of the others to
  a second; the eligible list is the first list then the second; a `PN` equal to the name
  is moved to the front. `filterProviders` then reads `PREFERRED_PROVIDER_<name>` (e.g.
  `PREFERRED_PROVIDER_virtual/foo`) and, if an eligible file has that `PN`, moves it to the
  front and marks the choice unique. There is no `PREFERRED_PROVIDERS` variable. Without a
  unique choice and with several eligible files BitBake fires `MultipleProviders` and uses
  the first one (`taskdata.py`: *verify*, not read for this task).

sous-chef differences: the candidates are the recipes whose `PN` or static `PROVIDES` (also
in an include, class or bbappend of the recipe) names the item in the index, confirmed by
evaluating each; a `PROVIDES` word that needs expansion, or one from a global class, is not
found. The file order inside a bucket is layer then path, not BitBake's collection order.
`DEFAULT_PREFERENCE`, `PE` and `PR` come from evaluating each candidate only when some
non-`.conf` file (or a `.conf` statement with an override) sets one of them; values set by
anonymous python are not seen. `BBMASK`, `SKIP_RECIPE` and `COMPATIBLE_*` are not applied,
and `BBCLASSEXTEND` variants (`-native`, `nativesdk-`) are not providers.

*Verify* (used by `org/policy-in-recipe`, not checked against the source by any task):
`PREFERRED_PROVIDER_*` is read only from the configuration datastore (`cooker.py`
`handlePrefProviders`, `providers.findBestProvider`), so a recipe's own value never takes
part in the choice.

### `PACKAGECONFIG` (verified, T3.7)

Checked against OE-Core `master` 2026-11 `meta/classes-global/base.bbclass` (anonymous
python) and the Yocto reference manual (`PACKAGECONFIG`, 6.1): `PACKAGECONFIG[f] =
"a,b,c,d,e,g"`, up to six comma-separated fields (more is a `bb.error`): 1 extra configure
arguments (`PACKAGECONFIG_CONFARGS`) when `f` is in `PACKAGECONFIG`, 2 the same when it is
not, 3 build dependencies (`DEPENDS`), 4 runtime dependencies (`RDEPENDS:${PN}`), 5 runtime
recommendations (`RRECOMMENDS:${PN}`), 6 conflicting options (an enabled conflict is fatal).
Flags are iterated in sorted order; a word of `PACKAGECONFIG` with no flag is ignored; no
flag name is special. The additions are made by anonymous python, which sous-chef does not
run: `deps` lists them from the flags and says so.

## Files and patches

`file://` entries in `SRC_URI` are searched along `FILESPATH`, built from `FILESEXTRAPATHS`
and the directories `${BP}`, `${BPN}` and `files` next to the recipe, each also tried with
override subdirectories. A bbappend that adds files must do:

```
FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"
```

(immediate expansion and the trailing colon both matter).

## Build output (for M5)

- Task logs: `${T}/log.do_<task>.<pid>` with a symlink `log.do_<task>` to the latest run;
  the generated script is `${T}/run.do_<task>.<pid>` with a symlink `run.do_<task>`
  (`BB_LOGFMT` / `BB_RUNFMT` defaults in bitbake `lib/bb/build.py`). `${T}/log.task_order`
  lists the runs. `T = "${WORKDIR}/temp"` and
  `WORKDIR = "${BASE_WORKDIR}/${MULTIMACH_TARGET_SYS}/${PN}/${PV}"` with
  `BASE_WORKDIR ?= "${TMPDIR}/work"` and `TMPDIR ?= "${TOPDIR}/tmp"` in OE-core
  `meta/conf/bitbake.conf` on scarthgap and master; kirkstone has
  `.../${PN}/${EXTENDPE}${PV}-${PR}`. Older releases also had `TMPDIR` names like
  `tmp-glibc` (`TCLIBCAPPEND`, now removed). All of these can be overridden, so ask
  `bitbake-getvar -r <pn> --value T` when the environment is available. (Checked:
  openembedded-core 9518ce9 of 2026-11-16, and the `kirkstone` and `scarthgap` branches'
  `bitbake.conf`.)
- Cooker logs: `BB_CONSOLELOG ?= "${LOG_DIR}/cooker/${MACHINE}/${DATETIME}.log"`, i.e.
  `tmp/log/cooker/<machine>/<datetime>.log` (OE-core `bitbake.conf`).
- Failure text, as printed by bitbake 5495a54 (2026-11-16) and OE-core 9518ce9:
  - Task log lines are `LEVEL: message` (`DEBUG`, `NOTE`, `WARNING`, `ERROR`; critical
    also prints as `ERROR`). On the console, knotty prefixes messages from a task with
    `<PF> <task>: ` (`ERROR: hello-1.0-r0 do_compile: ...`).
  - knotty prints `ERROR: Logfile of failure stored in: <path>`, then the log under
    `Log data follows:` with `| ` (or ` | ` when `BBINCLUDELOGS_LINES` is set) prefixes,
    but only when `BBINCLUDELOGS` is set and no `ERROR` was logged during the task. Then
    `ERROR: Task (<recipe file>:<task>) failed with exit code '<n>'` (runqueue), and at
    the end `Summary: N task(s) failed:` with `  <file>:<task>` and `    log: <path>`.
  - A failing shell task: `ERROR: Execution of '<run file>' failed with exit code <n>`
    goes to the console only (the task log handler is removed first). Inside the log, the
    exit trap in the run script prints, under bash,
    `WARNING: <run file>:<line> exit <n> from '<command>'` and
    `WARNING: Backtrace (BB generated script): ` with `\t#<i>: <function>, <file>, line
    <n>` frames, innermost first, ending in `main`; under a non-bash `/bin/sh` only
    `WARNING: exit code <n> from a shell command.`. The run file has a
    `# line: <n>, file: <metadata file>` comment above each function; build.py maps a
    run-file line `L` in a function whose comment is at 0-based index `i` to metadata line
    `n + L - i - 2`. build.py can also add `Backtrace (metadata-relative locations):`, but
    only from the exception's captured stdout, which is `None` when the output is logged,
    so it is not expected in task logs (not seen in a real log).
  - OE's `bbfatal_log` / `die` print `ERROR: <message>` (e.g. `oe_runmake failed`,
    `configure failed`).
  - A failing python task: `ERROR: Error executing a python function in exec_func_python()
    autogenerated:`, then `The stack trace of python calls that resulted in this
    exception/failure was:`, frames `File: '<file>', lineno: <n>, function: <f>` each
    followed by source lines (` *** NNNN:` marks the line), then
    `Exception: <Type>: <message>` (`bb.utils._print_exception`).
  - Fetch: `bb.fatal("Bitbake Fetcher Error: " + repr(e))` in `base_do_fetch`, e.g.
    `ERROR: Bitbake Fetcher Error: FetchError('Unable to fetch URL from any source.', '<url>')`;
    `FetchError` text is `Fetcher failure for URL: '<url>'. <msg>` or
    `Fetcher failure: <msg>`.
  - Patch: `NOTE: Applying patch '<name>' (<path>)`, then on failure
    `ERROR: Applying patch '<file>' on target directory '<dir>'` followed by the repr of
    the patch tool's `CmdError` (quilt's output with `\n` turned into newlines).
  - QA: `ERROR: QA Issue: <message> [<class>]` (or `WARNING:`), then
    `ERROR: Fatal QA errors were found, failing task.` (`meta/lib/oe/qa.py`).
  - The compiler, configure, make, wget and quilt messages inside logs are those tools'
    own formats; they were not checked against a source.
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

## Running BitBake (for T5.4)

Checked 2026-11-17 against bitbake `master` (`lib/bb/main.py`, `lib/bb/cooker.py`,
`lib/bblayers/query.py`) and OE-core `master` (`scripts/lib/devtool/standard.py`,
`scripts/lib/devtool/build.py`); the devtool positionals were also checked on OE-core
`kirkstone` (`standard.py`), where they are the same.

- `bitbake` options: `-k` / `--continue` (`dest="halt"`, store_false: "Continue as much as
  possible after an error"), `-f` / `--force` ("Force the specified targets/task to run
  (invalidating any existing stamp file)"), `-c` / `--cmd` (the task, e.g. `compile`),
  `-e` / `--environment` ("Show the global or per-recipe environment"). The cooker adds
  `do_` to a task that lacks it (`buildTargets`: `if not task.startswith("do_"): task =
  "do_%s" % task`), so `-c compile` and `-c do_compile` are the same; the tool passes the
  short form.
- `bitbake-layers` (`lib/bblayers/query.py`) registers `show-layers` (no arguments),
  `show-overlayed` (options `-f`, `-s`, `--mc` only), `show-recipes` (`pnspec`,
  `nargs='*'`, optional; options `-f -r -m -i -l -b --mc --show-variants`), `show-appends`
  (`pnspec`, `nargs='*'`, optional; `--mc`) and `show-cross-depends`. The tool passes at most
  one `pnspec`, and only to `show-recipes` and `show-appends`.
- `devtool`: `modify <recipename> [srctree]` (`srctree` is `nargs='?'`; the tool never
  passes it, so devtool uses its default under the workspace), `status` (no positionals),
  `reset [recipename...]` (`nargs='*'`; with none it requires `-a`/`--all`, which the tool
  does not offer, so the tool requires a target), `finish <recipename> <destination>`
  (both required; destination is "Layer/path to put recipe into", which may be a layer
  name from `bblayers.conf`; the tool accepts only the name of a writable workspace
  layer), `build <recipename>` (`build.py`; required; options `-s`, `-c` not offered).
- *Verify* (not checked): whether the BitBake server process started by a job leaves the
  job's process group (it daemonises) and so survives a cancel, which terminates only the
  group; and whether a second `bitbake` client (`-e`, `bitbake-layers`) waits for or
  fails on the lock while a build runs. The tool avoids the second question by running
  one job at a time.

## Releases

Added by T1.6 (2026-11-17). Wrynose (6.0 LTS) is the primary target, Scarthgap (5.0 LTS) is
fully supported, Kirkstone (4.0 LTS) is the floor; `src/release.rs` holds the table below as
`Feature::first()` / `Feature::last()`. A row marked *verify* has `Feature::verified()` false
and must not be used to report an error. No row is marked *verify* at present.

| `Feature` | Behaviour | First | Last | Source checked |
|---|---|---|---|---|
| `SplitClassDirs` | `inherit` searches `classes-<type>/` along `BBPATH` before `classes/` | 4.1 langdale (BitBake 2.2) | (current) | Yocto migration guide 4.1, "Classes now split by usage context"; BitBake `lib/bb/parse/parse_py/BBHandler.py` `inherit` on branches `2.0` and `2.2` |
| `InheritDefer` | the `inherit_defer` directive | 5.0 scarthgap (BitBake 2.8) | (current) | BitBake release branches, recorded under "Directives" (T1.2) |
| `IncludeAll` | the `include_all` directive | 5.2 walnascar (BitBake 2.12) | (current) | BitBake release branches, recorded under "Directives" (T1.2) |
| `UnpackDir` | `do_unpack` unpacks into `UNPACKDIR` (`${WORKDIR}/sources-unpack`), not `WORKDIR` | 5.1 styhead | (current) | Yocto migration guide 5.1 |
| `WorkdirAsSourceDir` | `S = "${WORKDIR}"` is accepted | 4.0 kirkstone (floor) | 5.0 scarthgap | Yocto migration guide 5.1 ("If a recipe has S set to be WORKDIR, this is no longer supported, and an error will be issued.") |
| `PrInWorkdir` | `WORKDIR` ends in `${EXTENDPE}${PV}-${PR}` (later: `${PV}`) | 4.0 kirkstone (floor) | 4.2 mickledore | OE-core `meta/conf/bitbake.conf` on branches `kirkstone`, `langdale`, `mickledore` (`.../${PN}/${EXTENDPE}${PV}-${PR}`) and `nanbield`, `scarthgap` (`.../${PN}/${PV}`); Yocto migration guide 4.3, "Versioning changes" ("PE and PR are no longer included in the work directory path") |

Details:

- **Class directories.** BitBake 2.0 (`kirkstone`) `inherit`: a name that is not absolute and
  does not end in `.bbclass` becomes `classes/<name>.bbclass`, searched with
  `bb.utils.which(BBPATH, ...)`; nothing refers to `classes-recipe` or `classes-global`.
  BitBake 2.2 (`langdale`) loops over `["classes-" + __bbclasstype, "classes"]`, each along
  the whole `BBPATH`, first match wins. The 4.1 migration guide adds that a class used in the
  wrong context is an error during parsing, and that `classes/` keeps working in every
  context. `workspace::resolve` (`find_class`, `class_candidates`) follows `ws.release`.
- **`UNPACKDIR`.** The 5.1 guide: "Files from do_unpack now unpack to WORKDIR/sources-unpack/
  rather than WORKDIR/", and recipes that reference `SRC_URI` files through `WORKDIR`
  "likely need changing to UNPACKDIR".

### Scaffold: sources, `S` and build-system classes (verified, T6.2)

Checked 2026-11-17 against the Yocto migration guides for 5.1 and 5.3, OE-core
`meta/conf/bitbake.conf` on the branches `kirkstone`, `styhead`, `walnascar`, `whinlatter`,
`wrynose` and `master`, `meta/classes-global/base.bbclass` (`styhead`), `go.bbclass`
(`kirkstone`, `wrynose`), and BitBake `lib/bb/fetch2/__init__.py` and `git.py` (branch `2.0`).
`src/scaffold/recipe.rs` follows this.

| Release | default `S` | `UNPACKDIR` | git unpack directory (no `destsuffix`) |
|---|---|---|---|
| kirkstone .. scarthgap | `${WORKDIR}/${BP}` | absent | `git/` under `WORKDIR` |
| styhead, walnascar | `${WORKDIR}/${BP}` | `??= "${WORKDIR}/sources-unpack"` | `git/` under `UNPACKDIR` |
| whinlatter, wrynose, master | `${UNPACKDIR}/${BP}` | `??= "${WORKDIR}/sources"` | `${BB_GIT_DEFAULT_DESTSUFFIX}` = `${BP}` |

- **Styhead move.** `base_do_unpack` (styhead): when `S` is under `WORKDIR` but not under
  `UNPACKDIR`, the first path component of `S` below `WORKDIR` is removed from `WORKDIR`
  before unpacking and, if it exists under `UNPACKDIR` afterwards, moved to `WORKDIR`. So
  `S = "${WORKDIR}/git"` and the default `${WORKDIR}/${BP}` keep working on styhead and
  walnascar (5.1 guide: "`S = "${WORKDIR}/${BP}"`, `S = "${WORKDIR}/git"` and also deeper
  paths continue to work as expected without changes").
- **5.1:** `S = "${WORKDIR}"` is an error; a recipe with only `file://` sources should set
  `S = "${UNPACKDIR}"` (the guide's recommended pattern). Go recipes add
  `destsuffix=${GO_SRCURI_DESTSUFFIX}` to `SRC_URI`.
- **5.3:** "If a recipe has S set to be `${WORKDIR}/something`, this is no longer supported,
  and an error will be issued"; the git fetcher unpacks into `BB_GIT_DEFAULT_DESTSUFFIX`
  (= `${BP}`), matching the default `S`, so `S = "${WORKDIR}/git"` should be removed.
- **Fetcher parameters** (BitBake 2.0, unchanged in meaning later): `subdir=<rel>` unpacks a
  URL into `<unpack root>/<rel>` (any fetcher); git's `destsuffix` defaults to `git/` and
  the checkout goes to `<unpack root>/<destsuffix>`.
- **Go:** `go.bbclass` on kirkstone has no `GO_SRCURI_DESTSUFFIX`; its `go_do_unpack` sets a
  missing `destsuffix` to `<basename of S>/src/${GO_IMPORT}/`. On wrynose
  `GO_SRCURI_DESTSUFFIX = "${@os.path.join(os.path.basename(d.getVar('S')), 'src', d.getVar('GO_IMPORT')) + '/'}"`
  and `GO_IMPORT` must be set. No single `SRC_URI` serves both, so a go recipe follows the
  workspace release when the layer claims releases on both sides of Styhead.

Consequences used by the generator:

- **git:** `destsuffix=${BP}` with no `S` line works on every supported release (kirkstone:
  `${WORKDIR}/${BP}` = `S`; styhead/walnascar: unpacked to `${UNPACKDIR}/${BP}`, then moved to
  `${WORKDIR}/${BP}` = `S`; whinlatter on: `${UNPACKDIR}/${BP}` = `S`). `sous-chef` has no
  `Feature` for the 5.3 git change, so it always writes this form.
- **local `file://` only:** `S = "${UNPACKDIR}"` when every release the layer is checked
  against has `UnpackDir`; `S = "${WORKDIR}"` when every one has `WorkdirAsSourceDir`;
  otherwise `subdir=${BP}` on each entry and the default `S` (the same reasoning as for git).
- **tarball:** the default `S` (`${BP}` under the unpack root) on every release, when the
  tarball's top directory is `<pn>-<pv>`.

Build-system classes, by file listing of OE-core `meta/classes` (`kirkstone`) and
`meta/classes-recipe` (`scarthgap`, `wrynose`): `autotools`, `cmake`, `meson`, `cargo`,
`go`, `module` (external kernel modules), `setuptools3`, `python_setuptools_build_meta`,
`python_flit_core`, `python_poetry_core` exist on all three; `python_hatchling` exists on
scarthgap and wrynose, not on kirkstone; `cargo-update-recipe-crates` is not on kirkstone. No
class used by the generator was renamed between kirkstone and wrynose (`distutils3` became
`setuptools3` before kirkstone). OE-core's `meta-skeleton` `hello-mod_0.1.bb` (wrynose) uses
`inherit module` and `RPROVIDES:${PN} += "kernel-module-hello"`.

A git URL without `branch` (and without `nobranch=1`): BitBake 2.0 `fetch2/git.py`
`urldata_init` warns ("does not set any branch parameter. The future default branch used by
tools and repositories is uncertain") and falls back to `master`. The generator asks for a
branch instead of relying on that.

### Scaffold: layers and classes (verified, T6.3)

Checked 2026-11-17 against OE-core `wrynose` (`meta/lib/bblayers/create.py`,
`meta/lib/bblayers/templates/layer.conf`, `meta/classes-global/base.bbclass`) and BitBake
`master` (`lib/bb/parse/ast.py`). `src/scaffold/layer.rs` and `class.rs` follow this.

- **`bitbake-layers create-layer`:** the layer name is the basename of the directory and is
  also the collection (`--layerid` overrides it); default priority 6; `LAYERSERIES_COMPAT`
  is filled from `LAYERSERIES_CORENAMES`; the `layer.conf` template extends `BBPATH` with
  `${LAYERDIR}`, adds `recipes-*/*/*.bb` and `.bbappend` to `BBFILES`, sets
  `BBFILE_COLLECTIONS`, `BBFILE_PATTERN_<id> = "^${LAYERDIR}/"`, `BBFILE_PRIORITY_<id>`,
  `LAYERDEPENDS_<id> = "core"` and `LAYERSERIES_COMPAT_<id>`; it writes `README` and
  `COPYING.MIT` and an example recipe. The generator differs where M6 says so: compat from
  the core layer's own `LAYERSERIES_COMPAT` (equal to `LAYERSERIES_CORENAMES` in OE-core),
  `README.md`, `COPYING.MIT` and `recipes-example/` only when asked. It adds the new path inside
  the last plain `BBLAYERS` assignment of `bblayers.conf` (as `${TOPDIR}/../<dir>` when the
  layer is beside the build directory's parent), falling back to an appended
  `BBLAYERS += "<path>"`; how `bitbake-layers add-layer` edits the file was not checked.
- **Tasks a class may override without `addtask`:** `base.bbclass` has `addtask fetch`,
  `unpack after do_fetch`, `configure after do_patch`, `compile after do_configure`,
  `install after do_compile`, `build after do_populate_sysroot` (and others), and
  `EXPORT_FUNCTIONS do_fetch do_unpack do_configure do_compile do_install`; `do_patch` comes
  from `meta/classes-global/patch.bbclass` (`addtask patch after do_unpack`,
  `EXPORT_FUNCTIONS do_patch`, implemented by python `patch_do_patch`). Any other task a class defines needs `addtask`.
- **`EXPORT_FUNCTIONS`** (`ExportFuncsNode`): in class `c`, `do_x` becomes a stub calling
  `c_do_x` (the function is named `<classname>_<func>`); a class name containing `-` with a
  shell function is fatal ("contains a dash character and is calling an sh function"), so
  the generator refuses `-` in the name of a class with tasks.

*Verify* (open, not used by any code): whether Wrynose changes how the build environment is
initialised, which matters to `src/bitbake/env.rs` (it sources `oe-init-build-env` or uses an
environment that is already set up). Checked 2026-11-17: the 6.0 migration guide only says
VSCode setup was dropped from `oe-init-build-env` ("Users are advised to use bitbake-setup
instead") and that the `meta-poky/conf/templates/default` templates moved to OE-core's
`meta` layer. The 6.0 release notes mention `bitbake-setup init` (it writes
`build/conf/auto.conf`) and that devtool ide-sdk looks for "bitbake-setup's init-build-env
first, and oe-init-build-env if not found". So a `bitbake-setup` build directory appears to
have its own `init-build-env` script; its location and whether `oe-init-build-env` still
works for such builds were not checked.

## OE-core checks and defaults used by lint

Checked by the wave 4 lint tasks against OE-core master (2026-11) unless marked:

- **`pkgvarcheck`** (`meta/classes-global/insane.bbclass`, anonymous python): only when
  `PACKAGES` is not empty, for each listed variable (`RDEPENDS`, `RRECOMMENDS`,
  `RSUGGESTS`, `RCONFLICTS`, `RPROVIDES`, `RREPLACES`, `FILES`, `pkg_preinst`,
  `pkg_postinst`, `pkg_prerm`, `pkg_postrm`, `ALLOW_EMPTY`), `if d.getVar(var, False):`
  reports it as set without a package. `override/missing-package` follows it.
- **`populate_lic_qa_checksum`** (`insane.bbclass`): `if lic == "CLOSED": return`, then
  `if not lic_files and d.getVar('SRC_URI')` is an error. So `LIC_FILES_CHKSUM` is required
  only when `LICENSE` is not `CLOSED` and `SRC_URI` is not empty (`hygiene/missing-metadata`).
- **`meta/conf/bitbake.conf` defaults** that lint must not count as set:
  `SUMMARY ?= "${PN} version ${PV}-${PR}"`, `LICENSE ??= "INVALID"`, `HOMEPAGE = ""`,
  `SRCREV ??= "INVALID"`. The `UNPACKDIR` defaults per release are in the table under
  "Scaffold: sources, `S` and build-system classes" (none on kirkstone and scarthgap).

*Verify* (relied on by lint, not checked by any task):

- that OE-core has an `uppercase-pn` QA check reporting upper-case letters in `PN`
  (`org/recipe-path`);
- that `IMAGE_INSTALL` is read only by image recipes (`org/policy-in-recipe`);
- the fetcher's checksum lookup, as `hygiene/src-uri-checksum` assumes it: an entry with
  `name=<n>` uses `SRC_URI[<n>.sha256sum]`, one without uses `SRC_URI[sha256sum]`, and a
  `sha256sum=` parameter also counts; and the `SRCREV` lookup order for a git entry with
  `name=<n>` (`SRCREV_<n>`, else `SRCREV`).

## What the static evaluator cannot do

- `${@python expression}` in values
- anonymous python functions and event handlers
- `d.setVar` / `d.appendVar` inside classes (e.g. `PACKAGECONFIG` processing in `base.bbclass`)
- `inherit` / `require` whose argument depends on any of the above

These are reported, never silently guessed. Authoritative mode (M5) covers them.

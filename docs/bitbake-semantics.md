# BitBake semantics we reproduce

This is the reference for the parser, the workspace code, the evaluator and lint. It is a
working summary, not the authority. **When a case is not covered here or you are unsure,
check the BitBake User Manual ("Syntax and Operators") or the BitBake source
(`lib/bb/parse/`, `lib/bb/data_smart.py`), then add what you learned to this file, under its
topic.** Each rule says where it was checked: "BitBake master (2026-11)" is the master branch
of mid-November 2026 (commit `5495a547`, 2026-11-16, unless another commit is named); "BitBake
2.18.0" is the release in the live environment (`6e260eb6`, live-environment.md), where
"live" or "observed" means seen in real output there. Items marked *verify* were written
from memory or taken from documentation and are not confirmed; they are listed again at the
end ("Not yet verified").

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

### Lines

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

### Statement order and the assignment regex

Statement match order (first match wins): function header, `def`, `EXPORT_FUNCTIONS`,
`addtask`, `deltask`, `addhandler`, `inherit`, `inherit_defer` (BBHandler), then assignment,
`include`, `require`, `include_all`, `export VAR`, `unset VAR`, `unset VAR[flag]`, `addpylib`
(base configuration only), `addfragments` (ConfHandler, any file; see "Configuration
fragments"); anything else is "unparsed line".
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

Checked against BitBake master (2026-11), `lib/bb/data_smart.py` (`getVarFlag`,
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
- **Append / prepend order.** Appends first, each active one
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
  A plain `setVar` outside parsing (`parsing` not set, as `expandVarref('LAYERDIR')` does
  after each `layer.conf`) does the opposite: it deletes the variable's pending
  `:append` / `:prepend` / `:remove` lists and its active override variants, so the value
  stored is the folded one (BitBake 2.18.0 `data_smart.py` `setVar`).

### Splitting list values

BitBake splits a value only **after** expanding it, python included: `need_overrides`
(`lib/bb/data_smart.py`, BitBake 2.18.0) does `self.overrides = (self.getVar("OVERRIDES")
or "").split(":")`; list consumers do `(d.getVar("DEPENDS") or "").split()`
(`ast.handleVirtRecipeProviders`, `taskdata`, OE-core's `base.bbclass` for `PACKAGECONFIG`,
whose flags it splits with `flagval.split(",")` after `getVarFlags` expanded them). The
`${@...}` has been run by then, so no python text is ever split. A static value can still
hold `${@...}` (or an undefined `${VAR}`): OE-core's `MACHINEOVERRIDES` for ti33x is
`${@['', 'ti-soc:ti33x:'][...]}dogbonedark`, curl's `DEPENDS:append:class-target` is
`${@bb.utils.contains('PTEST_ENABLED', '1', ' openssl-native', '', d)}`, libglvnd's
`PROVIDES` is four such expressions. Splitting that text gives fragments (`'',`, `']['ti-soc`)
that are not words of the real value.

sous-chef splits every value with one tokeniser, `src/parse/words.rs`: a `${...}` group is
opaque (braces nest; inside `${@...}` a quoted string is skipped, so a brace in a string
does not end it; unterminated, it runs to the end), so whitespace, `:` and `,` inside it do
not split. A word or entry that holds a group is **unevaluated**: it stands for text that is
not known (any number of real words, possibly none), it is shown as what it is (abbreviated
`${@name(...)}` in tables), and it is never an override name (`DataStore::overrides` leaves
it out; `override_entries` lists it), a dependency, a provider, a package or a
`PACKAGECONFIG` option. `:remove` matching uses the same split, so a remove word is never
matched against a fragment. (BitBake's own regex for the end of `${@...}`,
`\$\{@(?:\{.*?\}|.)+?\}`, stops at the first `}` that does not close a one-level `{...}`;
for the python OE-core writes the two agree.) Used by `eval/overrides.rs`,
`eval/providers.rs`, `eval/replay.rs` (`INHERIT`, `BB_DEFER_BBCLASSES`, the
`BB_RECIPE_VIRTUAL_PROVIDERS` rewrite, which keeps an expression whole when it joins the
words again), `index/scan.rs` (`provides` and `depends` rows, the `PACKAGECONFIG` build
field), the `overrides` and `deps` tools, and (w6) every lint rule that splits a value
(`SRC_URI` entries, `inherit` / `INHERIT` class lists, `BBCLASSEXTEND`, the `*OVERRIDES`
values `KnownOverrides` reads, task names, the words of `bblayers.conf` lines). The known
set of `override/dead` (and the `overrides` tool's dead view) takes candidate names from
`words::visible_text`: the text outside `${...}` and, inside `${@...}`, only its quoted
strings (the values python may add, `'armv7a:'`), never the python code (`bb`, `utils`,
`d`).

### Datastore

Checked against `ast.py` `DataNode.eval` and `data_smart.py`, BitBake master (2026-11):

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
- **Expansion** (`expandWithRefs`): every `${NAME}` with `NAME` matching
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

**The `func` flag of renamed and keyword functions** (BitBake 2.18.0 `lib/bb/parse/ast.py` `MethodNode.eval` and
`lib/bb/data_smart.py` `renameVar`, `setVar`): `MethodNode.eval` does
`setVar(name, body, parsing=True)` and `setVarFlag(name, "func", 1)` with the name as
written. For `pkg_postinst:${PN}:prepend () {` the `setVar` puts the body in the prepend
list of `pkg_postinst:${PN}` and the flag lands on the separate name
`pkg_postinst:${PN}:prepend`. `expandKeys` then renames `pkg_postinst:${PN}` to
`pkg_postinst:busybox` with `renameVar`, which copies the value and only the
`_append` / `_prepend` / `_remove` flags; `func` is lost. So in BitBake
`getVarFlag("pkg_postinst:busybox", "func")` is `None` and `bitbake -e` prints it as a
variable (`pkg_postinst:busybox="..."`, checked in the busybox dump of the differential
run), although its text is the shell body. sous-chef keeps the same flags
(`raw_flag(name, "func")`), and `DataStore::is_function` answers the question tools need,
"is this value a function body": the flag, or a name a function statement wrote (followed
through `expand_keys`).

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

Checked against BitBake master (2026-11) and the release branches on GitHub
(`openembedded/bitbake`), plus the BitBake User Manual
(`doc/bitbake-user-manual/bitbake-user-manual-metadata.rst`, "inherit_defer Directive",
"include_all Directive"):

- `inherit_defer` (`inherit_defer\s+(.+)`): present in bitbake 2.8 (Yocto 5.0 scarthgap),
  absent in 2.6. The text is stored unexpanded and inherited at the end of parsing, so
  `${VAR}` / `${@...}` see the final values; an empty expansion is a no-op. Classes listed in
  `BB_DEFER_BBCLASSES` are deferred even when written with plain `inherit`. The argument is
  everything after the keyword, `#` included (no comment stripping), for both `inherit` forms.
- `inherit_defer` **before bitbake 2.8** (checked against
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
  and `2.12`, present on `master` (2026-11). Settled (2026-11-17) in the BitBake
  clone of the live environment: added by commit `8e741b2e8` ("ast/BBHandler: Add support
  for BB_DEFER_BBCLASSES", 2025-06-12, `git describe --contains`: `2.15.0~2`); `git show
  origin/<branch>:lib/bb/parse/parse_py/BBHandler.py` has it on `2.16` and `2.18`, not on
  `2.12` (there is no `2.14` branch). **First release: BitBake 2.16 (Whinlatter).** The
  replay still honours it on every release.
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
- `addpylib` (`__addpylib_regexp__ = addpylib\s+(.+)\s+(.+)`, `re.match`, greedy: with
  more than two words the first group takes all but the last). Checked in BitBake
  `lib/bb/parse/parse_py/ConfHandler.py` `feeder` and `lib/bb/cookerdata.py` on branch
  `2.18` (`6e260eb6a`), branches `2.0`, `2.2` and `2.4`, and `git describe --contains
  afb8478d3` = `2.3.1~96` (the commit that added it). **First release: BitBake 2.4
  (Mickledore)**: on `2.0` (Kirkstone) and `2.2` (Langdale) `feeder` has no such pattern
  and the line ends in `raise ParseError("unparsed line: ...")`, so the file fails to parse.
  From 2.4 on, `feeder` accepts it only `if baseconfig and conffile and m`: `conffile` is
  false when `BBHandler.feeder` (recipes, classes, `.inc`) falls through to
  `ConfHandler.feeder`, and `baseconfig` is true only for the files `cookerdata` passes to
  `parse_config_file` (`bb.parse.handle(fn, data, include, baseconfig=True)`): the
  `-r` prerun files, `conf/bblayers.conf`, each layer's `conf/layer.conf`,
  `conf/bitbake.conf` and the `-R` postrun files. A file any of those *includes* is parsed by
  `ConfHandler.include_single_file` with `bb.parse.handle(fn, data, True)`, `baseconfig`
  false, so `addpylib` in `local.conf`, a machine or distro conf, or a file `layer.conf`
  includes is an "unparsed line" too. `ast.PyLibNode.eval` expands the directory, appends it
  to `sys.path` and imports the namespace (and the modules its `BBIMPORTS` lists); it sets no
  variable. sous-chef: `StatementKind::AddPyLib`, no edge, no effect on the replay;
  `hygiene/parse-error` reports it outside `conf/layer.conf` / `conf/bitbake.conf` /
  `conf/bblayers.conf` on every release, and there on a release without
  `Feature::AddPyLib`. (The prerun and postrun files are not known to sous-chef.)

### Configuration fragments

Checked in BitBake `lib/bb/parse/parse_py/ConfHandler.py` (`__addfragments_regexp__`,
`feeder`) and `lib/bb/parse/ast.py` (`AddFragmentsNode`, `handleAddFragments`) on the
branches `2.10`, `2.12` (head `ac0973009`), `2.16` (head `713fbbdb9`) and `2.18` (the live
environment, `6e260eb6a`, BitBake 2.18.0), the tags `yocto-5.1.4`, `yocto-5.2.4`,
`yocto-5.3`, `yocto-6.0`, the history of both files (`f68774670` "parse: add support for
'addfragments' directive", 2024-12-05; `ed9a3ca94` flagged variables; `3b9d7bea9` built-in
fragments, 2025-06-18, first in yocto-5.3; `406b1e3de` / `79fff3f42` fatal "Could not find
fragment"; `cfddce7ab` error for a built-in fragment next to an assignment; `5ed1dee9a`
`_BB_SKIP_FRAGMENTS`), `lib/bb/utils.py` `get_file_layer`, and OE-core
`meta/conf/bitbake.conf` (Wrynose, lines 813-822).

- **Where.** OE-core's `bitbake.conf` includes `conf/site.conf`, `conf/auto.conf`,
  `conf/toolcfg.conf` (written by `bitbake-config-build` / `bitbake-setup`, in the build
  directory, found on `BBPATH`) and `conf/local.conf`, then sets
  `OE_FRAGMENTS_PREFIX ?= "conf/fragments"`,
  `OE_FRAGMENTS_METADATA_VARS ?= "BB_CONF_FRAGMENT_SUMMARY BB_CONF_FRAGMENT_DESCRIPTION"`,
  `OE_FRAGMENTS_BUILTIN ?= "machine:MACHINE distro:DISTRO"` and runs
  `addfragments ${OE_FRAGMENTS_PREFIX} OE_FRAGMENTS OE_FRAGMENTS_METADATA_VARS OE_FRAGMENTS_BUILTIN`,
  **before** `include conf/machine/${MACHINE}.conf` and `include conf/distro/${DISTRO}.conf`.
  ConfHandler parses a file into statements and evaluates them in order, so the directive
  sees everything the earlier includes set and the machine and distro confs see its result.
- **Syntax.** BitBake 2.12 (Walnascar): `addfragments\s+(.+)\s+(.+)\s+(.+)`; from 2.16
  (Whinlatter): `addfragments\s+(.+)\s+(.+)\s+(.+)\s+(.+)`. `re.match` on the line (after
  `rstrip`), every group greedy, so extra words go to the first group (`a b c d` with three
  groups is `a b` / `c` / `d`). A line the release's regex does not match is "unparsed line"
  (a parse error): the three-argument form on 2.16 and later, anything on 2.10 and earlier,
  where the directive does not exist. The test is in `ConfHandler.feeder` with no `baseconfig`
  condition, so BBHandler accepts it in a recipe too. Groups are used unexpanded: the first
  is a path prefix (expanded when the file name is built), the others are variable names.
- **Evaluation** (`AddFragmentsNode.eval`, 2.18): return at once if `_BB_SKIP_FRAGMENTS` is
  `1` (2.16 and later; set by `bitbake-config-build`). Read `fragments = getVar(<2nd>)`,
  `BBLAYERS`, `getVar(<3rd>).split()` and, from 2.16, the built-in map
  `{prefix: variable}` from `getVar(<4th>).split()` split on `:` (an unset list variable or
  a word without `:` is a python exception). Return if `fragments` is empty. Warn when two
  built-in fragments name the same prefix (the last wins). Then for each word `f` of
  `fragments`, in order:
  - **built-in** (2.16+, `check_and_set_builtin_fragment`): `prefix, value =
    f.split('/', 1)`; when `prefix` is in the map, `bb.fatal` if
    `getVar(<variable>, noweakdefault=True)` is not `None` ("A builtin fragment '...' is
    used while MACHINE has already got an assignment"; a `??=` default is allowed), else
    `setVar(<variable>, value, parsing=True)` with the history pointing at the last `set`
    or `append` (`=` / `+=`) of the fragments variable whose value contains `f`
    (`toolcfg.conf:6` "kiss (OE_FRAGMENTS contains "distro/kiss")" in the live dump).
  - **named** otherwise: `layerid, name = f.split('/', 1)` (a word without `/` raises
    `ValueError`); the file is `<prefix>/<name>.conf` expanded, looked for under each
    `BBLAYERS` entry in order (`os.path.join(entry, file)`), the first that exists **and**
    whose collection (`bb.utils.get_file_layer`: the `BBFILES_PRIORITIZED` entry it matches,
    else the collection with the longest `BBFILE_PATTERN` regex that `re.match`es the path)
    is `layerid` wins. So `core/yocto/sstate-mirror-cdn` is
    `meta/conf/fragments/yocto/sstate-mirror-cdn.conf` of the layer whose collection is
    `core`. It is included like `include` (`ConfHandler.include`, "include fragment"); then
    for each variable `v` of the third argument: `setVarFlag(v, f, getVar(v))` and
    `setVar(v, None)` (the fragment's summary moves to a flag named after the fragment).
    Not found: `bb.fatal("Could not find fragment ...")` on 2.16 and later, `bb.error` (and
    go on) on 2.12.
- **sous-chef** (`src/eval/replay.rs` `add_fragments`, `Release::has(Feature::AddFragments)`
  / `Feature::BuiltinFragments`): the parser makes a line with at least three arguments a
  `StatementKind::AddFragments { rest }` (no diagnostic; fewer
  arguments stay an unknown statement with "unparsed line"), and
  `Statement::add_fragments` / `AddFragments::groups` reproduce the regex; the index stores
  it as kind `addfragments` with an `addfragments` edge whose target is always unresolved
  (the files follow from the evaluated fragments variable, never from the file alone). The
  replay does the above at the statement's place, with these differences: `BBLAYERS` is
  the workspace's layer list (canonical roots, the `LAYERDIR` gap); the
  `BBFILES_PRIORITIZED` step of `get_file_layer` is skipped (a `.conf` never matches a
  `.bb` glob in practice); a fatal error, an unknown fragment, a fragment list that needs
  python or undefined variables, and the directive on a release without it are
  `ReplayNotes::references` entries (`RefProblem::Fatal`, `NotFound`, `Python` /
  `Undefined`, `NotInRelease`) and the replay goes on; when the list cannot be expanded,
  the variables of the built-in map are flagged (`ReplayNotes::unresolved_vars`: their
  values carry the fragments variable in `unresolved`). A `machine` / `distro` given to the
  evaluator wins over a built-in fragment and does not count as an earlier assignment.

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

Checked against BitBake master (commit `5495a547`, 2026-11-16), and reproduced by
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
- **`inherit` inside a class** (checked 2026-11-17 on BitBake 2.18, `6e260eb6a`:
  `BBHandler.py` `inherit`, `cookerdata.py` `CookerDataBuilder.__init__` and
  `parseConfigurationFiles`; and on branch `2.0`): the context is not a property of the class
  but of the datastore when the `inherit` runs. `inherit` reads
  `d.getVar("__bbclasstype", False)` for every name; `cookerdata` sets it to `global` on
  `basedata` (before any configuration file) and to `recipe` on every multiconfig's data
  after `parseConfigurationFiles` (`mcdata[mc].setVar("__bbclasstype", "recipe")`). So an
  `inherit` in a class that `INHERIT` (or `base`) pulls in is searched
  `classes-global/`, `classes/`; the same class inherited from a recipe searches
  `classes-recipe/`, `classes/`. A class is parsed once per datastore (`__inherit_cache`):
  one reached through `INHERIT` is already in the recipe datastore's cache and is not parsed
  again in recipe context. A class in `classes-recipe/` can only be reached in recipe context
  and one in `classes-global/` only in global context (neither search looks in the other's
  directory), so their `inherit`s are recipe-only and global-only. A class in `classes/` is
  reachable from both: OE-core's `meta/classes/sign_rpm.bbclass` (also `sign_ipk`,
  `sign_package_feed`) says `inherit sanity`, and `sanity` is in `classes-global/`, found
  because those classes are used through `INHERIT`. BitBake 2.0 has no `__bbclasstype`
  (`inherit` looks for `classes/<name>.bbclass` only). sous-chef: the scanner resolves an
  `inherit` in a `classes/` class in both contexts (on a release with
  `Feature::SplitClassDirs`); it is resolved when either search finds the class, and
  `edges.context` records `recipe`, `global` or `both`; when the two find different files
  there is an edge for each. An `inherit` in an `.inc` follows what reaches the `.inc` (w9,
  below: "Which handler parses an included file"). The replay follows the context it is
  actually in.
- **Global classes** (`cookerdata.py`): the configuration inherits `["base"] + INHERIT`, in that
  order, after the configuration files, each with `__bbclasstype = "global"`.
- **bbappend matching** (`cooker.py` `get_file_appends`): with `b` the append's file name with
  `.bbappend` replaced by `.bb` and `f` the recipe's file name, the append applies when
  `b == f`, or when `b` contains `%` and `b.startswith(f[:b.index('%')])`. Only file names
  are compared, never directories.
- **`BBPATH`** is whatever the configuration leaves in the variable: OE layers usually do
  `BBPATH .= ":${LAYERDIR}"` but some prepend (`BBPATH =. "${LAYERDIR}:"`), so sous-chef reads
  the value from `bblayers.conf` and the layer confs instead of assuming the layer order
  (`layers::read_bbpath`). Changes made later in `bitbake.conf` are not seen.
- **`BBPATH` from the environment** (BitBake 2.18.0 `lib/bb/cookerdata.py`
  `findTopdir` and `lib/bb/data.py` `inheritFromOS`; OE-core `scripts/oe-buildenv-internal`
  line 82, `BBPATH=$BUILDDIR` / `export BBPATH`, sourced by `oe-init-build-env` and by
  bitbake-setup's `init-build-env`; the live curl dump shows `from env data.py:105
  [inheritFromOS]` as the first `BBPATH` operation): BitBake starts with `BBPATH` = the build
  directory. A bitbake-setup `bblayers.conf` sets only `BBLAYERS`, so this is how
  `include conf/site.conf`, `conf/toolcfg.conf` and `conf/local.conf` in `bitbake.conf` find
  the build directory's files. sous-chef seeds `BBPATH` with the build directory in the
  replay and in `layers::read_bblayers` (so the index edges agree); a `BBPATH = ...` in
  `bblayers.conf` replaces it, a `?=` keeps it (kas writes `BBPATH ?= "${TOPDIR}"`, the same
  value).

sous-chef differences: only files inside the workspace (layer roots and the build directory)
are ever found; `BBMASK` is not applied by `recipe_files` (the index applies a static one,
see "What the static evaluator cannot do").

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

## Recipes, bbappends and data files

Which files BitBake collects as recipes and bbappends, in which order, and which files in a
layer are not metadata at all. Checked against BitBake master (2026-11) and 2.18.0
`lib/bb/cooker.py`; reproduced by `src/workspace/resolve.rs` and `src/index/scan.rs`.

- **`BBFILES` expansion** (`cooker.py` `collect_bbfiles`): the words of `BBFILES` are sorted
  (stably) by the priority of the collection whose `BBFILE_PATTERN` matches the glob text,
  lowest first; a word that is a directory (`os.path.isdir`) is walked for `.bb` /
  `.bbappend` (`find_bbfiles`, `os.walk` order); any other word is expanded with
  `glob.glob` (not recursive; `*` does not match a leading `.`), a word that matches nothing
  but exists is kept as is, and the matches are taken in `sorted(globbed)` order; duplicates
  are dropped (first kept); `BBMASK` regexes are applied afterwards and only names ending in
  `.bb` / `.bbappend` are kept (a directory a glob matches is not walked). The `.bbappend`
  files are kept in this order, which is the order `get_file_appends` returns them in, so
  appends apply **lowest layer priority first** (the highest priority layer has the last
  word), ties in `BBFILES` order.
- **The sort is a string sort** (BitBake 2.18.0
  `lib/bb/cooker.py` `CookerCollectFiles.collect_bbfiles`, `for g in sorted(globbed)`):
  Python compares `str` by code point, so `-` (U+002D) sorts before `/` (U+002F) and
  `.../recipes-x/foo-bar/a_%.bbappend` comes before `.../recipes-x/foo/a_%.bbappend`; the
  `foo/` append is applied last. A `Path` comparison (component by component) gives the
  opposite. sous-chef compares the path strings by code point (`resolve::py_str_cmp`); a
  byte that is not valid UTF-8 counts as `U+DC00 + byte`, as Python's `surrogateescape`
  decoding of file names makes it (exact for single invalid bytes; for a longer invalid
  sequence Rust's and Python's decoders agree on which bytes are invalid as both follow
  the Unicode "maximal subpart" rule, not separately checked). The same order breaks ties
  between recipe files of one layer in provider selection and `query::find_recipes`.
  `find_bbfiles` (a directory in `BBFILES`) is not sorted by BitBake; sous-chef sorts it the
  same way for determinism.
  Confirmed against `bitbake-layers show-appends` on the live environment (BitBake 2.18.0, meta-arm
  `673e8d7c`): `optee-ftpm/optee-os_%.bbappend` comes before `optee/optee-os_%.bbappend`.

**A bbappend that matches no recipe** (checked in `cooker.py`
`show_appends_with_no_recipes`, master and branches `2.0` and `2.8`): BitBake stops with a
fatal error ("No recipes available for"). On BitBake 2.0 (Kirkstone) and 2.8 (Scarthgap)
`BB_DANGLINGAPPENDS_WARNONLY` set makes it a warning; master has no such variable. Settled
in the BitBake clone of the live environment: commit `fca9c9e3c` ("cooker: Drop support for
BB_DANGLINGAPPENDS_WARNONLY", 2024-11-21); `lib/bb/cooker.py` has it on branch `2.10`, not on
`2.12`. **Last release with it: BitBake 2.10 (Styhead); fatal from 2.12 (Walnascar).** The
`layers` tool (`show: appends`) and `org/append-no-match` report these appends. A bbappend
under a `BBFILES_DYNAMIC` entry whose collection is absent is not collected at all, so
BitBake neither applies nor reports it (live: meta-arm's
`dynamic-layers/meta-tpm2/.../tpm2-tools_5.7.bbappend` is not in `show-appends`).
**`BBFILES_DYNAMIC`** (checked in BitBake 2.18.0 `6e260eb6` `lib/bb/cookerdata.py`
`parseConfigurationFiles`, after the layer confs): the value is split on whitespace; each
word must be `<collection>:<glob>` (split on the first `:`; anything else is fatal,
"BBFILES_DYNAMIC entries must be of the form {!}<collection name>:<filename pattern>"); a
leading `!` inverts the test; the glob is appended to `BBFILES` when the collection is in
`BBFILE_COLLECTIONS` (or, inverted, when it is not). `collect_bbfiles` then treats it as
any other `BBFILES` glob. The first release with `BBFILES_DYNAMIC` is BitBake 1.36
(`04f8bd50a`, 2017, `git describe --contains`: `1.35.0~73`), the `!` form came with
`bfd729cfe` (2020, `1.48.0~144`); both are in `lib/bb/cookerdata.py` on branch `2.0`
(Kirkstone), so every supported release has them and no `Feature` gates them. A word with
an empty collection (`:glob`) makes `l[0]` raise in BitBake. sous-chef (w6,
`resolve::collect` / `resolve::bbfiles_dynamic`, used by `resolve::recipe_files` and so by
the index): the value is read from the datastore discovery builds when it reads the layer
confs (`workspace::layers::read_bbfiles_dynamic`, w7: `bblayers.conf`, then each
`layer.conf` in `BBLAYERS` order with the files it includes, `${LAYERDIR}` replaced after
each file as `expandVarref` does, every operator, override-qualified names and
`:append` / `:prepend` / `:remove` with their conditions, as the `OVERRIDES` known at that
point select them), so it is the value BitBake sees after all layer confs; each word is
attributed to the layer after whose `layer.conf` it first appeared. An active entry's glob
is added to the globs, sorted by that layer's priority like the static ones. A word without
`:` is BitBake's fatal error and makes discovery fail with BitBake's message ("BBFILES_DYNAMIC
entries must be of the form {!}<collection name>:<filename pattern>, not:" and the words,
one per line, plus the files that set them), so `status` shows it as the startup error and
`audit` stops on it; a word with an empty collection name fails discovery too. A word holding
python (`${@...}`), or whose collection or glob still holds a `${...}` (BitBake expands
`BBFILES` again later with the whole configuration), is left out and listed: `layers
show=appends` and the audit report's layer-structure notes name it. The index marks a file only an inactive entry matches
`files.not_collected = 'dynamic'`; an active entry's files get `bbappend` edges, are
evaluated with their recipe, linted, and `org/append-no-match` sees a dangling one.
`layers show=appends` reads the index.

**A `.bb` or `.bbappend` that no `BBFILES` glob matches** (BitBake 2.18.0 `lib/bb/cooker.py`): BitBake knows only the files `collect_bbfiles`
returns; recipe names, providers (`bb.providers` over the parsed recipes) and
`get_file_appends` all start from that list, so such a `.bb` is never a recipe for a name
(`bitbake -e example` for OE-core's `meta/lib/bblayers/templates/example.bb`: "Nothing
PROVIDES 'example'") and such a `.bbappend` applies to nothing. `matchFiles`, used for
`bitbake -b <file>`, takes any existing path (`os.stat(bf)` succeeds: `return [bf]`), so a
file named directly is still parsed, with the collected appends only. sous-chef keeps the
file in the index (`item`, `search`, `layers` show it) and marks it
(`files.not_collected = 'bbfiles'`, set with the edges from `resolve::recipe_files`);
`query::find_recipes`, `recipe_names_like`, `files_providing` and
`providers::recipes_using` leave it out, it takes part in no `bbappend` edge, and an
evaluation addressed to it by path (`layer:path`) adds the note "not matched by any layer's
BBFILES" (`ReplayNotes::target_notes`). A file a static `BBMASK` masks is `not_collected =
'bbmask'` (w6), one only an inactive `BBFILES_DYNAMIC` entry matches `dynamic`. The
`item` and `search` tools add the same note for a hit in such a file, and
`query::files_in_layer_in(.., FileSet::COLLECTED)` lists a layer's files without them. Lint
(w6) runs no file- or recipe-scope rule on such a file and reports it once
(`query::uncollected_recipe_files`; `org/recipe-path` for a `.bb`, `org/layer-conf` for a
`.bbappend`).

### Data files

Checked in BitBake 2.18.0 (`lib/bb/cookerdata.py` `parseConfigurationFiles`,
`parse/parse_py/ConfHandler.py` `include_single_file`, `parse/parse_py/BBHandler.py`
`inherit`, `lib/bb/cooker.py` `collect_bbfiles`): BitBake parses a `.conf` only when the configuration reaches it
(`bblayers.conf`, each layer's `conf/layer.conf`, `conf/bitbake.conf` on `BBPATH`, and what
those include), an `.inc` only when a `require` / `include` names it, a `.bbclass` only from
`classes-<type>/` or `classes/` along `BBPATH` (or by absolute path), and a `.bb` /
`.bbappend` only when `BBFILES` collects it. Nothing else in a layer is metadata: OE-core
ships about 35 `.conf` files that recipes install (`wpa-supplicant/wpa-supplicant/wpa_supplicant.conf`,
`busybox/files/mdev.conf`, the systemd drop-ins), `files/wic/common.wks.inc` (a wic
kickstart include) and `conf/ccache.conf` (ccache's own configuration, used as
`CCACHE_CONFIGPATH`). sous-chef cannot know without evaluating which files a dynamic
reference reaches, so it decides by place and by the index's edges, keeping a file when in
doubt (`index/scan.rs` `mark_data`): a `.conf` / `.inc` / `.bbclass` is metadata when it is
in the layer's `conf/` tree, is a class under a `classes*/` directory, is the target of a
resolved `include` / `require` / `include_all` / `inherit` edge, or could be the target of
an unresolved one (the reference's path, `${...}` standing for any text within a component,
ends the file's path; a bare name only next to the including file or at a layer root; a
reference that is all `${...}` matches nothing); a `.conf` in `conf/` is metadata unless
it is none of the files BitBake and OE-core read by convention **and** it has a line
BitBake's configuration parser rejects (w6, `scan.rs` `conventional_conf`: BitBake 2.18.0
`cookerdata.py` `parseConfigurationFiles` reads `conf/bblayers.conf`, every
`conf/layer.conf` and `conf/bitbake.conf`; OE-core's `bitbake.conf`, Wrynose lines
813-833, includes `conf/site.conf`, `auto.conf`, `toolcfg.conf`, `local.conf`, and by
pattern `conf/multiconfig/`, `conf/machine/`, `conf/machine-sdk/`, `conf/distro/`, and
`conf/fragments/` through `addfragments`; `conf/templates/` holds build templates; "rejects":
an unparsed line, a function, `inherit`, `addtask`, `deltask`, `addhandler` or
`EXPORT_FUNCTIONS`, which `ConfHandler.feeder` does not accept), in which case it is data
(a `.conf` that parses cleanly stays metadata: it may be included in a way the index cannot
see); otherwise it is **data** when it is under a
directory a recipe takes files from (`files/`, `<pn>/` or `<pn>-<pv>/` next to a `.bb` /
`.bbappend` of that name, `FILESPATH`'s defaults) or under `files/`, `lib/`, `scripts/` or
`wic/`; anything else stays metadata. A data file is indexed and marked
(`files.not_collected = 'data'`): `item` shows it with a note, `search` (every mode) and the
file listings (`all_files`, `files_by_kind`, `files_in_layer`; lint's file list) leave it out
unless `FileSet::ALL` is asked for, and the replay still reads it if an evaluation reaches it.
The marks are recomputed with the edges, and with one file's edges on `refresh_file`.
`conf/ccache.conf` (ccache's `key = value` syntax, unparsed lines for BitBake, referenced only
through `CCACHE_CONFIGPATH`) is data since w6: `hygiene/parse-error` with `--all` on the
live workspace went from 3 to 0 (422 before the data-file rule).

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
layer has the last word; see "Recipes, bbappends and data files"; `multi_finalize` parses
`__BBAPPEND` in `get_file_appends` order). Confirmed against `bitbake-layers show-appends`
on the live environment (BitBake 2.18.0, yocto-kiss kas build): the same recipes get the
same bbappends in the same order, ties sorted as strings. Priorities match `bitbake-layers
show-layers`, which in 2.18 names each layer by its **collection** (`core`,
`arm-toolchain`), not its directory; sous-chef names layers by directory.

Checked against BitBake master (2026-11) (`lib/bb/cookerdata.py`, `cooker.py`,
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

**Layer validation** (checked in `lib/bb/cookerdata.py` `parseConfigurationFiles` and
`lib/bb/cooker.py` `handleCollections`, master and branches `2.0` and `2.8`, which agree, and
2.18.0 for the compat checks):

- `LAYERSERIES_COMPAT_<c>` not set: a warning on every run ("should set
  LAYERSERIES_COMPAT_<c>"); the layer is accepted.
- `LAYERSERIES_COMPAT_<c>` with no name in common with `LAYERSERIES_CORENAMES`: fatal
  ("Layer <c> is not compatible with the core layer"). Only names are compared.
- **A copied `LAYERSERIES_COMPAT`** (`LAYERSERIES_COMPAT_x = "${LAYERSERIES_COMPAT_core}"`):
  BitBake 2.4 (Mickledore) and later delete each `LAYERSERIES_COMPAT_<c>` right after its
  layer's `layer.conf` and set them again only after the last layer (`cookerdata.py`,
  commit `6709aedcc` "cookerdata: Ensure layers use LAYERSERIES_COMPAT fairly", 2022-12-01;
  present on branches `2.4`, `2.8` and `2.18`; in 2.18.0 `cookerdata.py:415-433`), so the
  referenced variable has been deleted (or is not set yet, for a later layer) when the copy
  is expanded. An undefined reference is left as written by expansion (`data_smart.py`
  `VariableParse.var_sub`), so the copy expands to the literal `${LAYERSERIES_COMPAT_core}`,
  names no release, and BitBake stops with "Layer x is not compatible with the core layer
  which only supports these series: ..." (or "No core layer found to work with layer 'x'"
  without one; `cookerdata.py:462-464` in 2.18.0). BitBake 2.0 (Kirkstone) and 2.2
  (Langdale) delete nothing and read the expanded value after all layers, so the copy is
  accepted there. The check is in `cookerdata.py`, not `cooker.py`. `org/layer-conf`
  reports the copy as an error, and as a warning when the workspace release is Kirkstone or
  Langdale.
- The same collection added by two layers: fatal ("Found duplicated BBFILE_COLLECTIONS").
- A `LAYERDEPENDS_<c>` entry that no layer adds: an error that stops BitBake ("depends on
  layer '<d>', but this layer is not enabled in your configuration").

The replay does not reproduce these checks (it never stops on them); `org/layer-conf`
reports them.

**What the `BBLAYERS` order decides** (checked against BitBake master `5495a54`
`lib/bb/cookerdata.py` `parseConfigurationFiles` and `lib/bb/cooker.py`
`handleCollections`):

- Dependencies need not be listed first. `LAYERDEPENDS_<c>` (and `LAYERRECOMMENDS_<c>`) are
  checked in `handleCollections` against the whole `BBFILE_COLLECTIONS` list (`dep in
  collection_list`), after every `layer.conf` was read; a version constraint is compared with
  `LAYERVERSION_<dep>`. Priorities come from `BBFILE_PRIORITY_<c>` or, when unset, from the
  dependencies' priorities (`calc_layer_priority`), never from the position.
- The layer confs are parsed in `BBLAYERS` order into one datastore, so the order is the
  order of `BBPATH .= ":${LAYERDIR}"`, and `BBPATH` decides which file an `include` /
  `require`, an `inherit` and a conf lookup (`conf/machine/...`, `conf/distro/...`) find
  first (see "Finding files"). That is the only effect sous-chef reports
  (`layers show=compat`, `order` rows).
- `LAYERSERIES_CORENAMES` is taken from the first `layer.conf` (in `BBLAYERS` order) after
  which it is set, then deleted; `LAYERSERIES_COMPAT_<c>` of each collection the layer adds
  is read and deleted right after that layer's `layer.conf`, and every compat check runs
  after the last layer.

## Parse order

Configuration, once:

1. `conf/bblayers.conf`
2. every layer's `conf/layer.conf`, in `BBLAYERS` order
3. `conf/bitbake.conf` (found on `BBPATH`), which itself includes, in the order written in
   that file: `conf/site.conf`, `conf/auto.conf`, `conf/toolcfg.conf` (Wrynose),
   `conf/local.conf`, the `addfragments` directive (Walnascar and later; see "Configuration
   fragments"), the machine conf `conf/machine/${MACHINE}.conf`, the distro conf
   `conf/distro/${DISTRO}.conf`, and others.
   **Follow the includes as written; do not hard-code the list.**
4. classes named in `INHERIT`, plus `base.bbclass`

Then for each recipe, on top of a copy of the configuration:

5. the `.bb`, statement by statement, following `require` / `include` / `inherit` at the
   point where they appear
6. each matching `.bbappend`, in layer priority order
7. deferred inherits, anonymous python (not evaluated by us), then finalisation

### Replay

Checked against BitBake `master` 2026-11 (`lib/bb/cookerdata.py` `parseConfigurationFiles`,
`_parse_recipe`; `lib/bb/cooker.py` `collect_bbfiles`, `get_file_appends`,
`CookerCollectFiles.get_layername`; `parse/parse_py/ConfHandler.py` `handle`,
`include_single_file`; `parse/parse_py/BBHandler.py` `handle`, `inherit`; `parse/ast.py`
`MethodNode`, `PythonMethodNode`, `ExportFuncsNode`, `multi_finalize`, `finalize`;
`data_smart.py` `expandVarref`) and OE-Core `master` (`meta/conf/bitbake.conf`,
`meta/classes-global/base.bbclass`). This is what `src/eval/replay.rs` reproduces:

- **`BBPATH`** starts as the build directory, as BitBake's does from the environment (see
  "Finding files").
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
  (`re.match`), patterns tried in reverse lexical order of their text. Live (BitBake
  2.18.0): `bitbake -e curl` shows `set cookerdata.py:513 [_parse_recipe] "core"` after
  the `??= "config"` default. For a `BBCLASSEXTEND` variant (`bitbake -e curl-native`) the
  same line sets `"None"`: no pattern matches the `virtual:native:` file name, the variable
  is printed without a `FILE_LAYERNAME=` line, and that variant's `OVERRIDES` contains a
  literal `layer-${FILE_LAYERNAME}`.
- **bbappends** are parsed after the whole `.bb` (`multi_finalize`, in `__BBAPPEND` order
  = `get_file_appends` = `BBFILES` collection order: lowest layer priority first), each with
  BBHandler as an include. Confirmed in the source, and against `bitbake-layers
  show-appends` on the live environment (see "Finding files" for the string sort of ties).
- **Finalisation** (`ast.finalize`): deferred inherits (`__BBDEFINHERITS`, first in first
  out, including ones added meanwhile), event handler registration, `expandKeys`, then
  anonymous python (`runAnonFuncs`), then `handleVirtRecipeProviders` (BitBake 2.12 and
  later), then task processing (`bb.build.add_tasks`).
- **Recipe virtual providers** (BitBake 2.18.0
  `lib/bb/parse/ast.py` `handleVirtRecipeProviders`, called from `finalize` right after
  `runAnonFuncs` and before `add_tasks`; present on branches `2.12`, `2.16`, `2.18`, absent
  on `2.10`; added by `fb119c788`, 2025-01-21): `depends = (getVar("DEPENDS") or
  "").split()`, `virtprovs = (getVar("BB_RECIPE_VIRTUAL_PROVIDERS") or "").split()`; every
  word of `depends` that is in `virtprovs` becomes `getVar("PREFERRED_PROVIDER_" + word)`
  (empty or unset: `bb.fatal("Error, recipe virtual provider PREFERRED_PROVIDER_%s not
  set")`); then `setVar("DEPENDS", " ".join(newdeps))`, a plain post-parse `setVar`, so the
  `:append` / `:prepend` / `:remove` lists and the active override variants are dropped
  and the words are joined with single spaces. The same is done to the recipe part of each
  `r:t` entry of the `depends` flag of every task in `__BBTASKS`. Only `DEPENDS` and the
  task `depends` flags change (not `RDEPENDS`, not `PROVIDES`). OE-core (Wrynose) sets
  `BB_RECIPE_VIRTUAL_PROVIDERS` to the cross toolchain virtuals (`virtual/cross-cc
  virtual/cross-c++ virtual/cross-binutils` and the `nativesdk-` ones), whose
  `PREFERRED_PROVIDER_*` are per `TARGET_ARCH`. sous-chef (`replay::virtual_recipe_providers`,
  `Feature::RecipeVirtualProviders`) does the `DEPENDS` part after `expand_keys` (it does not
  run anonymous python, so the rewrite sees `DEPENDS` without what python would add, and
  the value stays flagged when it was): the new value is stored by `DataStore::rewrite`,
  recorded in the history as an operation of its own kind (`OpKind::VirtualProviderRewrite`,
  op `rewrite` in the `var` tool), not as an assignment: it points at the
  `PREFERRED_PROVIDER_*` assignment used (or the recipe when none has a location), it is
  `Applied`, and the operations that made the old value keep the status they had just
  before the rewrite (they are what was rewritten, not shadowed). A note names each
  rewritten word (`ReplayNotes::var_notes`). A missing provider is a reference issue
  (`RefProblem::Fatal`) and the word is kept.

  Read again for w8 (BitBake 2.18.0, `lib/bb/parse/ast.py` `handleVirtRecipeProviders`
  lines 494-515, called from `finalize` with `tasklist = d.getVar('__BBTASKS', False) or
  []`): the `setVar("DEPENDS", " ".join(newdeps))` runs **always**, so with no word
  replaced the value is still stored with its words joined by single spaces, and an unset
  `DEPENDS` becomes `""`; an unset `BB_RECIPE_VIRTUAL_PROVIDERS` is an empty list (`or
  ""`). For **each task in `__BBTASKS`**: `taskdeps = (d.getVarFlag(task, "depends") or
  "").split()` (expanded), each entry `r, t = entry.split(":")` (an entry without exactly
  one `:` raises `ValueError`, which fails the parse), `r` in the list becomes
  `d.getVar("PREFERRED_PROVIDER_" + r)` with **no** check (unset gives the text `None`, so
  `None:do_x`; only `DEPENDS` has the `bb.fatal`), and `d.setVarFlag(task, "depends", "
  ".join(remapped))` runs for every task, also one without the flag (it gets `""`). Nothing
  else is rewritten (not `RDEPENDS`, `PROVIDES`, `RRECOMMENDS`, or the `depends` flag of a
  name that is not in `__BBTASKS`). `__BBTASKS` is built by `bb.build.addtask` (from
  `AddTaskNode.eval`: the names are **not** expanded, `ensure_task_prefix` adds `do_`, a
  task already listed is not added again) and `bb.build.deltask` (`DelTaskNode.eval`
  expands the names; the task is removed).

  sous-chef (w8): the replay keeps `__BBTASKS` from `addtask` / `deltask` statements
  (`DataStore::tasks`; tasks anonymous python adds are not seen). `DEPENDS` is stored again
  with single spaces when that changes a certain value (an uncertain one is left flagged),
  and set to `""` when unset, each as an `OpKind::VirtualProviderRewrite` operation at the
  recipe with a note. Each task's `depends` flag is rewritten as above when its value is
  certain and the text changes, recorded as a `VirtualProviderRewrite` operation on the flag
  (at the `PREFERRED_PROVIDER_*` assignment used, else the recipe) with a note under the
  task's name; an entry without one `:` is a `RefProblem::Fatal` issue and that flag is
  left alone. Not reproduced: the `""` BitBake stores on a task without the flag (readers
  treat unset and empty alike).
- **Inherit** (`BBHandler.inherit`): the argument is expanded and split; a word listed in
  `BB_DEFER_BBCLASSES` is deferred (unless already deferred); a class file in
  `__inherit_cache` is not parsed again. Not found is a parse error.
- **Which handler parses an included file, and the class type** (BitBake 2.18.0
  `lib/bb/parse/__init__.py` `handle`: the first handler whose `supports` accepts the
  name; `BBHandler.supports` = `.bb`, `.bbclass`, `.inc`; `ConfHandler.supports` =
  `.conf`; `BBHandler.inherit` reads `d.getVar("__bbclasstype", False)`; `cookerdata.py`
  sets it to `"global"` on the base datastore before the configuration is parsed and to
  `"recipe"` on each multiconfig datastore after it): an `.inc` is always parsed by
  BBHandler, whoever includes it, so its `inherit` works, and searches `classes-global/`
  then `classes/` when the `.inc` is reached during the configuration (from a `.conf`, a
  layer conf, a global class), `classes-recipe/` then `classes/` during a recipe. A `.conf`
  is parsed by ConfHandler, whose `feeder` has no `inherit` / `inherit_defer` (an
  "unparsed line" `ParseError`). sous-chef: the replay's class type is per pass (`global`
  in the configuration pass, recipe in the recipe pass), which is exactly this; (w8) an
  `inherit` in a `.conf` is a `RefProblem::Fatal` issue and is not followed. (w9) The
  index, which resolves without evaluating, gets the same rule from the include edges
  (`index/scan.rs` `insert_inc_inherit_edges`): an `inherit` in an `.inc` is searched in
  global context when a `.conf` or a `classes-global/` class reaches the `.inc` through any
  chain of `require` / `include` / `include_all`, in recipe context when a recipe, a
  bbappend or a `classes-recipe/` class does, in both (`edges.context` saying which search
  found the class, as for a class in `classes/`) when both do or a `classes/` class does, and
  in recipe context when nothing does. An `inherit` in a `.conf` gets no index edge;
  `hygiene/parse-error` reports the line.
- **When a deferred inherit's argument is expanded** (BitBake
  2.18.0 `lib/bb/parse/parse_py/BBHandler.py` `inherit_defer` and `inherit`, `lib/bb/parse/ast.py`
  `InheritDeferredNode.eval` and `finalize`): `inherit_defer` stores the argument **as
  written** (`(expression, fn, lineno)` appended to `__BBDEFINHERITS`); `finalize` pops the
  entries in order and calls `inherit(..., deferred=True)`, which does `d.expand(files)` then,
  with the recipe and its bbappends fully parsed, so `${TOOLCHAIN}` is read with the recipe's
  own value (`kernel-arch.bbclass` sets `TOOLCHAIN = "gcc"`) and `getVar` semantics
  (override variants, `:append`). A word with `/` is a path under the class directories:
  `toolchain/gcc` is `classes-<type>/toolchain/gcc.bbclass` or `classes/toolchain/gcc.bbclass`
  along `BBPATH`, as for any name without `.bbclass`. OE-core Wrynose `base.bbclass:22-36`:
  `PREFERRED_TOOLCHAIN = "${PREFERRED_TOOLCHAIN_TARGET}"` with `:class-native`,
  `:class-cross`, `:class-crosssdk`, `:class-nativesdk` variants, `TOOLCHAIN ??=
  "${PREFERRED_TOOLCHAIN}"`, `inherit_defer toolchain/${TOOLCHAIN}`; `classes/toolchain/gcc.bbclass`
  sets `TCOVERRIDE = "toolchain-gcc"`, which `OVERRIDES` (`bitbake.conf:800`) references.
  So when BitBake expands `${TOOLCHAIN}`, `TCOVERRIDE` is still unset and `OVERRIDES` holds
  the literal `${TCOVERRIDE}` (no `class-*` override can come from it).
  
  sous-chef (`Replay::expand_ref`): a reference is refused (recorded in `references`) only
  when the expanded text itself still holds `${@...}` or an undefined `${VAR}`: then the file
  is not known. When the text is complete but the value is flagged (an override variant was
  judged inactive while `OVERRIDES` holds python or unresolved names, or a variable python
  may change), the file is read, the reference is recorded in
  `ReplayNotes::uncertain_references`, and every variable the files read under it change is
  flagged `unevaluated_python` (`ReplayNotes::uncertain_vars`; unresolved names go to
  `unresolved_vars`). An unresolved name that the file itself sets (`TCOVERRIDE`) is dropped
  from that uncertainty, because it was unset when BitBake expanded the argument too; if
  nothing else is uncertain the reference counts as exact. Not covered: variables only
  another candidate (`toolchain/clang`) would have set are not flagged.
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
- **No machine at all.** When neither the configuration (including fragments and, in a
  kas project without a build directory, the kas defaults, see "kas") nor the caller sets
  `MACHINE`, the evaluation carries the note "no MACHINE is set ... machine-specific values
  are not applied" (`ReplayNotes::target_notes`, printed by `var`, `overrides`, `deps` and
  the evaluated `recipe` views).

sous-chef differences (in `replay.rs`): `LAYERDIR` is the canonical layer root, not the
`BBLAYERS` text. `expandVarref` is done by replacing the reference inside each stored piece
(value, `??=` default, `:append` / `:prepend` / `:remove` texts) rather than folding the
appends into the value; final values are the same except where an override variant would
have been detached. `PN`, `PV`, `BPN`, `FILE_DIRNAME` and `THISDIR` are computed as above
only in the recipe pass and only while their stored value is python or absent (the python
is assumed to be OE-Core's); `BP` is set to `${BPN}-${PV}` when absent. Target `MACHINE` /
`DISTRO` are set before `bblayers.conf` and again after every configuration assignment to
them. A function's stored value is a pointer per piece instead of the body. A reference
whose expanded text still needs python or an undefined variable is not followed (one whose
text is complete but whose value is uncertain is followed and flagged, see above; BitBake would expand the
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

Checked against BitBake master (2026-11), `lib/bb/providers.py`
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
  front and marks the choice unique. There is a `PREFERRED_PROVIDERS`
  variable (BitBake 2.18.0 source): `cooker.handlePrefProviders` reads its words
  (`providee:provider`, an error for conflicting pairs) from the configuration into
  `recipecaches.preferred`; `filterProviders` overwrites that entry with
  `PREFERRED_PROVIDER_<name>` when it is set, then uses it as above. Without a unique
  choice and with several eligible files BitBake fires `MultipleProviders`
  (`taskdata.add_provider_internal`), adds every eligible file as a build target, and the
  runqueue uses `build_targets[name][0]`: **the first one** (settled in
  `taskdata.py` and `runqueue.py`, BitBake 2.18.0).

sous-chef differences: the candidates are the recipes whose `PN` or static `PROVIDES` (also
in an include, class or bbappend of the recipe) names the item in the index, confirmed by
evaluating each; a `PROVIDES` word that needs expansion, or one from a global class, is not
found. The file order inside a bucket is layer then path, not BitBake's collection order.
`DEFAULT_PREFERENCE`, `PE` and `PR` come from evaluating each candidate only when some
non-`.conf` file (or a `.conf` statement with an override) sets one of them; values set by
anonymous python are not seen. `BBMASK`, `SKIP_RECIPE` and `COMPATIBLE_*` are not applied,
and `BBCLASSEXTEND` variants (`-native`, `nativesdk-`) are not providers.
`PREFERRED_PROVIDERS` is not read.

Confirmed on the live environment (used by `org/policy-in-recipe`; BitBake 2.18.0 `providers.filterProviders(...,
cfgData, ...)` and `cooker.handlePrefProviders`, and observed: `bitbake -e virtual/kernel`
gives `FILE=.../meta-kiss/recipes-kernel/linux/linux-kiss_7.0.bb` from
`PREFERRED_PROVIDER_virtual/kernel ?= "linux-kiss"` in the machine conf):
`PREFERRED_PROVIDER_*` is read only from the configuration datastore, so a recipe's own
value never takes part in the choice.

### `PACKAGECONFIG`

Checked against OE-Core `master` 2026-11 `meta/classes-global/base.bbclass` (anonymous
python) and the Yocto reference manual (`PACKAGECONFIG`, 6.1): `PACKAGECONFIG[f] =
"a,b,c,d,e,g"`, up to six comma-separated fields (more is a `bb.error`): 1 extra configure
arguments (`PACKAGECONFIG_CONFARGS`) when `f` is in `PACKAGECONFIG`, 2 the same when it is
not, 3 build dependencies (`DEPENDS`), 4 runtime dependencies (`RDEPENDS:${PN}`), 5 runtime
recommendations (`RRECOMMENDS:${PN}`), 6 conflicting options (an enabled conflict is fatal).
Flags are iterated in sorted order; a word of `PACKAGECONFIG` with no flag is ignored; no
flag name is special. The additions are made by anonymous python, which sous-chef does not
run: `deps` lists them from the flags and says so.

**BitBake's own flags on `PACKAGECONFIG`** (OE-core Wrynose
`meta/classes-global/base.bbclass` anonymous python lines 471-549, BitBake 2.18.0
`lib/bb/data_smart.py` `getVarFlags`, `lib/bb/data.py` `build_dependencies`,
`lib/bb/cooker.py` `getAllKeysWithFlags`): `pkgconfigflags = d.getVarFlags("PACKAGECONFIG")`
returns every flag except the internal ones (names starting with `_` or `:`), and the loop
skips none, so OE-core's own `PACKAGECONFIG[vardepvalue] = "${PACKAGECONFIG}"`
(`bitbake.conf:942`) and `PACKAGECONFIG[doc] = "..."` (`documentation.conf:311`) go through
it like options. Each adds nothing unless its name is a word of `PACKAGECONFIG` (then field 1,
3, 4, 5) or its text has a non-empty second comma field (then that field goes to
`PACKAGECONFIG_CONFARGS`). Those flag names are BitBake's: `doc` (documentation; the
`getAllKeysWithFlags(["doc", "func"])` dump), `vardeps`, `vardepvalue`, `vardepsexclude`,
`vardepvalueexclude`, `exports`, `prefuncs`, `postfuncs`, `lineno`, `filename`
(`build_dependencies`, the task signature), `export`, `unexport`, `func`, `python`. The `deps`
tool (`kind=packageconfig`) lists such a flag as an option only when it would add something
by the rule above, and names the others in a note; a `${@...}` word of `PACKAGECONFIG` is
one row, "unknown", never an option (see "Splitting list values").

## Files and patches

`file://` entries in `SRC_URI` are searched along `FILESPATH`, built from `FILESEXTRAPATHS`
and the directories `${BP}`, `${BPN}` and `files` next to the recipe, each also tried with
override subdirectories. A bbappend that adds files must do:

```
FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"
```

(immediate expansion and the trailing colon both matter).

**Which `FILESEXTRAPATHS` forms keep the list** (checked against BitBake 2.18.0
`6e260eb6` `lib/bb/parse/ast.py` `DataNode.eval` and OE-core Wrynose `33a6bd6c`
`bitbake.conf:387` `FILESEXTRAPATHS ?= "__default:"`, `utils.bbclass` `base_set_filespath`,
`insane.bbclass` "FILESEXTRAPATHS-variable, must always use :prepend"):
`base_set_filespath` removes the text `__default:`, splits the rest on `:` and skips empty
entries; insane.bbclass warns when `__default` is no longer one of the `:`-split entries.
`predot` (`=.`) stores `value + old` and `postdot` (`.=`) `old + value`, unexpanded, at parse
time. So, in a recipe, `.inc`, bbappend or class (all parsed after `bitbake.conf`):

| Form | Result | Right? |
|---|---|---|
| `:prepend = / := "<p>:"` | `<p>:` before the final value | yes |
| `:append = / := ":<p>"` | `:<p>` after it | yes |
| `=. "<p>:"` | `<p>:__default:` | yes |
| `.= ":<p>"` | `__default::<p>` (the empty entry is skipped) | yes |
| `=. "<p>"` / `:prepend = "<p>"` | `<p>__default:`: the marker and the path are glued | no |
| `.= "<p>"` / `:append = "<p>"` | right after the default by luck, glued to any earlier append | no |
| `=`, `:=` | the default is gone | no |
| `+=`, `=+` | a space joins `<p>` to the neighbouring entry | no |
| `FILESEXTRAPATHS:<cond> =. "<p>:"` | builds `FILESEXTRAPATHS:<cond>` = `<p>:`, which replaces the list when `<cond>` is active | no |

In a `layer.conf` (parsed before `bitbake.conf`) `=.` / `.=` set the variable before the
`?=`, which then does nothing, so the default is lost; `:prepend` / `:append` are applied
at read time and are fine anywhere. `=.` / `.=` cannot be combined with `:=`, so the value
stays unexpanded: `${THISDIR}` (`base.bbclass`: `os.path.dirname(d.getVar('FILE'))`) and
`${FILE_DIRNAME}` expand late, to the directory of the `.bb`; that is right in the `.bb` and
in an `.inc` of the same directory, wrong in a bbappend or an `.inc` elsewhere.

Checked against OE-Core `9518ce9` and BitBake `5495a54`; this is what `recipe
show=sources` reproduces:

- **`FILESPATH`** is set in `meta/classes-global/base.bbclass`:
  `FILESPATH = "${@oe.utils.base_set_filespath(["${FILE_DIRNAME}/${BP}",
  "${FILE_DIRNAME}/${BPN}", "${FILE_DIRNAME}/files"], d)}"`, with `FILESEXTRAPATHS ?=
  "__default:"` and `FILESOVERRIDES = "${TRANSLATED_TARGET_ARCH}:${MACHINEOVERRIDES}:${DISTROOVERRIDES}"`
  in `bitbake.conf`. `base_set_filespath(path, d)` (`meta/lib/oe/utils.py`): removes
  `__default:` from `FILESEXTRAPATHS`; when the rest is not empty, its `:`-split entries go
  before `path`; the overrides are `(":" + FILESOVERRIDES).split(":")` **reversed** (so the
  last override first and the empty override last; an empty `FILESOVERRIDES` gives two empty
  overrides and every directory twice); the result is, **override by override, every path**
  `os.path.join(p, o)` (empty `p` skipped), joined with `:`. So an override directory of the
  recipe's `files/` is searched before a plain `FILESEXTRAPATHS` directory. `FILE_DIRNAME` is
  the recipe's directory (`FILE` is the `.bb` when the fetcher expands it). sous-chef takes
  the path list from the arguments as expanded in the store and the other values evaluated;
  it says the order is reconstructed.
- **Lookup** (`lib/bb/fetch/local.py` `localfile_searchpaths`): a `file://` path starting
  with `/` is used as it is; otherwise `bb.utils.which(FILESPATH, path)`: the first
  `<entry>/<path>` that exists (`os.path.exists`, so a directory matches). The path is the
  URL's location before the first `;` (`file://a/b.patch;striplevel=2` is `a/b.patch`).
- **Patches** (`meta/lib/oe/patch.py` `patch_path`, `src_patches`): any `SRC_URI` entry is
  a patch when its `apply` parameter is true (`oe.types.boolean`: `yes y true t 1`), or,
  without `apply`, when its extension after stripping `.gz` / `.bz2` / `.xz` / `.Z` is
  `.patch` or `.diff`; a local directory never is. (`mindate` / `maxrev` / `maxver`-style
  parameters can skip a patch at `do_patch` time: `should_apply`; sous-chef does not apply
  them.)
- **`Upstream-Status`** (`meta/lib/oe/qa.py` `check_upstream_status`, used by the
  `patch-status` QA check): a line matching
  `^Upstream-Status: (Pending|Submitted|Denied|Inappropriate|Backport|Inactive-Upstream)( .+)?$`
  (multiline, case-sensitive) is valid; else a line matching `upstream.*status`
  case-insensitively is "Malformed"; else "Missing".

## Tasks

Checked against BitBake `5495a54` (`lib/bb/parse/ast.py` `AddTaskNode`, `DelTaskNode`,
`finalize`; `lib/bb/build.py` `addtask`, `deltask`, `add_tasks`); `recipe show=tasks`
reproduces it from the indexed statements:

- `addtask a b after x before y`: for each name, `ensure_task_prefix` (`do_` added), the
  `task` flag set, appended to `__BBTASKS` if not there; each `after` name (prefixed) is
  appended to the task's `deps` flag if missing; for each `before` name, the task is
  **prepended** to that task's `deps` if missing (even when that task does not exist yet).
  The names are not expanded (`add_tasks` expands them later); a repeated `addtask` only adds
  dependencies.
- `deltask a`: the argument is expanded and split; each task is removed from `__BBTASKS`
  (with its `task` flag), its `deps` flag deleted, and it is removed from every remaining
  task's `deps`.
- The statements run where they are in parse order (a class's `addtask` at its `inherit`),
  so the configuration classes (`base`) come first. Anonymous python runs before
  `add_tasks` and can call `bb.build.addtask` / `deltask`; `do_x[deps]` can also be set as a
  flag directly; neither is seen statically.

## Build output

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
  `bitbake.conf`.) **Confirmed live** (BitBake 2.18.0, OE-core `33a6bd6c`, kas
  build of yocto-kiss): `bitbake-getvar --value TMPDIR` is `<build>/tmp` (the yocto-kiss
  README's `tmp-glibc` is out of date); `bitbake-getvar -r curl --value T` prints only
  `<build>/tmp/work/armv7at2hf-neon-oe-linux-gnueabi/curl/8.19.0/temp`. After `-c fetch`,
  `T` holds `log.do_<task>.<pid>` and `run.do_<task>.<pid>` each with its symlink,
  `run.<function>.<pid>` without a symlink for every helper function run
  (`base_do_fetch`, `extend_recipe_sysroot`, `sstate_*`, ...), `depsig.do_<task>.<pid>`
  with a symlink, and `log.task_order` with one line per task:
  `<YYYYMMDD-HHMMSS.ffffff> do_<task> (<pid>): log.do_<task>.<pid>`.
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
    `Fetcher failure: <msg>`. Seen in a real log (BitBake 2.18.0, wget, an
    unresolvable `https://` URL; `tests/fixtures/outputs/live/broken-fetch/`): that line is
    preceded by `ERROR: Fetcher failure: Fetch command <export ...; /usr/bin/env wget
    --tries=2 --timeout=100 --output-document=... --continue --directory-prefix=... '<url>'
    --progress=dot --verbose> failed with exit code 4, see logfile for output` (the
    upstream attempt; exit 4 is wget's network failure); wget's own lines (`Resolving
    <host> ... failed: Name or service not known.`, `ERROR 404: Not Found.`) appear
    unprefixed after each `DEBUG: Running export ...` line; OE-core's `MIRRORS` are tried
    after upstream (`downloads.yoctoproject.org/mirror/sources/`,
    `mirrors.edge.kernel.org/yocto-sources/`), each with `DEBUG: Mirror fetch failure for
    url <mirror url> (original url: <url>)`. On the console the two ERROR lines carry the
    `<PF> do_fetch: ` prefix, then `Logfile of failure stored in:`, the runqueue line and
    the summary, as described above; no `Log data follows:`. What the `log` digest takes
    from this: the `Fetch command ... failed with exit code <n>, <output>` text is
    `runfetchcmd`'s (`lib/bb/fetch2/__init__.py`, BitBake 2.18 `6e260eb6`: `<output>` is
    `output:\n<stdout>\n<stderr>`, or `see logfile for output` when the command's output
    went to the log); the cause a user needs is the downloader's own first error line
    (`wget: unable to resolve host address '<host>'`, `<date> <time> ERROR 404: Not
    Found.`), not the command. The task's **exit status is not in the task log**: only
    `ERROR: Task (<file>:<task>) failed with exit code '<n>'` on the console
    (`runqueue.py` `TaskFailure`) carries it, so it is read from the build's console log
    (`BB_CONSOLELOG`) or a job's output. `do_fetch` itself is `base_do_fetch` of the global
    `base.bbclass` (`EXPORT_FUNCTIONS do_fetch`), which no recipe's inherit tree contains:
    the task-definition pointer looks in `base` and the static `INHERIT` classes too.
  - Patch: `NOTE: Applying patch '<name>' (<path>)`, then on failure
    `ERROR: Applying patch '<file>' on target directory '<dir>'` followed by the repr of
    the patch tool's `CmdError` (quilt's output with `\n` turned into newlines).
  - QA: `ERROR: QA Issue: <message> [<class>]` (or `WARNING:`), then
    `ERROR: Fatal QA errors were found, failing task.` (`meta/lib/oe/qa.py`).
  - The compiler, configure, make, wget and quilt messages inside logs are those tools'
    own formats; they were not checked against a source.

### `bitbake -e` output

Confirmed against real output of BitBake 2.18.0 (`bitbake -e curl` on the live
environment, kas 5.5, OE-core `33a6bd6`; trimmed copy in
`tests/fixtures/outputs/envdump/`) and against the code that writes it in that BitBake:
`lib/bb/cooker.py` `showEnvironment`, `lib/bb/data.py` `emit_var` / `emit_env`,
`lib/bb/data_smart.py` `IncludeHistory.emit` / `VariableHistory.emit` and the `op` names
recorded by `lib/bb/parse/ast.py`, `data_smart.py` and `data.py`. The parser is
`bitbake::envdump`. In order:

1. **Leading noise.** Whatever ran first prints before the dump. Through `kas shell`:
   kas's log lines (`2026-11-17 16:28:01 - INFO     - kas 5.5 started on ...`, one per
   repository, `To start the default build, run: ...`); then the cooker's `NOTE:
   Reconnecting to bitbake server...`, `Loading cache...done.`, `Loaded 0 entries from
   dependency cache.`, `Parsing recipes...done.`, `Parsing of N .bb files complete ...` and
   `NOTE: Resolving any missing task queue dependencies`. A job captures stdout and stderr
   together, so these are interleaved as printed.
2. **Include history**: `#`, `# INCLUDE HISTORY:`, `#`, then one `# <file>` per
   configuration file, indented two spaces per level, with ` includes:` after a file that
   includes others. Paths are as BitBake opened them: absolute and not normalised
   (`.../build/../openembedded-core/meta/conf/layer.conf`), or relative to `BBPATH` for
   files found there (`conf/site.conf`). Classes inherited globally are part of it.
3. **Variables**: every key that is not a function (`func` flag), sorted by name, each as a
   block. Keys starting `__` are left out. Override variants are separate keys and blocks
   (`ABIEXTENSION:class-crosssdk`, `T:task-clean`, `FILES:curl`), and names may hold `:`,
   `-`, `.`, `/` (`PREFERRED_PROVIDER_virtual/kernel`).

   ```
   #
   # $FILE_LAYERNAME [2 operations]
   #   set /…/build/../openembedded-core/meta/conf/bitbake.conf:801
   #     [_defaultval] "config"
   #   set cookerdata.py:513 [_parse_recipe]
   #     "core"
   # pre-expansion value:
   #   "core"
   FILE_LAYERNAME="core"
   ```

   - The header is `# $NAME` with one operation and `# $NAME [N operations]` with more
     (never `[1 operations]`). A variable without history prints `#   [no history
     recorded]`, `#`, `#   "<value>"` instead of operations.
   - Each operation is `#   <op> <file>:<line>`, then `#     ` and the detail in double
     quotes, prefixed with `[<flag>] ` for a flag operation. The detail is the value as
     written in the statement (`+= "x"` records `x`), not the result. Multi-line details
     continue on lines starting `#     `. Nothing inside the quotes is escaped.
   - When python code recorded the operation (`d.appendVar`, `expandKeys`, the cooker),
     the file is only a basename and ` [<function>]` follows the line number
     (`append base.bbclass:508 [appendVar]`, `rename from FILES:${PN} data.py:97
     [expandKeys]`).
   - Operation names seen: `set` (`=`, `setVar`), `set?` (`?=`), `immediate` (`:=`),
     `append` (`+=`, or `appendVar` / `appendVarFlag`, which add no space), `prepend`,
     `postdot` (`.=`), `predot` (`=.`), `:append`, `:prepend`, `:remove` (the override
     operators; with a condition `:append[class-target]` for `VAR:append:class-target`),
     `exported` (`export VAR`, a set of the `export` flag), `del`, `delFlag`, `rename from
     <old key>`, `rename (to)`, `from env` and `auto env export` (inherited from the
     environment BitBake started in). `??=` is not an operation of its own: it is `set`
     of the flag `_defaultval` (`_defaultval_flag_<f>` for `VAR[f] ??=`).
   - Flag operations (`[doc]`, `[vardeps]`, `[sha256sum]`, `[export]`, `[unexport]`,
     PACKAGECONFIG options) are part of the same history. No final flag value is printed.
   - The base block repeats each override variant's operations, prefixed
     `override[<override>]:` (`override[class-native]:set`, `override[curl-dbg]:rename
     from ALLOW_EMPTY:${PN}-dbg`); the variant also has its own block. The variable's own
     operations come first, in the order they ran.
   - `# pre-expansion value:` and `#   "<value>"` follow only with more than one
     operation. The value is the unexpanded one; continuation lines start with `#` alone
     (no spaces), and an unset value prints `"None"`.
   - The final line is `NAME="<value>"`, or `export NAME="<value>"` when the `export` flag
     is set (`AR`, `PWD`, `systemd_unitdir`). The value is expanded; `"`, backtick and `$`
     are escaped with a backslash and each newline becomes ` \` plus a newline (backslashes
     are not escaped). **`unset NAME` means the `unexport` flag is set** (`SHELL`), not that
     the variable is unset; no value is printed then. **A variable whose value is `None`
     prints no final line at all**: only flags were set (`ALTERNATIVE`, `[doc]` only), or
     every value operation was conditional (`ALLOW_EMPTY`, only package overrides).
   - A variable whose expansion raised prints `# expansion of NAME threw <Error>:
     <message>` and no block (not seen in the recording; from `emit_var`).
4. **Shell functions**, sorted, directly after the last variable (no `#` before them):
   `# line: <n>, file: <file>`, `NAME() {`, the expanded body, `}`, a blank line. An
   `EXPORT_FUNCTIONS` stub has `# line: 1, file: autogenerated`. Functions have no history
   block, and names containing `-`, `.`, `+`, `{` or `}` are not printed.
5. **Python functions**, sorted, last: a blank line, `python NAME () {`, the body as
   stored, `}` (on the body's last line when the body does not end with a newline), a
   blank line. No location is printed. Anonymous python is printed under its mangled name
   (`__anon_<line>_<path with / . - as _>`), so these names encode the absolute path.

## Running BitBake

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
- **Server and cancel** (settled on the live environment, BitBake 2.18.0, kas build of yocto-kiss): the
  server started by a job (`bitbake-server decafbad ...`, two processes) is re-parented
  away from the job and runs in its own session and process group, so a cancel, which
  terminates the job's group, does not reach it. It was still running (and parsing) right
  after the cancel and exited by itself about 3 s later ("No timeout, exiting.",
  "Exiting as we could obtain the lock"), because with `BB_SERVER_TIMEOUT` unset the server
  leaves once its client is gone. With `BB_SERVER_TIMEOUT` set it would stay (not tried).
- **A second client** (settled on the live environment): while `bitbake -e curl` ran, `bitbake-layers
  show-layers` started in a second shell printed `NOTE: Reconnecting to bitbake server...`,
  **waited**, and completed normally (exit 0) half a second after the first client
  finished. `lib/bb/main.py`: a client that finds `bitbake.lock` taken connects to the
  running server's socket, retrying up to 8 times (5 s apart after the fourth try), then
  fails with "Unable to connect to bitbake server, or start one" (that failure was not
  observed). The tool still runs one job at a time.

## Build environments

How a shell gets a working BitBake environment, for the `bitbake` tool's runner
(`src/bitbake/env.rs`).

### bitbake-setup builds

Checked 2026-11-17 in BitBake's `bin/bitbake-setup` on branches `2.18` and `master`
(`_make_init_build_env`) and `doc/bitbake-user-manual/bitbake-user-manual-environment-setup.rst`
(master); confirmed on bitbake-setup 2.19.0 in the live environment. Each setup's build directory gets
`init-build-env`:

```
# init-build-env wrapper created by bitbake-setup
cd <directory holding oe-init-build-env>
set <build directory>
. ./oe-init-build-env
set --
```

(plus an `export BB_ENV_PASSTHROUGH_ADDITIONS=...` block when the configuration has
`bb-env-passthrough-additions`). It is sourced with no arguments and leaves the shell in the
build directory with `BUILDDIR`, `BBPATH`, `BB_ENV_PASSTHROUGH_ADDITIONS`, `OE_ADDED_PATHS`,
`PYTHONPATH` set and `PATH` extended. Bash keeps positional parameters that a sourced file
changes even when `source` was given arguments (checked with bash 5.3: after
`source s.sh x` where `s.sh` runs `set --`, `$#` is 0), so a wrapper of the form
`source "$1" "$2" && shift 2 && exec "$@"` loses the program with this script (live
verification). The `bitbake-setup` mode (a script named `init-build-env`, found in the
build directory or configured as `init_script`) saves the argv in an array before
sourcing, and so does the `init-script` wrapper
(`__sous_chef_argv=("$@"); source "$1" "$2" && exec "${__sous_chef_argv[@]:2}"`), so an
`init_script` under another name that ends with, or sources a script ending with, `set --`
works too (`tests/bitbake_env.rs`, `runs_through_a_script_that_ends_with_set_dashdash`). The setup directory is
`bitbake-builds/<config>-<machine>`; `conf/` holds `bblayers.conf` with absolute
paths, a `local.conf` with only a comment, `site.conf` as a symlink, and fragments in
`conf/toolcfg.conf`.

### The Wrynose build environment

**Settled** (2026-11-17, live environment, bitbake-setup 2.19.0, kas 5.5,
BitBake 2.18.0, bash 5.3): a `bitbake-setup` build has `<setup dir>/build/init-build-env`,
a script to source that takes no arguments: `cd <openembedded-core>; set <build dir>; .
./oe-init-build-env; set --` (`tests/fixtures/outputs/live/bitbake-setup-build/init-build-env`).
So `oe-init-build-env <build dir>` still works for such a build, and it also works for a kas
build directory (the live verification ran `bitbake-getvar` and `bitbake-layers` through it there, without
touching `conf/`). The final `set --` empties the caller's positional parameters when the
script is sourced from a function or `bash -c` that relies on `$@`. kas builds are entered
with `kas shell` (see live-environment.md); inside it `BUILDDIR` and `BBPATH` are set and
`bitbake` is on `PATH`. Earlier notes: the 6.0 migration guide only says
VSCode setup was dropped from `oe-init-build-env` ("Users are advised to use bitbake-setup
instead") and that the `meta-poky/conf/templates/default` templates moved to OE-core's
`meta` layer. The 6.0 release notes mention `bitbake-setup init` (it writes
`build/conf/auto.conf`) and that devtool ide-sdk looks for "bitbake-setup's init-build-env
first, and oe-init-build-env if not found". So a `bitbake-setup` build directory appears to
have its own `init-build-env` script (confirmed above).

## kas

Checked 2026-11-17 against kas **5.5** (`kas/__version__.py` on `master` that day): the
documentation pages https://kas.readthedocs.io/en/latest/userguide/project-configuration.html,
https://kas.readthedocs.io/en/latest/userguide/env-vars.html,
https://kas.readthedocs.io/en/latest/userguide/kas-container.html and the command pages
(`kas shell`, `build`, `checkout`, `dump`, `lock`), read from their sources in the kas
repository (`docs/userguide/*.rst`, `environment-variables.inc`, `docs/_man/*`), and where
the documentation is silent the code of the same revision: `kas/includehandler.py`,
`kas/config.py`, `kas/repos.py`, `kas/libcmds.py`, `kas/libkas.py`, `kas/context.py`,
`kas/plugins/shell.py`, `kas/plugins/build.py`, `kas/schema-kas.json` and the
`kas-container` script. Observed on a real kas 5.5 checkout of yocto-kiss (`wrynose`): the generated files, `CACHEDIR.TAG`, the lock
file name, the layer order. `src/workspace/kas.rs` implements what follows.

**Files and merging.**

- Formats: YAML (`.yml`, `.yaml`) and JSON (`.json`), chosen by extension; one document
  (`yaml.safe_load`, PyYAML, YAML 1.1). Every file needs `header.version`.
- `a.yml:b.yml` on the command line is the same as a file including both, in order. The
  files must all be in one repository or all outside version control.
- Merge order of one file (`IncludeHandler._internal_include_handler`): its lock file
  `<stem>.lock<.ext>` next to it, if present; then each entry of `header.includes`, depth
  first, in order; then the file itself. All collected configs are merged left to right
  (`_internal_dict_merge`): mappings recursively, keys keep their first position and new
  keys go last; any other value is replaced. So later files win, and the including file
  wins over its includes. `header.version` becomes the highest one.
- A string include is relative to the repository root of the including file; if that file
  does not exist kas falls back to the including file's directory (with a warning). A
  `{repo, file}` include is relative to that repository and is only followed once the
  repository is known (kas iterates checkout and parsing until every repository is
  resolved). Both must resolve inside the repository (`sanitize_include_path`), else kas
  fails. sous-chef never checks out, so a `{repo, file}` include of a repository not on disk
  is listed as a problem, and anything outside is reported and not read.
- The repository root ("top repo path") is `git rev-parse --show-toplevel
  --show-superproject-working-tree` (the superproject of a submodule), else `hg root`, else
  the first file's directory. sous-chef finds it without running git: the nearest ancestor
  with a `.git` directory, `.hg`, or a `.git` file whose `gitdir` is not under `modules/`
  (a worktree); a submodule's `.git` file continues to the superproject.
- `.config.yaml` in `KAS_WORK_DIR` is read when no file is given; only `kas menu` writes it
  (kas writes none on checkout, shell or build: observed).
- Lock files: `kas lock` / `kas dump --lock` write `<stem>.lock.yaml` next to the kas file
  (observed: `kas/kiss.lock.yaml`); it pins with `overrides.repos.<id>.commit`.

**Repositories and layers** (`Repo.factory`, `_create_layers_from_dict`, schema).

- `repos.<id>`: null or a mapping with `name` (default the id), `url`, `type`, `commit`,
  `branch`, `tag`, `refspec` (deprecated, exclusive with the other three), `path`,
  `layers`, `patches`, `signed`, `allowed_signers`. `defaults.repos.{branch,tag,refspec}`
  apply when the repo does not set the key (a key set to null removes the default).
  `overrides.repos.<id>.{commit,branch,url}` replace those of repositories that exist.
- Path: no `url` and no `path`: the top repo path (the project itself; kas performs no
  version control operation). `url` without `path`: `<kas work dir>/<name>`. A relative
  `path` is under the kas work dir. kas therefore clones next to the kas files'
  repository root when kas is started there (observed: `yocto-kiss/openembedded-core`).
- `layers` (kas 5.5 `repos.py` `Repo.factory`: `repo_config.get('layers', {'': None})`,
  then `_create_layers_from_dict`, read 2026-11-18): missing means the repository itself;
  an empty mapping `{}` adds **no** layer (the schema's description says "missing or
  `None` or an empty dictionary" is the repository itself; the code adds one layer per
  entry, so none); null fails kas's schema check (`layers` has `"type": "object"`; every
  file is validated on load, `includehandler.py` `ConfigFile.load`), so kas stops, and
  sous-chef lists it as a problem and adds no layer. `.` is the repository itself; a value
  null or `{prio: n}` adds the layer (`prio` default 0), `disabled` removes it, and so do
  the deprecated values kas compares by Python equality with `['excluded', 'n', 'no',
  '0', 'false', 0]`: those strings, and any plain scalar YAML 1.1 reads as false or as an
  integer 0 (below).
- `BBLAYERS` order: `prio` descending, then repository name, then layer name (`RepoLayer.
  __lt__`); observed on yocto-kiss (`meta-arm/meta-arm` before
  `meta-arm/meta-arm-toolchain`, though `kiss.yaml` lists them the other way round).
- `machine`, `distro`, `target`, `task` default to `qemux86-64`, `nodistro`,
  `core-image-minimal` and `build` (`schema-kas.json`). The environment variables override
  the files (kas 5.5 `config.py`, read 2026-11-18 in the installed package):
  `get_machine` / `get_distro` return `os.environ.get('KAS_MACHINE' / 'KAS_DISTRO', <file
  value or default>)`, so a set variable wins even when empty; `get_bitbake_task` returns a
  command-line `-c` first, else `os.environ.get('KAS_TASK', ...)` the same way;
  `get_bitbake_targets` returns a command-line `--target` first, else the
  whitespace-separated words of `KAS_TARGET` when there is at least one, else the files'
  `target` (a string or a list). Where they act: `MACHINE` and `DISTRO` only in the
  `local.conf` that `write_bbconfig` writes (`MACHINE ??= "<m>"`, `DISTRO ??= "<d>"`); the
  targets also in `BBMULTICONFIG` there (the `mc:` / `multiconfig:` ones); task and targets
  in `kas build`'s command (`bitbake -c <task> <targets>`) and in `kas shell`'s log line.
  So once `local.conf` exists, an override acts only through a kas command that rewrites
  it (not `kas shell -k`, which sous-chef runs). `kas-container` forwards each of the four
  into the container when it is set and not empty. sous-chef reads them from the
  environment it was started in (`KasEnv::from_process`, `KasOverrides`, not tied to a
  directory): `KasProject::machine_or_default`, `distro_or_default`, `targets_or_default`
  and `task_or_default` follow them, and `status` shows a `kas_env` row per override.

**Generated files** (`WriteBBConfig`; observed). Both are rewritten by `kas checkout`,
`kas shell` and `kas build` (unless `-k` / `--keep-config-unchanged`) and carry no
"generated" header:

- `bblayers.conf`: every `bblayers_conf_header` entry as `# <id>` then its text, in id order
  (sorted keys), then `BBLAYERS ?= " \` with one `${TOPDIR}/<relative path> \` line per
  layer, the closing `"`, `BBPATH ?= "${TOPDIR}"`, `BBFILES ??= ""`.
- `local.conf`: every `local_conf_header` entry the same way, then `MACHINE ??= "<m>"`,
  `DISTRO ??= "<d>"`, `BBMULTICONFIG ?= "<mc names of mc: targets>"`, and for
  `isar-rootless` `ISAR_ROOTLESS ?= "1"`.
- The build directory gets `CACHEDIR.TAG`, whose last line is `# kas build directory`
  (observed; the other lines are the cache-directory-tag signature and comments).
- **Before the first checkout** (no build directory, so no `local.conf`) the evaluator
  stands in for the `MACHINE ??=` and `DISTRO ??=` lines (kas 5.5 `libcmds.py`
  `WriteBBConfig._write_local_conf`: the `local_conf_header` entries, then `MACHINE ??=
  "{get_machine()}"`, `DISTRO ??= "{get_distro()}"`, `BBMULTICONFIG ?= "..."`): where
  `conf/bitbake.conf` includes `conf/local.conf` (OE-core Wrynose `bitbake.conf:817`
  `include conf/local.conf`; kas's build directory is first on `BBPATH`, so the generated
  file is the one found), the replay gives `MACHINE` and `DISTRO` the weak defaults
  `KasProject::machine_or_default` / `distro_or_default` (no history entry: there is no
  file), with `??=` semantics: an earlier `??=` loses to them, a later one beats them, any
  other assignment wins, and nothing parsed before the include sees them. When the final
  value is that default, the evaluation says so (`ReplayNotes::target_notes`: `MACHINE =
  "board1" from the kas file kas/board.yaml:5 ...`). When `conf/bitbake.conf` is not found
  at all (before the checkout OE-core is not there) they are applied where it would have
  been parsed; a `bitbake.conf` that never includes `conf/local.conf` gets nothing (BitBake
  would not read kas's file). Not stood in for: the `local_conf_header` entries and
  `BBMULTICONFIG ?=`. With a build directory nothing is added: kas wrote `local.conf`.

**Directories and environment** (environment-variables page; `context.py`).

- `KAS_WORK_DIR` (default: the current directory) and `KAS_BUILD_DIR` (default
  `${KAS_WORK_DIR}/build`) are read by kas and **not exported** into the build environment
  (observed: no `KAS_*` variable inside `kas shell`). Inside `kas shell` kas sets
  `BUILDDIR` and `BBPATH` (the build directory), a minimal `PATH`, `PYTHONPATH`,
  `OE_ADDED_PATHS`, `BB_ENV_PASSTHROUGH_ADDITIONS`, and `HOME` to a temporary directory
  (observed). So sous-chef can only use `KAS_*` from the environment it was started in, and
  only when that names a directory containing, or inside, its working directory.
- kas refuses to start when `NETRC_FILE` names a file that does not exist (observed).

**Running a command** (`plugins/shell.py`). `kas shell <files> -c <cmd>` runs
`[$SHELL, "-c", cmd]` with kas's environment in the build directory, where `$SHELL` is the
caller's (observed: `/usr/bin/zsh -c 'bitbake -p'`), so the command string is shell text
for whatever shell the user has. `-k` / `--keep-config-unchanged` skips the steps
`setup_dir`, `finish_setup_repos`, `repos_checkout`, `repos_apply_patches` and
`write_bbconfig`, so nothing is checked out and the configuration files are not rewritten;
the environment is still set up. sous-chef's kas mode always passes it. What `-k` does
**not** skip (kas 5.5 `libkas.py` `setup_parser_keep_config_unchanged_arg`, `libcmds.py`
`Macro`, read 2026-11-18): `setup_home`, `init_setup_repos` and `repo_setup_loop`, then
`setup_environ`. `init_setup_repos` asks the include handler for the repositories that
`{repo, file}` includes name and that it was not given a path for, which on this first
call (`find_missing_repos()` with no paths) is every one of them; `repo_setup_loop` then
fetches them (`repos_fetch`: clone when the directory is missing, `git remote set-url
origin <url>`, a fetch when the wanted commit, tag or branch is not there) and checks
them out (`repo.checkout()`, unless the checkout is dirty), until the includes resolve.
So even `kas shell -k` changes repositories that `{repo, file}` includes name; sous-chef
does not use kas mode for such a project (`KasProject::repo_includes`, `BbEnv::probe`).
`setup_environ` looks for `oe-init-build-env` in every repository's path and stops with
"Did not find any init-build-env script" when none is there; otherwise it sources it with
the build directory, which creates the directory and, when missing, OE-core's template
`conf/local.conf` and `conf/bblayers.conf` (not kas's). So before the first checkout
(OE-core not cloned) `kas shell -k` fails; with the repositories present but no build
directory it runs with OE-core's templates. sous-chef offers kas mode only once the kas
build directory exists.

**Subcommands** (`kas --help`, `kas-container` usage). `build` checks out and runs BitBake
(`-c` there is the task, `--target` the target); `checkout` only checks out and writes the
configuration; `dump` checks out and prints the flat configuration; `lock` writes lock
files; `shell` runs a shell or `-c` command; `for-all-repos`, `clean`, `cleansstate`,
`cleanall`, `purge`, `diff`, `menu`. Global options before the subcommand: kas `-l` /
`--log-level <level>`, `--no-color`, `--version`; `kas-container` adds its own (those with a
value: `--runtime-args`, `--docker-args`, `--ssh-dir`, `--aws-dir`, `--git-credential-store`,
`--git-credential-socket`, `-l` / `--log-level`) and takes the same subcommands.

**YAML 1.1** (kas loads with `yaml.safe_load`, PyYAML 6.0.3 as installed with kas 5.5;
`resolver.py`, `constructor.py`, read 2026-11-18). sous-chef parses with a YAML 1.2
parser (`yaml-rust2`) and reproduces what matters for kas files:

- A plain scalar is a boolean for `yes|Yes|YES|no|No|NO|true|True|TRUE|false|False|FALSE|
  on|On|ON|off|Off|OFF` and an integer for `0b...`, `0...` (octal), decimal, `0x...` and
  base-60 `1:20` forms (`_` allowed). sous-chef keeps scalars as text and records whether
  one was plain; the one place the type changes kas's result is a layer's value, where a
  plain false or 0 disables the layer (`False == 0` in Python, `0` is a disabled token)
  and a quoted `'No'` is a string kas's schema refuses (`yaml11_false_or_zero`). `y` and
  `n` are strings in PyYAML (its resolver leaves them out).
- Merge keys `<<: *anchor` and `<<: [*a, *b]` (`flatten_mapping`): the merged entries
  come before the mapping's own, its own keys win, and of a list the first mapping wins.
  sous-chef flattens them the same way (`flatten_merge_keys`).
- Nulls (`~`, `null`, `Null`, `NULL`, empty) are the same in both.

**`kas-container`** (the 5.5 script, read 2026-11-18). It runs kas in a container with
the repository of the first kas file (`git rev-parse --show-toplevel`, else `hg root`,
else its directory; `_source_dir_host` from `.config.yaml` when set) mounted at `/repo`
(read-only for rootless docker), `KAS_WORK_DIR` at `/work` (`KAS_WORK_DIR=/work`), and
`KAS_BUILD_DIR`, `DL_DIR`, `SSTATE_DIR`, `KAS_REPO_REF_DIR`, `BB_HASHSERVE_DB_DIR` and
`KAS_BUILDTOOLS_DIR` as `/work/<relative>` when under the work directory, else mounted at
`/build`, `/downloads`, `/sstate`, `/repo-ref`, `/bb-hashserve-db`, `/buildtools`. The kas
file arguments under the repository are rewritten to `/repo/...` (`sed`); for `diff` with
two files the second repository goes to `/repo2`. A shared git worktree gets its common
directory at `/repo-common` and a rewritten `.git` file. `KAS_DISTRO`, `KAS_MACHINE`,
`KAS_TARGET`, `KAS_TASK` (and a few others) are passed in when set and not empty. kas then
writes `bblayers.conf` with `${TOPDIR}/<path relative to the build directory>` computed
inside the container, so a layer of the project repository (`/repo/...`) appears as
`${TOPDIR}/../../repo/...` from `/work/build`; on the host that names a directory that does
not exist unless the work directory's parent happens to hold the repository as `repo/`.
sous-chef reads `bblayers.conf` as it is, so discovery stops on such an entry (`BBLAYERS
in ... lists ..., which is not a directory`), with a hint that kas-container may have
written the file when the kas project generates that build directory. Not reproduced:
mapping container paths back to the host.

Not reproduced: `env`, `patches`, `signers`, `buildtools`, `artifacts`,
`menu_configuration`; JSON comments.

## Releases

Decided 2026-11-17. Wrynose (6.0 LTS) is the primary target, Scarthgap (5.0 LTS) is
fully supported, Kirkstone (4.0 LTS) is the floor; `src/release.rs` holds the table below as
`Feature::first()` / `Feature::last()`. A row marked *verify* has `Feature::verified()` false
and must not be used to report an error. No row is marked *verify* at present.

| `Feature` | Behaviour | First | Last | Source checked |
|---|---|---|---|---|
| `SplitClassDirs` | `inherit` searches `classes-<type>/` along `BBPATH` before `classes/` | 4.1 langdale (BitBake 2.2) | (current) | Yocto migration guide 4.1, "Classes now split by usage context"; BitBake `lib/bb/parse/parse_py/BBHandler.py` `inherit` on branches `2.0` and `2.2` |
| `InheritDefer` | the `inherit_defer` directive | 5.0 scarthgap (BitBake 2.8) | (current) | BitBake release branches, recorded under "Directives" |
| `IncludeAll` | the `include_all` directive | 5.2 walnascar (BitBake 2.12) | (current) | BitBake release branches, recorded under "Directives" |
| `UnpackDir` | `do_unpack` unpacks into `UNPACKDIR` (`${WORKDIR}/sources-unpack`), not `WORKDIR` | 5.1 styhead | (current) | Yocto migration guide 5.1 |
| `WorkdirAsSourceDir` | `S = "${WORKDIR}"` is accepted | 4.0 kirkstone (floor) | 5.0 scarthgap | Yocto migration guide 5.1 ("If a recipe has S set to be WORKDIR, this is no longer supported, and an error will be issued.") |
| `AddFragments` | the `addfragments` directive (three-argument form) | 5.2 walnascar (BitBake 2.12) | (current) | BitBake `ConfHandler.py` `__addfragments_regexp__` and `ast.py` `AddFragmentsNode`: absent on branch `2.10` and tag `yocto-5.1.4`, present on branch `2.12` (`ac0973009`) and tag `yocto-5.2.4`; added by `f68774670` (see "Configuration fragments") |
| `BuiltinFragments` | `addfragments` takes a fourth argument, the built-in fragments variable (`machine/x` sets `MACHINE`); the three-argument form is an unparsed line | 5.3 whinlatter (BitBake 2.16) | (current) | `3b9d7bea9` "parse/ast: add support for 'built-in' fragments", first in tag `yocto-5.3`; the four-group regex on branches `2.16` (`713fbbdb9`) and `2.18`, the three-group one on `2.12` and `yocto-5.2.4` |
| `RecipeVirtualProviders` | `DEPENDS` words in `BB_RECIPE_VIRTUAL_PROVIDERS` are replaced with `PREFERRED_PROVIDER_<word>` at finalisation | 5.2 walnascar (BitBake 2.12) | (current) | `ast.py` `handleVirtRecipeProviders`: absent on branch `2.10`, present on `2.12`, `2.16`, `2.18`; added by `fb119c788` "parse/ast: Add support for BB_RECIPE_VIRTUAL_PROVIDERS" (2025-01-21, in tag `2.12.0`) (see "Replay") |
| `AddPyLib` | the `addpylib` directive (base configuration only) | 4.2 mickledore (BitBake 2.4) | (current) | BitBake `ConfHandler.py` `feeder`: absent on branches `2.0` and `2.2`, present on `2.4` and `2.18`; added by `afb8478d3` (`2.3.1~96`) (see "Directives") |
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

## Scaffold conventions

What the generators (`src/scaffold/`) rely on, beyond the rules above.

### Scaffold: sources, `S` and build-system classes

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

### Scaffold: layers and classes

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

### Scaffold: `.inc` split

What `src/scaffold/inc_split.rs` relies on, all from rules already recorded above:

- `require <pn>.inc` with a relative name is tried in the including file's directory
  first ("Finding files"), so an `.inc` beside the `.bb` files is the one found.
- While the `.inc` is parsed, `FILE` is the `.inc` and is restored afterwards ("Replay",
  `FILE`): an immediate expansion (`:=`, `${...}` in `inherit` / `include`, also inside a
  class inherited from the `.inc`, since classes do not reset `FILE`) of `PN` gives
  `defaultpkgname`, of `PV` `1.0`. `THISDIR` / `FILE_DIRNAME` are the same directory. Lazy
  values are expanded after the whole recipe and its bbappends, with `FILE` the recipe
  again, so moving a lazy statement into the `.inc` does not change it.
- Operators read the current value of exactly the name written ("Datastore"); `:append`
  and friends are applied at the end ("Overrides", parse time); `unset` clears flags.
  The generator treats any two statements on the same variable name (any override form)
  as order-dependent, and a flag (`VAR[f]`) as separate from the value.
- Checked in BitBake 2.18.0 (live environment): a plain assignment `VAR = "x"` during
  parsing leaves the flags of `VAR` alone. `ast.py` `DataNode.eval` calls
  `setVar(key, val, parsing=True)` for an assignment without a flag; `data_smart.py`
  `setVar` (lines 544-626) then only registers override variants for a name with `:` and
  writes `self.dict[var]["_content"]`, while flags live in `self.dict[var][<flag>]`, written
  by `setVarFlag` (line 762). So `SRC_URI = ...` and `SRC_URI[sha256sum] = ...` may change
  order. The before/after evaluation test (`tests/scaffold_inc_split.rs`) checks this case
  against the sous-chef evaluator, which models flags the same way.
- `addtask` / `deltask` change only the task list (`__BBTASKS`, kept by the replay since
  w8); their relative order is kept by the generator (all of them count as touching the
  task list).
- (w8) The plan evaluates every version before and after the split, the planned files held
  in memory (`eval::evaluate_with_overlay`; the configuration cache and the index are not
  changed), and is refused when any final value, flag, function piece text, anonymous python
  text or `addtask` / `deltask` / `addhandler` statement order differs; the notes name the
  variables. The static order and `FILE` analysis above runs first: the cases it finds that
  involve python (an immediate expansion that reaches `${@...}`, which the evaluator leaves
  unexpanded both times) or a file it cannot read refuse the split on their own; the others
  are only explanations, and a split they would have refused is made when the evaluation
  shows no change (`:append` reordered past a version's own `=`: `:append` is applied at
  the end of parsing, "Overrides").
- (w9) The split never changes which recipes and bbappends BitBake collects: it modifies
  existing `.bb` files under their own names and creates one `.inc`, and `collect_bbfiles`
  keeps only names ending in `.bb` / `.bbappend` (BitBake 2.18.0 `lib/bb/cooker.py` lines
  1936-1946), bbappends being matched to recipes by file name ("Finding files"). So the
  evaluation with an overlay, which replaces file contents but takes bbappends and the
  collection from the index, sees the bbappends BitBake would apply.

## OE-core checks and defaults used by lint

Checked against OE-core master (2026-11) unless marked:

- **`pkgvarcheck`** (`meta/classes-global/insane.bbclass`, anonymous python): only when
  `PACKAGES` is not empty, for each listed variable (`RDEPENDS`, `RRECOMMENDS`,
  `RSUGGESTS`, `RCONFLICTS`, `RPROVIDES`, `RREPLACES`, `FILES`, `pkg_preinst`,
  `pkg_postinst`, `pkg_prerm`, `pkg_postrm`, `ALLOW_EMPTY`), `if d.getVar(var, False):`
  reports it as set without a package. `override/missing-package` follows it. Confirmed
  on OE-core Wrynose `33a6bd6c` (`insane.bbclass:1586-1591`): `getVar(var, False)`
  is the unexpanded final value, so an empty one (`bitbake.conf:336 FILES = ""`) is never
  reported, and `+=` of an empty value (which stores a space) is. `image.bbclass:31` sets
  `PACKAGES = ""`, so its own `RDEPENDS +=` / `RRECOMMENDS +=` (lines 34-35) are never
  checked.
- **`populate_lic_qa_checksum`** (`insane.bbclass`): `if lic == "CLOSED": return`, then
  `if not lic_files and d.getVar('SRC_URI')` is an error. So `LIC_FILES_CHKSUM` is required
  only when `LICENSE` is not `CLOSED` and `SRC_URI` is not empty (`hygiene/missing-metadata`).
- **`meta/conf/bitbake.conf` defaults** that lint must not count as set:
  `SUMMARY ?= "${PN} version ${PV}-${PR}"`, `LICENSE ??= "INVALID"`, `HOMEPAGE = ""`,
  `SRCREV ??= "INVALID"`. The `UNPACKDIR` defaults per release are in the table under
  "Scaffold: sources, `S` and build-system classes" (none on kirkstone and scarthgap).

Relied on by lint; settled against BitBake 2.18.0 and OE-core Wrynose (`33a6bd6c`)
in the live environment:

- OE-core has an `uppercase-pn` QA check reporting upper-case letters in `PN`
  (`org/recipe-path`): confirmed, `insane.bbclass:1557` ("PN: %s is upper case, this can
  result in unexpected behavior."), listed in `CHECKLAYER_REQUIRED_TESTS`.
- `IMAGE_INSTALL` is read only by image recipes (`org/policy-in-recipe`): confirmed by
  grep: as that exact name it is read only by `classes-recipe/image.bbclass` and
  `core-image.bbclass` (and recorded by `classes/buildhistory.bbclass`). A machine conf may
  still add to it (`meta-kiss-ti`'s `dogbonedark.conf` has `IMAGE_INSTALL:append`).
- The fetcher's checksum lookup (`fetch2/__init__.py` `FetchData.configure_checksum`):
  confirmed with refinements. For an entry with `name=<n>` the expected value is the URL
  parameter `<n>.sha256sum=`, else the URL parameter `sha256sum=`, else the flag
  `SRC_URI[<n>.sha256sum]`; without `name`, the parameter `sha256sum=`, else
  `SRC_URI[sha256sum]`. So with `name=<n>` the flag `SRC_URI[sha256sum]` is **not** used.
  Checksums are looked up only for `http`, `https`, `ftp`, `ftps`, `sftp`, `s3`, `az`,
  `crate`, `gs`, `gomod` and `npm` URLs (other types get none).
- The `SRCREV` lookup (`srcrev_internal_helper`): confirmed and extended. For `name=<n>`
  the variables tried are `SRCREV_<n>:pn-<pn>`, `SRCREV_<n>`, `SRCREV:pn-<pn>`, `SRCREV`
  (without a name only the last two), first value set and not `INVALID`; a `rev=` URL
  parameter is used when none is set, and must equal the variable's value otherwise
  ("Conflicting revisions").

## What the static evaluator cannot do

- `${@python expression}` in values
- anonymous python functions and event handlers
- `d.setVar` / `d.appendVar` inside classes (e.g. `PACKAGECONFIG` processing in `base.bbclass`)
- `inherit` / `require` whose argument depends on any of the above
- **class-extended variants** (`BBCLASSEXTEND`): BitBake 2.18.0 `lib/bb/parse/ast.py`
  `multi_finalize` makes a datastore per word (`ext.split(':', 2)`; a plain word sets
  `PN = "<pn>-<word>"` and `inherit_defer`s the class); OE-core (Wrynose)
  `nativesdk.bbclass` `nativesdk_virtclass_handler` renames `<pn>-nativesdk` to
  `nativesdk-<pn>`, `classes/multilib.bbclass` sets `PN = "<variant>-<pn>"` for
  `multilib:<variant>`. sous-chef evaluates the base recipe only. Reported (w6,
  `recipe::class_extend_hint`): a name of the form `<pn>-native`, `nativesdk-<pn>` or
  `<variant>-<pn>` whose base recipe's files list `native`, `nativesdk` or
  `multilib:<variant>` in `BBCLASSEXTEND` (static words; `${...}` words and global
  `BBCLASSEXTEND:append` from multilib configuration are not seen) is an error saying it is
  that variant and naming the base recipe; other classes' naming was not checked and is not
  claimed.
- **`BBMASK`** (BitBake 2.18.0 `lib/bb/cooker.py` `CookerCollectFiles.collect_bbfiles`:
  `BBMASK.split()`, a leading `|` dropped from a word, each word compiled alone and ignored
  when invalid, the rest joined with `|` and `re.search`ed in the path of each file the
  `BBFILES` globs gave): applied (w6) when the configuration's value (default machine and
  distro) is static; masked files are `files.not_collected = 'bbmask'` (no recipe, no
  provider, no bbappend edge, not linted). Recomputed after every full edge rebuild and
  after a `.conf` is reindexed (not after an `.inc` a conf includes). The paths searched
  are the workspace's canonical ones, not the `BBLAYERS` spelling (the `LAYERDIR` gap). A
  value with python or undefined variables, or a word Rust's `regex` cannot compile, is not
  applied and every evaluation says so (`note: BBMASK is set but sous-chef does not apply
  it`). (w8) An evaluation for an explicit `machine` or `distro` compares its own static
  `BBMASK` with the index's marks: the files the index collects plus those it marked
  `bbmask` are searched with the evaluation's mask, and when the masked set differs the
  evaluation gets `note: BBMASK for this machine and distro differs from the one the index
  applied (...): it would also mask N files: ...; it would not mask N files the index
  treats as masked: ...` (`replay::bbmask_note`); recipe names, providers and bbappends
  still follow the index's mask. A `BBMASK` written with an override variant
  (`BBMASK:boardx`) is often not static in the configuration pass, because `OVERRIDES`
  holds `pn-${PN}` and `layer-${FILE_LAYERNAME}` there; it then gets the "does not apply"
  note instead.
- **Skipped recipes** (OE-core `classes-global/base.bbclass`, Wrynose, anonymous python):
  `SKIP_RECIPE[<PN>]` non-empty; `COMPATIBLE_MACHINE` set (and `PARSE_ALL_RECIPES` not
  true) that `re.match`es (anchored at the start only) no entry of
  `MACHINEOVERRIDES.split(":")`; `COMPATIBLE_HOST` set (and neither `SOURCE_MIRROR_FETCH`
  nor `PARSE_ALL_RECIPES` true) that does not `re.match` `HOST_SYS`. Nothing is hidden: when
  every value involved is static (no python, no undefined variable, not set by anonymous
  python by name) and the check fails, the evaluation gets `note: BitBake would skip this
  recipe: ...` (`replay::skip_reasons`), shown by `recipe`, `var`, `deps`, `overrides`.
  (w8) When the check fails on what is known but unknown values could still decide it, the
  note is `note: BitBake may skip this recipe: ...` naming them: `COMPATIBLE_MACHINE` that
  matches none of the known entries of `MACHINEOVERRIDES` while some entries hold python or
  undefined variables ("N parts of MACHINEOVERRIDES are not evaluated") or anonymous python
  sets it by name; a `COMPATIBLE_MACHINE` / `COMPATIBLE_HOST` / `HOST_SYS` that is not
  static, unset `HOST_SYS`, a regular expression Rust cannot compile. A match on a known
  `MACHINEOVERRIDES` entry is a match (nothing said). On yocto-kiss `MACHINEOVERRIDES` is
  `${@...}...${@...}dogbonedark` (the `soc-family.inc` and `arch-arm*.inc` `=.` python
  glued to the machine name, no `:` outside the python), so no entry is known statically.
  `SKIP_RECIPE` is said only when its flag is static.
- **`INCOMPATIBLE_LICENSE`** (OE-core Wrynose `33a6bd6c`, `classes-global/base.bbclass`
  anonymous python lines 591-621, only when neither `SOURCE_MIRROR_FETCH` nor
  `PARSE_ALL_RECIPES` is true; `lib/oe/license.py` `skip_incompatible_package_licenses`,
  `incompatible_license`, `incompatible_pkg_license`, `flattened_licenses` /
  `FlattenVisitor` / `LicenseVisitor.get_elements`, `expand_wildcard_licenses`,
  `apply_pkg_license_exception`, `canonical_license`, `license_ok`): `bad =
  (INCOMPATIBLE_LICENSE or "").split()`; `pkgs = PACKAGES.split()`; when `pkgs` is empty
  nothing is skipped. `skip_incompatible_package_licenses` returns nothing (so nothing is
  skipped) when `bad` is empty or `PN` starts with `nativesdk-` or `gcc-source-` or ends
  with `-native`, `-cross-${TARGET_ARCH}`, `-cross-initial-${TARGET_ARCH}`,
  `-crosssdk-${SDK_SYS}`, `-crosssdk-initial-${SDK_SYS}`,
  `-cross-canadian-${TRANSLATED_TARGET_ARCH}`; otherwise `bad` goes through
  `expand_wildcard_licenses` (`AGPL-3.0*`, `GPL-3.0*`, `LGPL-3.0*` become their `-only` and
  `-or-later` names; any name in `obsolete_license_list()` or any other `*` is `bb.fatal`,
  for every recipe), an obsolete license in `INCOMPATIBLE_LICENSE_EXCEPTIONS` (the part
  after `:`) is `bb.fatal`, and for each package the patterns left after removing
  exceptions written `<pkg>:<lic>` are checked against `LICENSE:<pkg>`, or `LICENSE` when
  that is empty: the license string is split on `&|() `, names are `[a-zA-Z0-9.+_\-]+`
  (two in a row get `&`; anything else is `InvalidLicense`, `bb.fatal`), parsed as a Python
  expression (`&` binds tighter than `|`), and flattened keeping, at each `|`, the left
  side if every license in it, mapped through `SPDXLICENSEMAP[lic]`, matches no pattern
  (`fnmatchcase`), else the right side; the package is skipped when a kept license matches
  a pattern. The recipe is skipped only when **every** package is, with `SkipRecipe("it
  has incompatible license(s): <sorted unique list>")`, the list being the recipe
  `LICENSE`'s own incompatible licenses (against `bad` **unexpanded**, no exceptions) plus
  every package's. sous-chef (w8, `replay::license_skip`, `eval/license.rs`) does this on
  static values: "would skip" with BitBake's list when everything is known, nothing when
  some package keeps a compatible license, "may skip" naming what is not known (a
  `LICENSE` with python, undefined variables, or a form it does not parse such as `A (B)`,
  `PACKAGES`, a non-static `SPDXLICENSEMAP` flag, `INCOMPATIBLE_LICENSE` itself), and
  `note: BitBake would stop parsing this recipe: ...` for the fatal cases. On yocto-kiss
  `INCOMPATIBLE_LICENSE` is not set.

These are reported, never silently guessed. Authoritative mode (M5) covers them.

## Not yet verified

Statements above that rest on documentation, memory or the source alone, not on a check
against BitBake's behaviour or output:

- kas: `layers` null and `{}`, the `KAS_*` overrides, what `kas shell -k` does (before the
  first checkout and with `{repo, file}` includes), the YAML 1.1 rules and `kas-container`
  were settled from the kas 5.5 and PyYAML 6.0.3 source (see "kas"), not by running kas
  (the live environment ran only `kas checkout`, `kas dump` and `kas shell -c`); the kas
  keys sous-chef does not read (`env`, `patches`, ...) were not looked at.
- The skip and license notes ("Skipped recipes", `INCOMPATIBLE_LICENSE`, the `BBMASK`
  difference note): from the BitBake and OE-core source and fixture tests; none fires on
  yocto-kiss (its `MACHINEOVERRIDES` holds python, `INCOMPATIBLE_LICENSE` is unset), so no
  note was compared with a real BitBake skip.
- The class type of an `inherit` in an `.inc` that a `.conf` or a global class reaches
  ("Which handler parses an included file"): from the source and tests; no such `inherit`
  exists on yocto-kiss (every `inherit` in an `.inc` there is reached from recipes only).
- `bitbake -e`: the `# expansion of NAME threw ...` line is from `emit_var`, not seen in
  real output.
- Task logs: the compiler, configure, make, wget and quilt messages are those tools' own
  formats, not checked against a source; build.py's `Backtrace (metadata-relative
  locations):` was not seen in a real log.
- Running BitBake: the second client's failure after about 20 s of waiting (from
  `lib/bb/main.py`, not observed); the server's behaviour on cancel with
  `BB_SERVER_TIMEOUT` set (not tried).
- How `bitbake-layers add-layer` edits `bblayers.conf` (not run).
- The release that first has `python_hatchling` (present on Scarthgap and Wrynose, absent on
  Kirkstone; Langdale to Nanbield not checked).
- File-name order: for a longer invalid UTF-8 sequence, that Rust's and Python's decoders
  find the same invalid bytes (both follow the Unicode "maximal subpart" rule; not
  separately checked).

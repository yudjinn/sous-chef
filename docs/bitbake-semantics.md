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
```

`OVERRIDES` is a colon-separated list. **Entries to the right have higher priority.** For a
variable with several active override variants, the one whose override is furthest right in
`OVERRIDES` wins. Multiple overrides can be chained (`VAR:a:b`); all must be active.

Finalisation order for a variable: choose the winning override variant (or the plain value),
then apply every active `:append`, then every active `:prepend`, then every active `:remove`.
*Verify* the exact relative order of append and prepend application against
`data_smart.py`; the result does not depend on it except for flags.

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

A shell or `python` function body ends at the first line that is exactly `}` in column 0.

### Directives

```
inherit a b c                        inherit classes (names may contain ${VAR})
inherit_defer a                      like inherit, evaluated at the end of parsing (recent releases; verify)
require path/file.inc                include; error if not found
include path/file.inc                include; silently ignored if not found
include_all path/file.inc            include every match on BBPATH (recent releases; verify)
addtask do_x after do_a before do_b  (also: addtask x, "do_" optional; verify)
deltask do_x
addhandler name
EXPORT_FUNCTIONS do_x do_y           in class foo: do_x defaults to calling foo_do_x
```

## Finding files

- `BBPATH` is the search path: by convention each `layer.conf` does `BBPATH .= ":${LAYERDIR}"`,
  so it is the layer roots in `BBLAYERS` order (the build directory comes first).
- `require` / `include`: an absolute path is used as is. A relative path is tried relative to
  the including file's directory, then against each `BBPATH` entry. *Verify* the order.
- Classes: `inherit foo` looks for `classes-recipe/foo.bbclass` then `classes/foo.bbclass` on
  `BBPATH`. Global `INHERIT` looks for `classes-global/foo.bbclass` then `classes/foo.bbclass`.
  The split directories exist since Yocto 4.1; older layers only have `classes/`.
- `base.bbclass` is always inherited globally.
- Recipes and appends: the `BBFILES` globs set in each `layer.conf`, typically
  `${LAYERDIR}/recipes-*/*/*.bb` and `.bbappend`.

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
layer has the last word). *Verify* append ordering against `bitbake-layers show-appends`.

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

# Lint rules

Each rule is one file `src/lint/rules/<id with / and - replaced by _>.rs` (e.g.
`override/legacy-syntax` is `override_legacy_syntax.rs`), implements the `Rule`
trait (M4, T4.1), has a positive and a negative fixture, and can be disabled by id or group
in `sous-chef.toml`.

Scope tells the engine what to hand the rule: `file` (one parsed file), `recipe` (the
evaluated recipe tree), `layer` (index queries over a layer).

Every rule that splits a list value (`SRC_URI`, `DEPENDS`, `OVERRIDES`, `BBCLASSEXTEND`,
...) does it with `parse::words` (since w6), so a `${@...}` expression or another `${...}`
group is one word and never read as a name (bitbake-semantics.md, "Splitting list
values").

Severity: **E** error (will break or is no longer accepted), **W** warning (likely bug or
convention breach), **I** info (style). Two letters mean the severity depends on the case;
the row says which.

Releases: there is one rule set for all supported Yocto releases (Kirkstone to Wrynose). A
rule whose subject exists only in some releases declares that range (`Rule::releases`,
default: all), and the engine runs it for a file or layer only when the range includes one
of the releases it is checked against (`LintContext::releases`, oldest first). A layer is
checked against every supported release in its `LAYERSERIES_COMPAT` plus the workspace's
release when the layer is writable, when it is the lint target or contains it (a named
layer, or a file or recipe in it), or when the workspace has no writable layer. A layer
that is not writable and is linted only because a whole-workspace run has `--all` is
checked against the workspace's release alone, and so is a file outside every layer (the
build directory's `conf/*.conf`). A finding that holds for only some of the releases names
them. See "Release contract" in architecture.md. Changing `release` in `sous-chef.toml`
rebuilds the index for the new release. A user changes behaviour with `release`,
`[lint] disable` and `[lint.severity]` in `sous-chef.toml`; there are no user-defined rules.

## Running lint

`sous-chef lint [target] [--rule ID]... [--severity S] [--fail-on S] [--format F] [--all]
[--as-release R [--only-release]] [--migrate-to R] [--baseline FILE | --write-baseline FILE]`
and the `lint` tool take the same target: a layer name (`build` for the build directory),
a pointer `layer:path` or `layer:path:A-B` (the range keeps only findings in it), a path to
a file or directory relative to the working directory, or a recipe `pn` or `pn_pv` (its
`.bb` as BitBake picks it, through provider selection, so `PREFERRED_VERSION` and
`DEFAULT_PREFERENCE` count; the files it requires or includes; its bbappends; not classes).
With no target, the whole workspace: the writable layers and the build directory's
configuration files. Layers that are not writable are skipped, with a note, unless
`--all`; if no layer is writable, every layer is linted. A named layer, recipe or file is
linted whatever its layer. For a recipe or file target, layer-scope rules run on the layers
the files are in and only findings in those files are kept. The releases each layer is
checked against are given under "Releases" above.

Recipe-scope rules run once per `.bb` of the target, on the evaluation of that file. So
`override/dead` and `override/shadowed` run only when the `.bb` itself is linted, and their
findings can be located in a bbappend of another layer.

Which files: the indexed BitBake files of the target. Data files (a `.conf` / `.inc` a recipe
installs, bitbake-semantics.md "Data files") are left out. A `.bb` or `.bbappend` BitBake does
not collect (no `BBFILES` glob matches it, only an inactive `BBFILES_DYNAMIC` entry does, or
`BBMASK` masks it; `files.not_collected`) is told about once and otherwise left alone: it
is never evaluated, no file- or recipe-scope rule runs on it, and a layer-scope rule reports
it: `org/recipe-path` for a `.bb` that no `BBFILES` glob matches, `org/layer-conf` for such a
`.bbappend` (one finding at the layer's `BBFILES` line). It still counts in the number of
files checked.

Which rules run:

- With no selection, every rule except those in `[lint] disable` and those off by default
  (`Rule::default_enabled` false; only `hygiene/variable-order`).
- `--rule` / `rules` names ids or groups; an unknown one is an error. A rule named by its
  exact id runs even when disabled or off by default; a group selects only rules that are
  enabled.
- `style/variable-order` is an alias of `hygiene/variable-order` wherever an id is
  accepted. `style` is an alias group: it names exactly the rules its aliases name, and
  selecting it counts as naming them by id, so `--rule style` runs `hygiene/variable-order`
  even though it is off by default or disabled.
- Unknown ids or groups in `[lint] disable` or `[lint.severity]` do not stop lint; each
  becomes a note in the report.
- `[lint.severity]` replaces a rule's severity; an entry for the id wins over one for its
  group.

Inline suppression: a comment `# sous-chef: allow <id-or-group>[, <id-or-group>...]` on the
line directly above the finding's line, or directly above the statement that contains it.
Several such comment lines may be stacked.

Exit status of `sous-chef lint`: 0 clean, 1 a reported finding at or above `--fail-on`
(default `error`), 2 lint could not run (bad target, unknown rule id, unreadable
workspace, missing or malformed baseline). Findings hidden by `--severity` never fail the
run.

Other releases:

- `--as-release <codename>` (tool: `as_release`) checks every unit against that release
  as well, as if its layer claimed it in `LAYERSERIES_COMPAT`; with `--only-release`
  (`only_release`) against that release alone. Only `LintContext::releases` changes (carried
  in `lint::Options`); findings name their releases as usual. An unknown codename is an
  error listing the supported ones, and a release older than Kirkstone is refused.
- `--migrate-to <codename>` (tool: `migrate_to`; audit: section 7) prints the migration
  report of the target (default: the writable layers and the build configuration): the
  findings new with that release (lint against it alone, minus the rule and pointer pairs
  lint finds now), those that already hold, the features the target release changed that
  the layers use (each `Feature` whose value differs between the newest release a layer is
  checked against now and the target, with the guide's instruction and every pointer), each
  layer's `LAYERSERIES_COMPAT` and what to write, and what the report cannot tell. Text or
  JSON (`sous-chef-migration/1`). Exit status 1 when a new finding is at or above
  `--fail-on`.

CI (docs/ci.md): `--format sarif` (SARIF 2.1.0 for GitHub code scanning), `--format
github` (workflow command annotations), `--write-baseline FILE` and `--baseline FILE`
(report and fail only on findings not in the baseline). With `sarif` and `github` the
summary line and notes go to stderr.

## Group `override` (task T4.2)

| Id | Sev | Scope | Flags |
|---|---|---|---|
| `override/legacy-syntax` | E | file | Underscore override syntax: a name the parser marks legacy (`VAR_append`, `VAR_remove`, `RDEPENDS_${PN}`, a package variable followed by `_<lowercase package>`), or `VAR_<o>` whose trailing `_` elements are all machine or distro names of the workspace, known `class-*` overrides or `pn-<name>`. (Since x7) also the package variables of OE-core's `scripts/contrib/convert-overrides.py` that the parser's list lacks (`SYSTEMD_SERVICE_${PN}`, `INITSCRIPT_PARAMS_${PN}`, `INITSCRIPT_NAME`, `ALTERNATIVE`, `USERADD_PARAM`, `GROUPADD_PARAM`, `PKGV`, `IMAGE_CMD_<type>`, ...), but not `ALTERNATIVE_TARGET_<pkg>` / `ALTERNATIVE_PRIORITY_<pkg>`, which `update-alternatives.bbclass` still reads with `_`. Never flags `do_install`, the per-collection `layer.conf` names, a `PREFERRED_*` name itself (`PREFERRED_PROVIDER_virtual/kernel`, `PREFERRED_VERSION_<pn>`, `PREFERRED_PROVIDER_<machine>`), `SRCREV_<name>` or a suffix that is a collection name; a machine or distro suffix after the name a `PREFERRED_*` variable is keyed by is reported (`PREFERRED_PROVIDER_virtual/kernel_qemux86`, which the old syntax read as an override: write `PREFERRED_PROVIDER_virtual/kernel:qemux86`). The message names what the fix writes (`VAR_qemux86_append`: `VAR:qemux86:append`). Misses `VAR_<machine>` when that machine has no `.conf` in the workspace. |
| `override/append-no-space` | W | file | `:append` value without a leading space, or `:prepend` without a trailing space, on a variable known to be a space-separated list. Not flagged: colon lists (`FILESEXTRAPATHS`), scalars, flags and unknown variables; values starting with `${@`, and a `:prepend` value ending in python; a value starting (`:append`) or ending (`:prepend`) with `${NAME}`, unless the file itself gives `NAME` a non-empty literal value without the space; `+=` / `=+` on an `:append` / `:prepend` name (they add the space); function bodies. |
| `override/plus-equals-with-override` | W | file | `VAR:foo += "x"` (and `=+`, `.=`, `=.`): this builds the variable `VAR:foo`, which replaces `VAR` when `foo` is active, rather than appending to `VAR`. Suggests `VAR:append:foo`. Not flagged: the package override of a package-scoped variable (`FILES:${PN} +=`, `RDEPENDS:libfoo +=`), overrides containing `${` or not starting with `[a-z0-9]`, names with `:append` / `:prepend` / `:remove`. A condition after the package is flagged (`RDEPENDS:${PN}:qemux86 +=`). |
| `override/remove-in-recipe` | W/I | file | `:remove` (not on a flag). Warning in a `.bb`, `.inc` or `.bbclass`: it is applied last, so no bbappend or conf can add the item back. Info in a `.bbappend`, a `.conf`, or an `.inc` under a `conf/` directory (configuration), where it is the usual tool. (Since lint-precision) The advice depends on where the value is set: when the file itself assigns the name before `:remove`, leave the item out there; otherwise (`CFLAGS:remove`, `DEBUG_OPTIMIZATION:remove`: configuration sets them) remove it through a variable a bbappend can change. |
| `override/dead` | W/I | recipe | An override element of an inactive operation in the recipe's own files (the `.bb`, its `.inc` files, its bbappends) that is no entry of the recipe's `OVERRIDES` (an entry that holds python is never a name), no machine, distro, class or declared override of the workspace, and no word of any `*OVERRIDES` assignment. Suggests the closest known name; (since lint-precision) an element within edit distance 2 of `append`, `prepend` or `remove` is named as that operation misspelt (`do_install_ptest:aapend` in OE-core Wrynose's `python3-wcwidth`). Info instead of warning when the override list is only partly known, and then the message says the name may be set by a machine, distro or architecture outside the workspace (on OE-core these are mostly real architecture and OS overrides: `arc`, `sh4`, `darwin`, `mingw32`). Never checks package names, `${...}` elements or legacy names. |
| `override/shadowed` | I | recipe | A `NAME =` / `NAME :=` in the recipe's own files that the evaluation marks shadowed, when a later `=` / `:=` of exactly the same name follows. BitBake's rewrite of virtual providers in `DEPENDS` (a `rewrite` row of the history) is no assignment and replaces nothing. Not flagged: functions; a later assignment in a bbappend other than the earlier one's; any `NAME` that something in the tree could read first (a `:=` mentioning it, an `inherit` / `include` / `require` with `${NAME}`); an earlier assignment in a file that more than one recipe (or a class, `.conf` or bbappend) reaches through `require` / `include`, unless the later one is in that same file. |
| `override/weak-after-default` | I | file | `??=` after a `=`, `:=` or `?=` of exactly the same name (overrides and flag included) earlier in the same file; an `unset` in between starts over. |
| `override/missing-package` | E | file | `RDEPENDS`, `RRECOMMENDS`, `RSUGGESTS`, `RCONFLICTS`, `RPROVIDES`, `RREPLACES`, `FILES`, `ALLOW_EMPTY` and the `pkg_*` functions set to a value without a package override (`:${PN}`), in recipes that produce packages. Also `VAR:append`, `VAR:append:<cond>` and a function `pkg_postinst()`. Not flagged: an empty value (`FILES = ""`; but `+=` / `=+` store a space and are flagged), flags, `:remove` alone, legacy names (`override/legacy-syntax`), the classes `image` and `core-image`, a class that inherits one of them or itself empties `PACKAGES`. |
| `override/append-vs-plus-equals` | I | file | An unconditional `VAR:append =` in a `.bb` after a plain `VAR =` / `VAR :=` in the same file, where `+=` (or `.=`) would do. Not flagged when a later statement of the file assigns `VAR` again or is an `inherit`, `require` or `include`, when (since lint-precision) the file assigns an override variant `VAR:<o>` anywhere (it replaces `VAR` with its `+=` text, but still gets the `:append`: OE-core's `lttng-modules` and `SRC_URI:class-devupstream`), after an `unset VAR`, or outside `.bb` files (in bbappends and classes `:append` is usually right). |
| `override/plus-equals-on-default` | W | file | `VAR +=` (and `=+`, `.=`, `=.`) on a plain name in a conf file, when a `?=` of the same name is parsed later and nothing gave `VAR` a value before: the default is lost. Suggest `:append`. Not flagged (since lint-precision) when that default is empty (`VAR ?= ""`, as OE-core's `bitbake.conf` writes `DISTRO_EXTRA_RDEPENDS`): nothing is lost. Parse order comes from the evaluated configuration; nothing is reported when the order cannot be told. |
| `override/filesextrapaths` | E | file | `FILESEXTRAPATHS` replaced instead of extended (any operator but `=.` / `.=` without `:prepend` / `:append`; `=.` / `.=` on a conditional name or in a `.conf`), a prepend (`:prepend`, `=.`) without the trailing colon or an append (`:append`, `.=`) without the leading one, or no `:=` when the value uses `${THISDIR}`, `${FILE_DIRNAME}` or `${FILE}` outside the `.bb` itself and outside an `.inc` that only `.bb` files of its own directory include. Not flagged: `:remove`, a `?=` / `??=` in a `.conf` (the default's definition), and a value whose end in question is a `${NAME}` the file does not set to a literal value (python never). |

Notes:

- **`legacy-syntax`.** Three sources: the parser's `VarRef.legacy_syntax` (see "Legacy
  syntax" in bitbake-semantics.md); (since x7) `SCRIPT_PACKAGE_VARS` in the rule's file, the
  package variables of OE-core's `scripts/contrib/convert-overrides.py` the parser's list
  lacks, each checked to be read with `:<package>` by OE-core on wrynose; and the suffix
  check, which needs the index's machine and distro conf names. The suffix check never
  reads what follows one of these prefixes as an override suffix: `BBFILE_PATTERN`, `BBFILE_PRIORITY`, `LAYERDEPENDS`, `LAYERRECOMMENDS`,
  `LAYERSERIES_COMPAT`, `LAYERVERSION`, `PREFERRED_PROVIDER`, `PREFERRED_RPROVIDER`,
  `PREFERRED_VERSION` or `SRCREV` (so `PREFERRED_PROVIDER_qemux86` is a name, but a machine
  suffix after the provided name, `PREFERRED_PROVIDER_virtual/kernel_qemux86`, is reported).
  Both forms are errors: BitBake rejects a name containing
  `_append`, `_prepend` or `_remove`, and treats the others as a different variable.
- **`plus-equals-with-override`.** The package-scoped variables are OE-core's `PACKAGEVARS`
  plus `INSANE_SKIP` and `QA_EMPTY_DIRS`. The first override of such a variable is taken as
  the package unless it is a known machine, distro, class or other conditional override.
- **`dead`.** "Known" means: in the recipe's evaluated `OVERRIDES` (its known entries: an
  entry that holds `${@...}` or an undefined `${VAR}` stands for unknown text and is never
  taken as a name, bitbake-semantics.md "Splitting list values"); a machine or distro conf
  stem, a static word of `OVERRIDES` / `MACHINEOVERRIDES` / `DISTROOVERRIDES`, a `class-*`
  or `pn-*` form, the target OS or architecture, or a class name; or any word in the value
  of an `OVERRIDES`, `MACHINEOVERRIDES`, `MACHINEOVERRIDES_EXTENDER`, `DISTROOVERRIDES`,
  `CLASSOVERRIDE`, `LIBCOVERRIDE`, `TCOVERRIDE`, `TUNEOVERRIDES` or `FILESOVERRIDES`
  assignment of the workspace, python included (only the quoted strings of a `${@...}`
  expression, the values it may add, not its code: `bb`, `utils`, `d` are no override
  names). The override list counts as partly known
  when `OVERRIDES` needs python or has unresolved references, when anonymous python is
  opaque, or when it changes one of those variables; this is the usual case on real
  OE-core, so findings there are mostly info. The suggestion is a known machine, distro,
  declared or active override within edit distance 2 (names of four or more characters).
  Elements that are `append` / `prepend` / `remove`, contain `${`, or do not start with
  `[a-z0-9]` are never checked; nor is the first override of a package-scoped variable
  (packages can come from python at build time).
- **`shadowed`.** Conservative on purpose: any `:=` whose value mentions `NAME` (`${NAME}`
  or python), and any `inherit` or `include` / `require` whose argument mentions `${NAME}`,
  anywhere in the tree (classes included), keeps the assignment from being reported; their
  order relative to the two assignments is not checked. The rule runs on one recipe's
  evaluation, so a shared file (an `.inc` several `.bb` files require, such as OE-core's
  `u-boot-common.inc`, or a `.bb` another `.bb` requires, such as `buildtools-tarball.bb`)
  is not judged from it: an assignment there is reported only when the assignment that
  replaces it is in the same file, where it is replaced for every includer. Who reaches a
  file comes from the index's resolved include edges; an include that needs evaluation is
  not seen, and when the index cannot answer the file counts as shared.
- **`missing-package`.** The variables are those of OE-core's `pkgvarcheck` QA check, which
  rejects them set without a package. That check runs only when `PACKAGES` is not empty, so
  a file is not reported when every recipe it belongs to statically has an empty
  `PACKAGES`: an unconditional `PACKAGES = ""`, or `inherit image` / `core-image`, and
  nothing that could give `PACKAGES` a value. Packagegroups are checked
  (`packagegroup.bbclass` sets `PACKAGES = "${PN}"`). The check tests `if d.getVar(var,
  False)`, so an empty value is never a finding. A class is exempt when it is `image` or
  `core-image`, inherits one of them directly, or sets an unconditional `PACKAGES = ""` and
  gives `PACKAGES` no other value (OE-core's `image.bbclass`: `PACKAGES = ""` then
  `RDEPENDS +=`); other classes (a `*-image` name alone does not count), a file reached from
  a class or a `.conf`, or from no recipe, keep their findings.
- **`plus-equals-on-default`.** A `?=` in a recipe, bbappend, recipe class or
  non-configuration `.inc` is always parsed after every `.conf`; a `?=` later in the same
  file is after by line; between `.conf` files the order is the history of the evaluated
  configuration (no recipe). A `.conf` the configuration does not reach, compared with
  another such file, gives no finding.
- **`filesextrapaths`.** In the `.bb` itself `${THISDIR}` still expands to the recipe's
  directory at the end of parsing, so `=` is harmless there; the same holds in an `.inc`
  that only recipes of its own directory reach through `require` / `include` (OE-core's
  `clang/common.inc`, `gcc-15.3.inc`). `FILESEXTRAPATHS =. "${FILE_DIRNAME}/x:"` in a
  recipe is correct (see "Files and patches" in bitbake-semantics.md for which forms keep
  the list). A value ending (prepend) or starting (append) with `${NAME}` is reported only
  when the file assigns `NAME` a literal value without the colon (`${THISDIR}`,
  `${FILE_DIRNAME}`, `${FILE}` and `${LAYERDIR}` are paths and count as missing it). Legacy
  names (`FILESEXTRAPATHS_prepend`) are left to `override/legacy-syntax`.

## Group `org` (task T4.3)

| Id | Sev | Scope | Flags |
|---|---|---|---|
| `org/recipe-path` | W/I | layer | Recipe not at `recipes-<category>/<dir>/<file>.bb` (W); `<dir>` holds only this recipe and is neither its `PN` nor a prefix of it (I); file name with more than one `_` (W) or no `_<pv>` (I); upper-case letters in `PN` (W). Recipes under `dynamic-layers/` are skipped; the no-version check skips `packagegroup-*` and recipes whose static tree (their own lines and the files they require) inherits an image class (`image`, `core-image`, `*-image`) or `packagegroup`, and (since lint-precision) recipes whose static tree sets `PV` (meta-oe's `colord-native.bb` requires `colord.inc`, which sets it); the directory check skips such an image in `images/` and such a packagegroup in `packagegroups/`. A `.bb` no `BBFILES` glob matches gets one finding instead, naming its layer's globs: I in a directory that is clearly no recipe tree (a `lib/`, `scripts/`, `files/`, `wic/`, `templates/` or `*-templates/` component, or a recipe's own `<pn>/` directory), W anywhere else; a `.bb` left out by an inactive `BBFILES_DYNAMIC` entry or `BBMASK` gets none. |
| `org/duplicate-versions` | I | layer | Two or more versions of a recipe whose statements are mostly identical (at least 60 percent of the longer one, and at least 3): suggest a shared `<pn>.inc`. Version data and `require` / `include` lines are left out of the comparison. Reported on the newest version. (Since lint-precision) A `.bb` BitBake does not collect is left out, and two files of the same version are never compared: they are alternatives (meta-oe's `dynamic-layers/selinux/` copies of `android-tools-conf`), not versions. |
| `org/inc-has-version-data` | W/I | file | An `.inc` that `.bb` files of two or more versions of one recipe (since lint-precision: the same `PN`; OE-core's `common-clang.inc` sets the one `PV` of `clang`, `llvm`, `lld`, ..., one version each, and is not reported) require or include directly sets `PV`, `SRCREV` / `SRCREV_<name>` (not with `AUTOREV`) or a `SRC_URI` checksum flag (W), or `LIC_FILES_CHKSUM` (I: fine while the license text is the same). Appending to `PV` is not flagged. |
| `org/inc-unused` | I | layer | A `.inc` that no `require`, `include` or `include_all` of the workspace resolves to. An unresolved argument with `${...}` that could match the file name keeps it from being reported. |
| `org/include-must-exist` | W | file | `include` of a file the recipe cannot work without (it sets `SRC_URI` or `LICENSE`, or defines a `do_*` task or `addtask`): use `require`. Also `require` of a file that does not resolve. A `require` whose argument still contains `${` is never reported, even when it expands statically (a known miss). |
| `org/repeated-logic` | I | layer | The same function body (5 or more lines) or the same 5 consecutive assignments in three or more recipes of a layer: suggest a bbclass or `.inc`. Recipes are counted by `PN` (an `.inc` counts as itself); classes are not looked at. Reported on the first file in path order. |
| `org/class-recipe-specific` | W | file | A bbclass sets `SRC_URI`, `PV` or `LICENSE` with `=` or `:=`, or sets another variable with `=` / `:=` that a recipe inheriting it directly sets before its `inherit` line (the class replaces the recipe's value). `?=`, `??=`, `+=` and `:append` are not flagged, nor (since lint-precision) a recipe value written exactly as the class's, whitespace aside (OE-core's `python-async.inc` and `pypi.bbclass` both write `SECTION = "devel/python"`: nothing is lost). A value equal only after expansion (`python3-maturin`'s `S`) is still reported. |
| `org/class-export-functions` | W | file | A bbclass defines `<class>_do_<task>` without `EXPORT_FUNCTIONS do_<task>`, or defines `do_<task>` directly for a task it does not add itself with `addtask` (this replaces the recipe's own definition). Classes with `-` in the name are skipped. (Since x7) Only a task: `do_<task>` must be added by `addtask` somewhere in the workspace (base.bbclass's standard tasks included) or by the class itself (also from python, `bb.build.addtask('do_x', ...)`); a function the class uses as a helper is never reported: named in a `[prefuncs]` / `[postfuncs]` flag or a `*FUNCS` / `*_COMMAND` list (`do_qa_sysroot`, `package_do_filedeps`), called as the first word of a line of another function or with `exec_func("<name>"` (`do_package_deb`). An index with no `addtask` at all says nothing about tasks, and every `do_*` counts as one. |
| `org/class-location` | I | layer | Class in `classes/` in a layer all of whose releases search `classes-recipe/` and `classes-global/` (Langdale and later): suggest the directory that matches how the workspace uses it. Not flagged for a layer that claims Kirkstone, or for a class used both globally and by recipes. |
| `org/inherit-defer` | I | file | In a `.bb`, `inherit ${VAR}` where `VAR` may be changed later (a later statement of the file, a bbappend of the recipe, or anonymous python there): suggest `inherit_defer`. Silent unless every release the layer is checked against has `inherit_defer` (Scarthgap and later). |
| `org/append-no-match` | W | layer | A bbappend collected by `BBFILES` that matches no recipe in the workspace (BitBake stops on it). The message lists recipes of the same `PN` if there are any. |
| `org/append-wildcard-broad` | I | layer | A `%` bbappend that matches recipes of two or more versions while it pins version-specific data: a `file://` patch in `SRC_URI`, `SRCREV` / `SRCREV_<name>` (not `AUTOREV`), a set `PV`, or a `SRC_URI` checksum flag. One finding per append. |
| `org/append-missing-filesextrapaths` | E/W | file | A bbappend adds `file://` to `SRC_URI` but neither it nor a file it includes touches `FILESEXTRAPATHS`. Error when a named file exists below the append's directory, else warning. Not flagged when the append sits in the recipe's own directory. |
| `org/policy-in-recipe` | W | file | A recipe or bbappend sets distro or machine policy: `DISTRO_FEATURES`, `MACHINE_FEATURES`, `PREFERRED_PROVIDER_*`, or `IMAGE_INSTALL` outside an image recipe. When the recipe's inherit tree cannot be resolved statically, `IMAGE_INSTALL` is not flagged. |
| `org/recipe-change-in-local-conf` | I | file | `local.conf` carries `:pn-<recipe>` changes that belong in a bbappend or distro conf. In a kas project the message adds that kas rewrites `local.conf` and names the kas file. |
| `org/generated-conf-edit` | W | file | In a kas project, a statement in the kas build directory's `conf/local.conf` or `conf/bblayers.conf` that comes from no `local_conf_header` / `bblayers_conf_header` entry and is not one of kas's own lines: kas rewrites both files on every checkout, shell and build. Names the kas file to put it in. A `BBLAYERS` entry that is no kas layer is reported on its line. Silent outside a kas project, when the kas files could not all be read, and when the file does not look kas-generated. |
| `org/layer-conf` | E/W/I | layer | Problems in `conf/layer.conf`, one check per case, each with its own severity (see the notes): missing `BBFILE_COLLECTIONS`, duplicate collection, `LAYERSERIES_COMPAT` missing, copied or incompatible, unknown `LAYERDEPENDS`, missing `LAYERDEPENDS` while using classes from another layer, `BBFILES` that misses a `.bbappend`, `BBPATH` not extended with `LAYERDIR`. |
| `org/upstream-edit` | W | layer | Uncommitted modifications inside a layer that is not marked writable, from `git status`. Runs only when such a layer is linted (`--all`, or naming it). Any git failure skips the layer silently. |

Notes:

- **`recipe-path`.** The pointer is line 1 of the recipe. Directories that hold several
  `PN`s (`images/`, `packagegroups/`) are not reported, nor is a directory whose name is a
  prefix of the `PN` (`linux/linux-foo_6.6.bb`), nor an image recipe alone in `images/` or
  a packagegroup alone in `packagegroups/` (OE-core's `recipes-core/images/` layout; the
  directory names the kind). An image recipe is one whose static tree inherits a class
  `helpers::image_class` accepts, as for `hygiene/missing-metadata`
  (`core-image-minimal-dev.bb` only requires `core-image-minimal.bb`). A file name with
  more than one `_` makes BitBake read `PN`, `PV` and `PR` from it
  (`bb.parse.vars_from_file`).
- **`duplicate-versions`.** Statements are normalised (lines trimmed, blank and comment
  lines dropped, whitespace collapsed) and compared as a multiset. Left out: `require` and
  `include` lines, `PV`, `PR`, `PE`, `SRCREV*`, `LIC_FILES_CHKSUM` and `SRC_URI` checksum
  flags. The other duplicates are listed as pointers.
- **`include-must-exist`.** Resolution is the scanner's: the including file's directory,
  then `BBPATH`, with only the static variables (`PN`, `BPN`, `PV`, `THISDIR`...) expanded.
  A `require` of an absolute path that exists outside the workspace is not reported.
- **`repeated-logic`.** Function bodies (shell, python, anonymous, top-level `def`) are
  compared after normalisation; names may differ. In assignment blocks, version data is
  ignored. Overlapping windows of one longer block are one finding. The other files are
  listed as pointers (at most five, then a count).
- **`class-location`.** Global use: `base`, a word of a static `INHERIT`, `USER_CLASSES`,
  `PACKAGE_CLASSES` or `INHERIT_DISTRO` value, or an inherit by a class in
  `classes-global/`. Recipe use: an index `inherit` edge from any other file. Neither: the
  message (since lint-precision) says to keep the class in `classes/` if it is meant for
  both contexts, where OE-core keeps `archiver`, `externalsrc` and the like (BitBake
  searches `classes/` after `classes-<type>/`, `BBHandler.py`), and names both directories
  otherwise.
- **`inherit-defer`.** The rule's release range starts at Scarthgap, so it runs for a layer
  checked against Scarthgap or later; it then stays silent when any of the layer's releases
  lacks `inherit_defer`, since the suggestion would not parse there.
- **`append-no-match`.** A bbappend that `BBFILES` does not collect is reported by
  `org/layer-conf`, not here.
- **`policy-in-recipe`.** An image recipe is one that inherits `image` or `core-image`, or a
  class whose name ends in `-image`, directly or through its static tree (for a bbappend:
  a recipe it applies to).
- **`generated-conf-edit`.** kas writes each header entry as `# <id>` then its text, in id
  order, then `MACHINE ??=`, `DISTRO ??=` and `BBMULTICONFIG ?=` (`local.conf`), or the
  `BBLAYERS ?=` list, `BBPATH ?= "${TOPDIR}"` and `BBFILES ??= ""` (`bblayers.conf`); see
  "kas" in bitbake-semantics.md. A statement is kas's when every line of it is a line of a
  header entry of the current kas files or one of those lines (with any value: `KAS_MACHINE`
  and the like change them). The rule runs only when the workspace's kas files were all
  read (no unreadable include), the build directory is the kas build directory, and the
  directory has kas's `CACHEDIR.TAG` or the file has kas's own lines. A line left from a
  header entry that was since removed from the kas files is reported: it will be lost too.
  A `BBLAYERS` entry is judged against the kas layers, whose remote repositories are in
  the kas work directory; that directory comes from `KAS_WORK_DIR` or the kas build
  directory's parent, not from where sous-chef runs, and when it can only be guessed (no
  kas build directory) and the project has a remote repository, no entry is reported.
  The kas file named is the first project file with a header of that kind, else the last
  file on the command line.
- **`layer-conf`.** The checks and their severities:

  | Check | Sev |
  |---|---|
  | no `BBFILE_COLLECTIONS` | W |
  | a collection another layer of the workspace also adds | E |
  | `LAYERSERIES_COMPAT_<c>` not set or empty | W |
  | `LAYERSERIES_COMPAT_<c>` copies `${LAYERSERIES_COMPAT_...}`: BitBake 2.4 (Mickledore) and later delete each layer's value after its `layer.conf`, so the reference stays literal and BitBake stops; the message lists the release names to write | E (W when the workspace release is Kirkstone or Langdale, whose BitBake 2.0 / 2.2 expands the copy and accepts the layer, and no `--as-release` adds a later one; `Feature::KeptUntilLangdale`) |
  | shares no name with the core layer's `LAYERSERIES_CORENAMES` | E |
  | no core layer in the workspace, and the compat does not name the workspace release, unless that release was only assumed | W |
  | names a release older than Kirkstone | I |
  | names a release the release table does not know | I |
  | `LAYERDEPENDS` names a collection no layer adds | E |
  | a file inherits a class from another layer whose collection is not in the layer's `LAYERDEPENDS`, followed transitively | W |
  | a `.bbappend` outside `dynamic-layers/` that no `BBFILES` glob collects | W |
  | no `BBPATH` statement mentions `LAYERDIR` and the layer root is not in `BBPATH` | E when the layer has classes or `.conf` files other than `conf/layer.conf`, else W |

  Only the first collection's `LAYERSERIES_COMPAT` is checked. The pointer is the relevant
  statement of `conf/layer.conf`, or line 1. The BitBake behaviour behind each severity is
  in bitbake-semantics.md ("Layers").
- **`upstream-edit`.** In the layer root it runs `git rev-parse --show-prefix`,
  `git ls-files -- conf/layer.conf` (a layer whose `layer.conf` is not tracked is skipped)
  and `git status --porcelain=v1 -z --untracked-files=all -- .`, as argv (no shell, `GIT_*`
  location variables removed), each with a 5 s timeout. Reported, one finding per path on
  line 1: tracked files modified, added, deleted, renamed or copied, and untracked BitBake
  files. At most 20 findings, then one on `conf/layer.conf` with the count of the rest.

## Group `hygiene` (task T4.4)

| Id | Sev | Scope | Flags |
|---|---|---|---|
| `hygiene/missing-metadata` | W/I | recipe | No `SUMMARY` or `LICENSE`, or no `LIC_FILES_CHKSUM` when `LICENSE` is not `CLOSED` and `SRC_URI` is not empty (W, one finding); no `HOMEPAGE` (I) when the recipe has an upstream: a `SRC_URI` entry that is not `file://`, and not an image or a packagegroup. Configuration defaults do not count as set. Values that depend on python are not reported. (Since x1) `LIC_FILES_CHKSUM` is not asked of a recipe the evaluation says BitBake would skip under the configuration evaluated (no license QA runs there). |
| `hygiene/git-pv` | I | recipe | (since x7) A `<pn>_git.bb` recipe fetching from `git://` or `gitsm://` whose `PV` comes only from `bitbake.conf`'s default, so it is the file name's version part `git` (BitBake's `vars_from_file`; yocto-kiss's `u-boot-kiss`: `PV="git"` in `bitbake -e`). Without a version number or a `+`, `package.bbclass` adds no source revision to `PKGV`, so a new `SRCREV` builds packages of the same version. Info: OE-core itself has three such recipes (`sato-icon-theme_git.bb`, ...). Not reported when python sets `PV`. No fix (the version is not known). For a layer checked against a release up to Mickledore (`KeptUntilMickledore`) the message suggests `PV = "<version>+git${SRCPV}"`: there `package.bbclass` adds no revision to `PKGV` at all (Kirkstone's `package_convert_pr_autoinc` only replaces `AUTOINC`; acc-releases). |
| `hygiene/insane-skip` | I | file | (since x7) `INSANE_SKIP` (any override, not `:remove`, not a flag) of `ldflags`, `textrel` or `buildpaths` in a `.bb`, `.bbappend` or `.inc`: QA checks of `ERROR_QA` whose failure usually means the recipe builds wrongly (no distro `LDFLAGS`, text relocations, host paths in the output). Other skips (`dev-so`, `arch`, `already-stripped`, ...) are routine and not reported; nor are classes (`go.bbclass` skips `ldflags` on purpose) or a file that inherits `bin_package`. |
| `hygiene/autorev` | W | file | `SRCREV` or `SRCREV_<name>` (any operator or override, not a flag) whose value contains `${AUTOREV}`, outside `local.conf` and `auto.conf`. `SRCREV_FORMAT` is not checked. |
| `hygiene/src-uri-checksum` | W | recipe | Remote download (`http`, `https`, `ftp`) in the evaluated `SRC_URI` without a checksum; `git://` / `gitsm://` URL without a revision, or without `branch=` (or `nobranch=1`) unless a `tag=` is given. E for a recipe checked against Walnascar or later (BitBake 2.12 stops on it): a git URL without `branch=` / `nobranch=1` / `usehead=1`, a `tag=` notwithstanding, or with several comma-separated branches; the message names those releases. Not reported when `SRC_URI` depends on python, for an entry still containing `${` after expansion, or (since x1) when the evaluation says BitBake would skip the recipe under the configuration evaluated (a static "would skip" note: it fetches nothing there, and a value per machine says nothing about the others). |
| `hygiene/host-path` | W | file | Absolute host paths (`/usr/lib`, `/etc`, `/home/...`) at the start of a word in `do_<task>` shell bodies, instead of `${D}`, `${S}`, `${bindir}` and friends; a `do_install` that never references `${D}` while running a command that writes files itself. Not scanned: other functions, comments, here-documents, the command word, values after `=`; `/dev`, `/proc`, `/sys`, `/tmp` are never reported. Data is not host access: the arguments of `echo`, `printf` and `bb*` messages, quoted arguments of `sed`, `grep`, `awk`, `perl` (expressions, patterns, shebangs), the target of `ln -s`; a redirection target always is. |
| `hygiene/workdir-s` | E | file | `S = "${WORKDIR}"` (or `${WORKDIR}/`), reported only when the file's releases include one that rejects it (Styhead and later), naming them. A layer checked only against releases that accept it gets nothing. |
| `hygiene/variable-order` | I | file | In a `.bb`, the first statement out of the OE style guide order (description block, license, dependencies, `SRC_URI`, `S`, inherit, `PACKAGECONFIG` and build options, tasks, packaging), once per file. Off by default: runs only when selected by its id, the alias `style/variable-order` or the alias group `style`. |
| `hygiene/tabs` | I | file | Tabs in shell task bodies mixed with spaces; python bodies not indented with 4 spaces. At most one finding per function. |
| `hygiene/continuation` | W | file | Whitespace after a trailing `\`; a blank line that ends a continued statement before its closing quote. A comment line inside a continued statement is reported by `hygiene/parse-error`, not here. |
| `hygiene/release-syntax` | E/W | file | A construct that one of the releases the file is checked against lacks, from the `Feature` table: `inherit_defer` before Scarthgap (not in a `.conf`, where `hygiene/parse-error` reports it), `include_all` before Walnascar, a class in `classes-recipe/` or `classes-global/` on Kirkstone, `${UNPACKDIR}` before Styhead (unless a file of the same layer, or of a layer other than the core layer, assigns `UNPACKDIR`: the core layer's own assignment comes with the workspace's release), `addfragments` before Walnascar, and its three-argument form from Whinlatter (BitBake 2.16 needs a fourth argument). Warning: `${WORKDIR}/<file>` for a `file://` entry of the same file's `SRC_URI` on Styhead and later (unpacked into `UNPACKDIR`). The message names the releases. |
| `hygiene/release-removed` | E/W | file | Something a release the file is checked against removed, renamed or deprecated, from one table (`REMOVALS` in the rule's file, one `KeptUntil*` `Feature` per migration guide; see "Release removals" below): a renamed variable (`BB_RENAMED_VARIABLES`, BitBake's own renames; E: BitBake stops), a variable OE-core dropped (W), an `inherit` (or `INHERIT`-style list word) of a removed class (E, unless a layer other than the core layer has the class), a removed word in `IMAGE_FEATURES` (`debug-tweaks`, E) or a QA list (`license-incompatible`, W), an old virtual toolchain provider name (W), a `SRC_URI` scheme whose fetcher is gone (E); `S` below `${WORKDIR}` or `S = "${UNPACKDIR}/git"` from Whinlatter (E); `LAYERSERIES_COMPAT_<c> = "${LAYERSERIES_CORENAMES}"` from Mickledore (E); an obsolete license name in `LICENSE` (W, E from Styhead); an assignment without whitespace around its operator from Whinlatter (W); (since x7) `${SRCPV}` in `PV` when every release the file is checked against is Nanbield or later (I: from Nanbield `SRCPV = ""` and the revision goes to `PKGV`; not when a layer other than the core layer assigns `SRCPV`). The message names the releases. |
| `hygiene/parse-error` | E/W | file | Any `Unknown` statement no parser diagnostic covers, and parser diagnostics. Not reported: whitespace after a trailing `\` (left to `hygiene/continuation`). `addfragments` with three or more arguments and `addpylib` with two or more are statements of their own; with fewer each is an unparsed line. `addpylib` is an error outside the base configuration (`conf/layer.conf`, `conf/bitbake.conf`, `conf/bblayers.conf`; BitBake rejects it in a recipe, a class, an `.inc` and any `.conf` those include), and in the base configuration on a release before Mickledore (BitBake 2.4), which has no such directive. Data files are not linted (bitbake-semantics.md, "Data files"): a `.conf` / `.inc` / `.bbclass` in a recipe's `<pn>/`, `<pn>-<pv>/` or `files/` directory, of any `.bb` / `.bbappend` whether BitBake collects it or not (meta-oe's `dynamic-layers/` recipes whose collection is absent included), or under `lib/`, `scripts/`, `wic/`, or one no reference can reach that does not parse (not in `conf/`, not a class in `classes*/`, not at a layer root, not in or below the directory of a file with a reference sous-chef cannot expand). It runs (alone of the file rules) on a `.bb` / `.bbappend` only an inactive `BBFILES_DYNAMIC` entry collects: BitBake parses it wherever the collection is present. Warning: the `addtask` quirk that joins two `before`/`after` groups into one task name. Error: everything else. In a `.conf` file, a function, `def`, `inherit`, `inherit_defer`, `addtask`, `deltask`, `addhandler` or `EXPORT_FUNCTIONS` is an error (BitBake's configuration parser rejects them). |

Notes:

- **`missing-metadata`.** Recipe scope on the evaluated recipe, so its `.inc` files,
  bbappends and classes count. A variable counts as set when an operation from a non-`.conf`
  file contributes to it (any status but an inactive override) and its value is not empty;
  OE-core's `bitbake.conf` defaults (`SUMMARY ?=`, `LICENSE ??= "INVALID"`,
  `HOMEPAGE = ""`) do not count. The `LIC_FILES_CHKSUM` condition is OE-core's
  `populate_lic_qa_checksum`. A variable whose value is python, or that anonymous python
  sets, is not reported. Both findings are on line 1 of the `.bb`. `HOMEPAGE` is the
  upstream project's page, so it is not asked of an image (a visited or directly inherited
  class `image`, `core-image` or `*-image`), a packagegroup (`packagegroup`), or a recipe
  whose `SRC_URI` is empty or only `file://` entries (OE-core's `connman-conf`,
  `base-files`). A remote URL written out in `SRC_URI` counts even when other parts of the
  value are python; a value with python and no such URL counts as no upstream. OE-core's
  own images and packagegroups set `SUMMARY` and get `LICENSE` from their class (`LICENSE ?= "MIT"`, which counts: a class is not
  configuration); only four of its 37 set `HOMEPAGE`. `SUMMARY` and `LICENSE` are asked of
  every recipe.
- **`src-uri-checksum`.** A checksum is a `sha256sum` flag on `SRC_URI` (`<name>.sha256sum`
  with a `name=` parameter), a `sha256sum=` parameter, or an `md5sum`, `sha1sum`,
  `sha384sum` or `sha512sum` flag or parameter. A git revision is a `rev=` or `tag=`
  parameter, or a value of `SRCREV_<name>` (with `name=`) or `SRCREV` that is not empty and
  not OE-core's `INVALID` default; a python value (`${AUTOREV}`) counts as set. The finding
  is on the last applied `SRC_URI` operation whose expanded value contains the entry (which
  may be in an `.inc` or a bbappend), else line 1 of the recipe.
- **`host-path`.** Only shell functions named `do_<task>` (any override) are scanned;
  `pkg_postinst` and friends run on the target, where absolute paths are right. A path after
  `${D}`, `$D` or any other variable is not at the start of a word. The `do_install` check
  applies to a whole definition (not `:append` / `:prepend`) that mentions neither `${D}`
  nor `$D` and runs `install`, `cp`, `mkdir`, `ln`, `mv`, `rsync`, `tar`, `touch`, `cat` or
  `echo` as the first word of a line; it is not reported when the body calls a
  `*_do_install` function, `oe_runmake`, or sets `DESTDIR`, or only calls other functions.
  Each path is judged by the command it belongs to: the line is split into words at
  unquoted spaces, a command starts after `|`, `||`, `&&`, `;` or `&` (keywords such as
  `if`, `then`, `!` and `VAR=value` prefixes skipped), and a line ending in `\` carries its
  command to the next. Still reported, and right: `cp "/etc/x" ...`, `sed -i '...' /etc/x`,
  `grep x /etc/passwd`, `echo x > /etc/x`, the link name of `ln -s`. Not told apart (so not
  reported): a quoted file name given to `sed` / `grep` (`sed -i '...' "/etc/x"`), and a
  path that another command only prints or stores; a path glued to a redirection
  (`>/etc/x`) is not at the start of a word and is missed.
- **`release-removed`.** One table drives most of it (`REMOVALS` in
  `src/lint/rules/hygiene_release_removed.rs`): kind, name, the `KeptUntil*` `Feature`
  whose last release still has it (none: a rename older than Kirkstone), severity, what to
  write instead. "Release removals" below lists every migration-guide change from 4.1 to
  6.0 and where it is covered. A renamed variable is reported on any assignment (a flag
  included: `setVarFlag` checks too). A dropped variable is reported on an assignment
  without a flag (no effect) and on a `${NAME}` reference, unless the file or a layer other
  than the core layer assigns it. A removed class is reported on `inherit` / `inherit_defer`
  and on a word of `INHERIT`, `USER_CLASSES`, `IMAGE_CLASSES`, `KERNEL_CLASSES` or
  `PACKAGE_CLASSES`, unless a layer other than the core layer has a class of that name. A
  removed value word is not reported in a `:remove` operation. `S` checks skip `.conf`
  files (the default comes with the release's core layer) and conditional names. The
  whitespace check reads the first line of an assignment as BitBake 2.16's
  `__config_regexp__` does (`FOO="x"`, `FOO= "x"`, `FOO ="x"`).
- **`workdir-s`.** Matched: an assignment to `S` (no flag; not `+=`-style, not `:append`,
  `:prepend` or `:remove`) whose value is `${WORKDIR}` or `${WORKDIR}/`. Since every layer
  is checked against the workspace's release, a Styhead or later workspace reports it
  everywhere. Source: the 5.1 migration guide (`Feature::WorkdirAsSourceDir`).
- **`variable-order`.** Statements that belong to no group (unknown variables, anonymous
  python, `def`, `addhandler`, includes) are ignored. The finding names the statement the
  reported one should precede.
- **`tabs`.** Shell: the first indented line sets the style (tab or space as first
  character); the first later line starting with the other one is reported; here-document
  bodies are skipped. Python and `def`: a tab anywhere in the indentation, or a first
  indented code line not indented by exactly 4 spaces.

## Fixes

`sous-chef lint --fix [TARGET]` (the `lint` tool: `fix: true`) is a dry run: per file a
unified diff of what it would change, and a summary (fixes available, safe and suggested, by
rule). `--write` (`write: true`) applies; `--fix-suggested` (`fix_suggested: true`) also
applies the suggested fixes. Target, `--rule` and `--severity` select the findings as for
lint; only a finding the run reports (after suppression and filters) is fixed. The engine
is `src/lint/fix.rs`; the exit status is that of the findings left.

A rule attaches a fix to a finding (`Rule::fixes`): edits of one file (byte ranges of the
text as it was read, located from the parsed statement and read back against it), a
one-line description and a class:

- **safe**: preserves what the metadata means, or is exactly what BitBake requires (the
  rule's own correction). `--fix` applies it.
- **suggested**: changes meaning the way the rule recommends, or needs a human look. Shown
  with its own diff; applied only with `--fix-suggested`.

Applying: the edits of one file become one scaffold `Modify` (writable layers only,
`Workspace::confine`, the file must still hash to what was read and to what the index
holds, temporary file and rename); a file in a layer that is not writable, or in the build
directory, is never touched and its fixes are listed. Fixes whose edits overlap one already
taken wait (dry run: "re-run after applying"). After writing, the index is refreshed and the
changed files are linted again; fixes that became available there (an overlap, a finding a
fix made visible) are applied in further rounds, at most five, so a second `--fix --write`
changes nothing. The report gives, for the changed files, the findings fixed and remaining,
**new** ones (a new finding is a bug in a fix and is reported as such) and **revealed** ones:
findings on a line whose legacy name a fix converted, which the other rules could not see
before because they skip legacy names (`EXTRA_OECONF_remove` in a recipe becomes
`override/remove-in-recipe`'s `:remove`). Everything outside an edit is kept byte for byte
(line endings, trailing whitespace, comments).

Per rule (proofs: `tests/lint_fix.rs`; a `bad.*` or `fixable.*` fixture is fixed into the
`fixed.*` beside it byte for byte, the result has no finding of the rule and no new finding
of any file-scope rule, and fixing it again changes nothing):

| Rule | Fix | Transformation, and why it is safe or only suggested |
|---|---|---|
| override/legacy-syntax | safe; suggested for a machine / distro suffix alone | The name as written becomes the current syntax, `_` to `:` at exactly the split the rule reports and nothing else (flag, operator, value, function body unchanged): `VAR_append` `VAR:append`, `RDEPENDS_${PN}` `RDEPENDS:${PN}`, `do_install_append()` `do_install:append()`, `VAR_append_qemux86` `VAR:append:qemux86`; a parser-detected name whose base ends in a machine, distro, `class-*` or `pn-*` element gets those too (`VAR_qemux86_append` `VAR:qemux86:append`, what the old name meant; the message says `VAR_qemux86:append`). Safe: pre-honister BitBake used `_` with exactly the rules `:` has now, and the old spelling is fatal (`_append`, `_prepend`, `_remove`) or a variable nothing reads. A suffix-only name (`KERNEL_DEVICETREE_qemux86`) is safe for `class-*` / `pn-*` elements, suggested for a machine or distro name (a current layer may read `FOO_${MACHINE}` as a variable of its own). Proof: `bad.bb`, `bad.conf`, `bad-more.bb` (and CRLF) to `fixed*`; the evaluation test `legacy_syntax_fix_gives_what_the_legacy_file_meant`. References in values and python (`${RDEPENDS_${PN}}`, `d.getVar('X_' + pkg)`) are not touched: the rule does not report them |
| override/append-no-space | safe | One space after the opening quote (`:append`) or before the closing one (`:prepend`) of a space-list variable: the separator BitBake does not add. Proof: `bad.bb`, `bad-ref.bb`; evaluation `DEPENDS` `basezlib` to `base zlib` |
| override/plus-equals-with-override | suggested | `VAR:foo += "x"` to `VAR:append:foo = " x"` (`=+`: `:prepend` with the trailing space; `.=` / `=.` without a space); the rule's point, and a meaning change (`VAR:foo` set elsewhere no longer gets this part) |
| override/remove-in-recipe | none | Needs a decision: where to leave the item out instead (the value is set elsewhere, or belongs behind a variable or `PACKAGECONFIG`) |
| override/dead | none | The intended override is a guess (the suggestion is the closest known name, which may be wrong); renaming or removing the line is the author's call |
| override/shadowed | none | Recipe scope, judged from one evaluation; removing the assignment must hold for every machine, distro and variant that reaches the file |
| override/weak-after-default | safe | The `??=` statement's whole lines are removed. It never applies: the name already has a value from the earlier `=` / `:=` / `?=` of the file, a weak default is used only for a name without one, and the `unset` that would clear the value clears the default with it. Not for `export NAME ??=` (it also sets the export flag). Evaluation: every value equal |
| override/missing-package | suggested | `:${PN}` after the base name (`RDEPENDS =` `RDEPENDS:${PN} =`, `pkg_postinst()` `pkg_postinst:${PN}()`); which package is meant is a decision |
| override/append-vs-plus-equals | suggested | `VAR:append = " x"` to `VAR += "x"` (one leading space taken out, the one `+=` adds), else `VAR .= "x"`; the same value at the end of the file, not when a bbappend or conf later sets `VAR` or an override of it |
| override/plus-equals-on-default | suggested | `VAR += "x"` to `VAR:append = " x"` (`=+` to `:prepend`, `.=` / `=.` without a space), as the message says; keeps the `?=` default, and also survives a later plain `VAR =`. Usually in the build directory's `local.conf`, which is never written: the fix is shown |
| override/filesextrapaths | safe; suggested for `:=` with other references and for a replaced list | The missing colon inserted at the quote (safe: the separator; not when a blank sits between quote and path). `=` to `:=` on `:prepend` / `:append` (safe when the value refers only to `${THISDIR}`, `${FILE_DIRNAME}`, `${FILE}`; suggested when it refers to more, which `:=` freezes too: `${PN}` in a bbappend comes from the append's file name, in an `.inc` it is `defaultpkgname`, never a `BBCLASSEXTEND` variant's). A plain unconditional `FILESEXTRAPATHS =` / `:=` outside a `.conf` to `FILESEXTRAPATHS:prepend :=` (suggested). Other operators, conditional names and `.conf` forms: none. Proof: `fixable.bbappend`, `bad-dot.bb` |
| org/recipe-path | none | Would rename or move files |
| org/duplicate-versions | none | Restructures several files into a new `.inc` (`scaffold kind=inc_split` does it with a plan to review) |
| org/inc-has-version-data | none | Moves version data from a shared `.inc` into each `.bb`: several files and a decision |
| org/inc-unused | none | Would delete a file another layer may still use through a path the index cannot resolve |
| org/include-must-exist | safe (`include` only) | `include` to `require` for a file the index resolved: BitBake reads a found file the same way (`IncludeNode.eval` passes `error_out`, which matters only when the file is missing), so a missing file now stops the parse, the rule's correction. A `require` that does not resolve: none (the right path is unknown) |
| org/repeated-logic | none | Needs a new class or `.inc` and edits to several recipes |
| org/class-recipe-specific | none | Needs a decision (`?=`, or moving the setting to the recipes) |
| org/class-export-functions | suggested | `EXPORT_FUNCTIONS do_<task>` as a line after the function (after: `ExportFuncsNode.eval` copies the `python` flag when the line is parsed), and for a `do_<task>` defined directly the rename to `<class>_do_<task>`. On OE-core (wrynose) the rule found 23 functions before x7, nearly all helpers (`postfuncs`, `PACKAGEFUNCS`, `exec_func`); since x7 one, `kernel.bbclass`'s `do_kernel_version_sanity_check`, a task `kernel-yocto.bbclass` adds. Review before `--fix-suggested` |
| org/class-location | none | Would move a file |
| org/inherit-defer | suggested | `inherit` to `inherit_defer`: the class is chosen by the final value, and is parsed after the recipe (its plain assignments now win over the recipe's) |
| org/append-no-match | none | Removing the append or adding a layer is a decision |
| org/append-wildcard-broad | none | Would rename a file |
| org/append-missing-filesextrapaths | none | Which directory holds the files (`files/`, `${PN}/`, `${BP}/`) is the layer's layout, not the rule's |
| org/policy-in-recipe | none | Moves policy to a distro or machine conf: another file and a decision |
| org/recipe-change-in-local-conf | none | Moves the change to a new bbappend; `local.conf` is in the build directory, which is never written |
| org/generated-conf-edit | none | The kas file is outside the writable layers |
| org/layer-conf | suggested for two checks | `BBPATH .= ":${LAYERDIR}"` before the first statement of `conf/layer.conf` when `BBPATH` is not extended; the line the message names, `LAYERDEPENDS_<c> += "<other>"`, after the last `LAYERDEPENDS_<c>` when a class comes from a layer it does not depend on. The other checks need a decision (a collection name, the releases to claim, a glob) |
| org/upstream-edit | none | The layer is not writable by definition |
| hygiene/missing-metadata | none | The values (summary, license, checksum) are not known |
| hygiene/git-pv | none | The version is not known |
| hygiene/insane-skip | none | Fixing the build (passing `LDFLAGS`, `-fPIC`) is not a text edit |
| hygiene/autorev | none | Needs a real revision |
| hygiene/src-uri-checksum | none | Needs the download's checksum |
| hygiene/host-path | none | Which variable (`${D}`, `${sysconfdir}`, ...) is meant is a decision |
| hygiene/workdir-s | safe (since x7), when every release the file is checked against has `UNPACKDIR` | `S = "${WORKDIR}"` / `"${WORKDIR}/"` (any override) to `S = "${UNPACKDIR}"`, the form of the 5.1 guide ("The recipe should be changed to: S = "${UNPACKDIR}""). A layer that also claims a release before Styhead (no `UNPACKDIR` there): none, and the message says so ("no automatic fix while the layer also claims scarthgap, which has no UNPACKDIR"). Evaluation: only `S` (and what is set from it, `B`) changes, to the value of `UNPACKDIR` |
| hygiene/variable-order | none | Moving statements changes parse order (`?=`, `+=`, `:=` depend on it) |
| hygiene/tabs | none | Not exact: the tab width is unknown, python indentation is syntax, here-documents are content |
| hygiene/continuation | safe (whitespace only) | The whitespace between a trailing `\` and the end of the line is removed (the terminator stays): BitBake right-strips every line before joining, so it reads the file exactly as before. Evaluation: every value equal. The blank line ending a statement: none (the blank line or the `\` may be the mistake) |
| hygiene/release-syntax | safe for one case (since x7) | `${WORKDIR}/<file>` to `${UNPACKDIR}/<file>`, every such reference of the statement, where `<file>` is a `file://` entry of the same file without `subdir=` (5.1 guide: "If they reference files from SRC_URI, they likely need changing to UNPACKDIR"), only when every release the file is checked against has `UNPACKDIR`; otherwise none, and the message says so. Other `${WORKDIR}` text (a `sed` expression, a file that is no `SRC_URI` entry) is left alone. The other cases (`inherit_defer`, `include_all`, `addfragments`, `${UNPACKDIR}` on an old release, class directories): none, they need a decision (an older release is claimed) |
| hygiene/release-removed | safe (since x7) for two cases | A renamed variable with a one-to-one new name whose value keeps its meaning (`PURE_RENAMES` in the rule's file: the `BB_*_WHITELIST` names, `PNBLACKLIST`, `MULTI_PROVIDER_WHITELIST`, `SDK_*`, `SSTATE_DUPWHITELIST`, `SYSROOT_DIRS_BLACKLIST`, `UNKNOWN_CONFIGURE_WHITELIST`, `LICENSE_FLAGS_WHITELIST`, `*_FEATURES_BACKFILL_CONSIDERED`, each checked against OE-core's `BB_RENAMED_VARIABLES` or BitBake's `bitbake_renamed_vars`): renamed, overrides, flag, `export`, operator and value kept, only when no release the file is checked against still has the old name. `${SRCPV}` in `PV`: removed (it is empty on every release the finding is reported for). None for: a removed variable or class, a rename that is a removal ("remove it"), `WHITELIST_<license>` (`INCOMPATIBLE_LICENSE_EXCEPTIONS` takes `<pkg>:<license>`), `CVE_CHECK_*` and `ICECC_*` (their classes are gone), the other checks. Evaluation: the new name holds the old one's value, nothing else changes |
| hygiene/parse-error | none | What an unparsed line meant is unknown |

Compared with OE-core's `scripts/contrib/convert-overrides.py` (0.9.3), which rewrites any
text that looks like an override with regular expressions (names, values, python strings,
comments, function bodies) from a fixed list of overrides plus `--override`: on the
fixtures and on a dozen old-style recipes the two agree on every name the rule reports. The
script's package variables (`packagevars`, `imagevars`) are the rule's too since x7
(`SYSTEMD_SERVICE_${PN}`, `INITSCRIPT_PARAMS_${PN}`: until x7 a miss of the rule); the script rewrites
text inside function bodies and comments (`: # do_install_append` in a shell body); it does
not convert `PREFERRED_PROVIDER_virtual/kernel_<machine>` (its regular expression stops at
`/`), which the rule reports and the fix converts (suggested). The script warns that its
matching is loose (`_append` inside other names must be filtered by hand); the fix converts
only names the parser or the suffix check identifies.

## Release removals

Every change in the Yocto migration guides for 4.1 to 6.0 (https://docs.yoctoproject.org/migration-guides/)
that a layer maintainer must act on, and how lint covers it. "Feature" is the
`src/release.rs` entry, whose source is in the "Releases" table of bitbake-semantics.md;
"added" means added on branch `x2-migrate`. The `release-removed` rows live in the table
`REMOVALS` of `src/lint/rules/hygiene_release_removed.rs`. Not detected: a change that needs
a build, the target release's own metadata (its recipes and class layout), or that only
changes a default; and a few the guide names but the source contradicts.

| Release | Change | Covered by |
|---|---|---|
| (before 4.0) | Variable renames of 4.0 (`BB_ENV_WHITELIST`, `PNBLACKLIST`, `LICENSE_FLAGS_WHITELIST`, `WHITELIST_GPL-3.0`, ... : BitBake's `bitbake_renamed_vars` and OE-core's 27 `BB_RENAMED_VARIABLES`) | `release-removed`, no Feature (every supported release stops on them); added |
| (before 4.0) | Obsolete license names (`GPLv2`, `LGPLv2.1+`, ...): OE-core `obsolete_license_list` | `release-removed`: W, E from 5.1 (`KeptUntilScarthgap`); added |
| 4.1 | Classes split into `classes-recipe/` / `classes-global/` | `SplitClassDirs`: `release-syntax` (a split class on Kirkstone), `org/class-location`; a class used in the wrong context (`INHERIT += "testimage"`) is not detected: it needs the target release's class layout |
| 4.1 | A missing local file in `SRC_URI` is an error | not detected (`FILESPATH` lookup per override) |
| 4.1 | `PEP517_BUILD_API` removed | `release-removed`, `KeptUntilKirkstone`; added |
| 4.1 | buildpaths QA in `WARN_QA`, shebang size, `help2man-native` dependency, recommends of complementary and `-dev` packages, `IMAGE_BUILDINFO_FILE` / `ARCHIVER_OUTDIR` defaults, `icu-config`, rng-tools service name, `largefile` feature, connman `PACKAGECONFIG`, wpa-supplicant defconfig, `mount.blacklist` | not detected (build-time, runtime or a recipe's own defaults) |
| 4.1 | Removed recipes (`lzop`, `alsa-utils-scripts`, ...) | not detected (needs the target release's recipes) |
| 4.2 | `addpylib` | `AddPyLib`: `hygiene/parse-error` |
| 4.2 | `SERIAL_CONSOLE`, `PACKAGEBUILDPKGD` removed | `release-removed`, `KeptUntilLangdale`; added |
| 4.2 | Classes `rust-bin`, `package_tar` removed | `release-removed`, `KeptUntilLangdale`; added |
| 4.2 | `LAYERSERIES_COMPAT` copying `LAYERSERIES_CORENAMES` or another layer's value | `release-removed` (`${LAYERSERIES_CORENAMES}`, added); `org/layer-conf` (`${LAYERSERIES_COMPAT_...}`, made release-aware: `KeptUntilLangdale`) |
| 4.2 | `OEBasic` signature handler removed | `release-removed` (`BB_SIGNATURE_HANDLER`), `KeptUntilLangdale`; added |
| 4.2 | crate checksums, patch-fuzz / patch-status in `ERROR_QA`, `QB_SLIRP_OPT`, `IMAGE_LINK_NAME`, kmod / cups / openssh / scons defaults, `PKG` conflicts | not detected |
| 4.3 | `PERLARCH`, `PERLVERSION`, `QEMU_USE_SLIRP`, `SERIAL_CONSOLES_CHECK` removed | `release-removed`, `KeptUntilMickledore`; added |
| 4.3 | Class `glide` removed | `release-removed`, `KeptUntilMickledore`; added |
| 4.3 | `PE` / `PR` no longer in `WORKDIR` | `PrInWorkdir` (evaluator); no lint finding (paths only) |
| 4.3 | `AUTHOR` removed | not added: OE-core only documented it (`documentation.conf`), so nothing changes |
| 4.3 | `CVE_CHECK_IGNORE` deprecated, `SRCPV` no longer needed, `OLDEST_KERNEL`, `FILE_LAYERNAME` patch QA, 64-bit `time_t`, `usrmerge` with systemd, `IMAGE_NAME_SUFFIX`, `imgsuffix`, `virtual/` provider prefixes, removed recipes | not detected (deprecations that still work, build-time checks) |
| 5.0 | `inherit_defer` | `InheritDefer`: `release-syntax`, `org/inherit-defer` |
| 5.0 | `DEPLOY_DIR_TAR`, `oldincludedir` removed | `release-removed`, `KeptUntilNanbield`; added |
| 5.0 | `PYTHON_PN`, `USE_L10N`, `CVE_SOCKET_TIMEOUT` removed | not added: OE-core's `python3-dir.bbclass` still sets `PYTHON_PN` on wrynose; `USE_L10N` is in no OE-core branch checked; `CVE_SOCKET_TIMEOUT` is back in `cve-update-db-native.bb` on whinlatter |
| 5.0 | Go `linkmode`, systemd `nss-resolve`, ipk zstd, `virtual/` in runtime contexts, removed recipes | not detected |
| 5.1 | `S = "${WORKDIR}"` rejected | `WorkdirAsSourceDir`: `hygiene/workdir-s` |
| 5.1 | `UNPACKDIR`; `${WORKDIR}/<file>` for local sources | `UnpackDir`: `release-syntax` (made to ignore the core layer's own `UNPACKDIR`) |
| 5.1 | `TCLIBCAPPEND`, `VOLATILE_LOG_DIR`, `VOLATILE_TMP_DIR` (renamed variables) | `release-removed`, `KeptUntilScarthgap`; added |
| 5.1 | Class `siteconfig` removed | `release-removed`, `KeptUntilScarthgap`; added |
| 5.1 | `license-incompatible` QA check renamed `license-exception` | `release-removed`, `KeptUntilScarthgap`; added |
| 5.1 | insane checks promoted to errors | `obsolete-license` only (license rows above); the others run at build time |
| 5.1 | Go `GO_SRCURI_DESTSUFFIX`, `${WORKDIR}/${BP}`, removed recipes, perf / openssl / gobject-introspection packaging | not detected |
| 5.2 | `include_all`, `addfragments`, `BB_RECIPE_VIRTUAL_PROVIDERS` | `IncludeAll`, `AddFragments`, `RecipeVirtualProviders`: `release-syntax` (`addfragments` added on this branch) |
| 5.2 | Git URL without `branch=` is an error; one branch per URL | `hygiene/src-uri-checksum`, made release-aware (`KeptUntilStyhead`) |
| 5.2 | `debug-tweaks` removed from `IMAGE_FEATURES` | `release-removed`, `KeptUntilStyhead`; added |
| 5.2 | Virtual toolchain providers renamed (`virtual/${TARGET_PREFIX}gcc` to `virtual/cross-cc`, ...) | `release-removed`, `KeptUntilStyhead`; added |
| 5.2 | `BB_DANGLINGAPPENDS_WARNONLY` (renamed variable), `PACKAGE_SNAP_LIB_SYMLINKS`, `ACLOCALDIR`, `ACLOCALEXTRAPATH` | `release-removed`, `KeptUntilStyhead`; added |
| 5.2 | Class `migrate_localcount` removed | `release-removed`, `KeptUntilStyhead`; added |
| 5.2 | `SETUPTOOLS_INSTALL_ARGS` removed | not added: `setuptools3_legacy.bbclass` still uses it on wrynose |
| 5.2 | `CARGO_INSTALL_LIBRARIES`, systemd `Also=` / split-usr / `pni-names`, `BB_CURRENT_MC`, `configure` required, `UBOOT_ENTRYPOINT` `0x`, `ld-is-gold`, `ZSTD_COMPRESSION_LEVEL`, `FIT_UBOOT_ENV`, removed / renamed recipes | not detected |
| 5.3 | `S` below `${WORKDIR}`; `S = "${WORKDIR}/git"` / `"${UNPACKDIR}/git"` | `release-removed`, `KeptUntilWalnascar`; added |
| 5.3 | Whitespace around `=` | `release-removed`, `AssignmentWhitespaceWarning`; added |
| 5.3 | `addfragments` four-argument form | `BuiltinFragments`: `release-syntax`; added on this branch |
| 5.3 | `BBCLASSEXTEND` variants and deferred inherits | `ClassExtendDeferred`, `DeferredInheritsClassOverride` (evaluator; listed in the migration report) |
| 5.3 | `GPE_MIRROR`, `BUILDHISTORY_RESET` removed | `release-removed`, `KeptUntilWalnascar`; added |
| 5.3 | Classes `kernel-fitimage`, `icecc` removed | `release-removed`, `KeptUntilWalnascar`; added |
| 5.3 | `KERNEL_IMAGETYPE = "fitImage"` no longer supported | not added: the check was not found in OE-core's `kernel.bbclass` |
| 5.3 | Git `tag` checked against `SRCREV`, hard-coded `git` paths, `*FLAGS` for native, `GDK_PIXBUF_LOADERS`, `REMOVE_UNLICENSED`, removed `PACKAGECONFIG` entries, `distro_alias.inc`, wic plugin names, removed recipes | not detected |
| 6.0 | `DISTRO_FEATURES_BACKFILL*`, `MACHINE_FEATURES_BACKFILL*`, `*_FEATURES_DEFAULT` (renamed variables) | `release-removed`, `KeptUntilWhinlatter`; added |
| 6.0 | Classes `create-spdx-2.2`, `cve-check`, `oelint` removed; `CVE_CHECK_SHOW_WARNINGS` | `release-removed`, `KeptUntilWhinlatter`; added |
| 6.0 | `bzr`, `osc`, `cvs` fetchers removed; `npm`, `npmsw` disabled | `release-removed`, `KeptUntilWhinlatter`; added |
| 6.0 | `INIT_MANAGER` default, default `DISTRO_FEATURES`, templates, `DEBUG_BUILD`, `UBOOT_CONFIG` flags, pkgconfig exports, `.vex.json`, `CVE_PRODUCT` escaping, `WIC_SECTOR_SIZE` (deprecated), `.wks` under `files/wic`, systemd with sysvinit, `meson_do_qa_configure`, removed recipes / `PACKAGECONFIG` | not detected |

## Findings on the fixture workspace

What the default rule set reports on `tests/fixtures/ws`, whose `sous-chef.toml` makes
`meta-mine` writable. `sous-chef lint`, run in that directory:

```
4 findings in 6 files: 0 error, 3 warning, 1 info; by rule: hygiene/release-syntax 1, org/append-no-match 1, org/recipe-change-in-local-conf 1, override/plus-equals-on-default 1
severity	rule	pointer	message
info	org/recipe-change-in-local-conf	build:conf/local.conf:9	`EXTRA_OECONF:append:pn-hello` in local.conf changes only recipe hello, and only in this build directory; move it to a hello_%.bbappend in your layer as `EXTRA_OECONF:append`, or to your distro conf if it is policy
warning	override/plus-equals-on-default	build:conf/local.conf:13	`DISTRO_FEATURES +=` gives DISTRO_FEATURES a value before the `?=` default at meta-core:conf/distro/testdistro.conf:5 is parsed, so that default is lost; write `DISTRO_FEATURES:append = " wayland"`
warning	hygiene/release-syntax	meta-mine:recipes-app/app/app_0.1.bb:22	`${WORKDIR}/app.c`: from styhead on, `file://` sources are unpacked into ${UNPACKDIR}, not ${WORKDIR}, and this layer is checked against wrynose; write `${UNPACKDIR}/app.c` (or `${S}/app.c` with S = "${UNPACKDIR}"); no automatic fix while the layer also claims kirkstone and scarthgap, which have no UNPACKDIR
warning	org/append-no-match	meta-mine:recipes-app/orphan/orphan_9.%.bbappend:1	orphan_9.%.bbappend matches no recipe in the workspace, which BitBake treats as an error ("No recipes available for"); there is no orphan recipe; remove the append or add its layer
note: not checked: 2 layers that are not writable (meta-core, meta-vendor); name one as the target or pass --all
note: checked 6 files with 41 rules: Yocto conventions and release compatibility, not whether the recipes build or work
```

`sous-chef lint --all` adds four findings in the other layers (`8 findings in 21 files: 1
error, 4 warning, 3 info`):

```
info	org/class-location	meta-core:classes/base.bbclass:1	class base is in classes/, but every release this layer is checked against (wrynose) searches classes-recipe/ and classes-global/ first; move it to classes-global/ (it is used through INHERIT) and keep the name
error	hygiene/release-removed	meta-core:recipes-core/libfoo/libfoo_2.0.bb:10	`S = "${WORKDIR}/git"`: from whinlatter on OE-core's do_qa_unpack stops on it, and this layer is checked against wrynose; remove the assignment: the git fetcher unpacks into `${UNPACKDIR}/${BP}` there, which the default S names
info	org/append-wildcard-broad	meta-vendor:recipes-core/hello/hello_%.bbappend:4	hello_%.bbappend applies to hello 1.0 and 1.1 but pins a patch, which usually fits one version; name the append after that version (hello_1.1.bbappend), or check that it fits every version
warning	override/dead	meta-vendor:recipes-core/hello/hello_%.bbappend:9	override `boradx` in `EXTRA_OECONF:append:boradx` is no machine, distro, class, package or OVERRIDES entry in the workspace, so this never applies; fix the name (`boardx`?) or remove the line
```

The `hygiene/release-removed` finding is true for Wrynose: from Whinlatter OE-core's
`do_qa_unpack` stops on `S = "${WORKDIR}/git"` (the fixture's `libfoo` predates it). The
`override/dead` finding appears only with `--all` because it comes from linting the
`hello` recipes in `meta-core`; it is located in `meta-vendor`'s bbappend. Every finding is
intended by the fixtures: `local.conf:13` is parsed before `testdistro.conf:5`, `boradx` is
a deliberate typo of `boardx`. The tests pin these lists per group
(`tests/fixtures/lint/override/workspace.expected`, `fixture_workspace_findings` in
`tests/lint_org.rs`).

## On a real workspace

The kas build of the live environment (live-environment.md): OE-core (`meta`), meta-arm,
meta-arm-toolchain, and yocto-kiss's `meta-kiss` and `meta-kiss-ti` as the writable layers.
"Before" is the live verification at `c4930b7` (live-verification.md, item 8); "after" was
measured for this document at `8f66f94` with `sous-chef lint --severity info`, no BitBake
run (2026-11-17).

| Run | Before | After |
|---|---|---|
| Default (writable layers and build conf, 13 files) | 6 findings, all info | 1 finding (info: no `HOMEPAGE` in `linux-kiss_7.0.bb`, true) |
| With --all | 1486 findings in 1755 files (437 E, 186 W, 863 I) | 647 findings in 1719 files (3 E, 167 W, 477 I); data files are no longer linted |

Per rule with `--all` (rules with no finding either time left out):

| Rule | Before | After | Cause of the change |
|---|---|---|---|
| hygiene/parse-error | 422 | 3 | data files (B1) |
| override/shadowed | 338 | 83 | shared files are not judged from one includer (B8) |
| hygiene/missing-metadata | 132 | 16 | no `HOMEPAGE` asked of images, packagegroups, local-only recipes (B12) |
| org/recipe-path | 115 | 102 | images in `images/`, packagegroups in `packagegroups/` (B12) |
| override/remove-in-recipe | 114 | 114 | |
| hygiene/tabs | 92 | 92 | |
| override/dead | 92 | 92 | |
| org/class-location | 45 | 45 | |
| override/plus-equals-with-override | 32 | 32 | |
| org/class-export-functions | 23 | 23 | |
| hygiene/host-path | 15 | 0 | text, patterns and quoted arguments are data (B13) |
| override/filesextrapaths | 12 | 0 | `=.` / `.=` with the colon accepted (B9) |
| override/append-no-space | 10 | 6 | leading / trailing references judged by the file's own value (B14) |
| org/inc-has-version-data | 8 | 8 | |
| org/class-recipe-specific | 7 | 7 | |
| hygiene/continuation | 6 | 6 | |
| override/append-vs-plus-equals | 6 | 6 | |
| hygiene/autorev, org/duplicate-versions, org/repeated-logic | 4 each | 4 each | |
| override/missing-package | 3 | 0 | empty values and classes without packages (B10) |

After branch `w6-fix` (measured 2026-11-17 with the same command, kas build, no BitBake run;
"before" is `ba8902b`):

| Run | Before | After |
|---|---|---|
| Default (13 files) | 1 finding (info, `hygiene/missing-metadata`) | the same |
| With --all | 647 findings in 1719 files (3 E, 167 W, 477 I) | 625 findings in 1718 files (0 E, 166 W, 459 I) |

| Rule (with --all) | Before | After | Cause |
|---|---|---|---|
| override/shadowed | 83 | 65 | the virtual-provider rewrite of `DEPENDS` is no assignment (18 false positives) |
| org/recipe-path | 102 | 101 | an uncollected `.bb` gets one finding (`example.bb`: 2 -> 1) |
| hygiene/parse-error | 3 | 0 | `conf/ccache.conf` is a data file |

Every other rule's count is unchanged, and so is every other finding.

Re-measured for branch `w9-final` (2026-11-18, the kas build, the release binary with
`scratch/sous-chef.toml`, `sous-chef lint --severity info`, no BitBake run): default 1
finding in 13 files (info, `hygiene/missing-metadata` on `linux-kiss_7.0.bb`); `--all` 625
findings in 1718 files (0 E, 166 W, 459 I). Every rule's count is the one in the "After"
column of the w6-fix table above: neither the w7 / w8 rounds (addpylib, the `.inc` split
overlay, the task `depends` rewrite, `BBMASK` per machine) nor this branch's `.inc` inherit
contexts changed a finding there (no `inherit` in an `.inc` of that workspace is reached
from a `.conf` or a global class).

Re-measured for branch `w11-final` (2026-11-18, same command and configuration,
`runs/w11-final/sous-chef-kas.toml`, run from the kas project directory `yocto-kiss/`, no
BitBake run): default 1 finding in 13 files; `--all` 625 findings in 1718 files (0 E,
166 W, 459 I) in 14.5 s, every rule's count as above. In w11 the working directory
mattered: with `kas` set in `sous-chef.toml` the kas work directory, where kas checks out
the remote repositories, was taken to be the working directory (or `KAS_WORK_DIR`), so run
from elsewhere `org/generated-conf-edit` reported the `BBLAYERS` entries of the remote
repositories (`meta-arm`, `openembedded-core`) as "in no kas file" (3 warnings). Fixed in
w12: the kas work directory is `KAS_WORK_DIR`, else the parent of the build directory
carrying kas's `CACHEDIR.TAG`, else the directory of sous-chef.toml (a guess, under which
`BBLAYERS` entries are not judged), never the working directory alone
(bitbake-semantics.md, "kas"). Re-measured for `w12-final` run from
`runs/w12-final/` with `runs/w12-final/sous-chef-kas.toml`: `--rule
org/generated-conf-edit build` 3 warnings before, 0 after; the default run 1 finding in 13
files, and `--all` 625 findings in 1718 files, identical line for line to the w11 run
from the project directory. Since w11 the `lint` tool and the audit's lint stage report MCP progress per file.

Re-measured for branch `x2-migrate` (2026-11-18, the release binary with
`scratch/sous-chef.toml`, run from `yocto-kiss/`, cache under `runs/x2-migrate/`, no
BitBake run): default 1 finding in 13 files, unchanged; `--all` 628 findings in 1718 files
(0 E, 169 W, 459 I), 15 s: the w12 counts plus 3 `hygiene/release-removed` warnings, all
true for Wrynose: `meta:conf/machine/include/mips/tune-octeon.inc:1` (`DEFAULTTUNE
?="octeon2"`, which BitBake 2.18 warns about) and two `DEPENDS:remove =
"virtual/${TARGET_PREFIX}compilerlibs ..."` in meta-arm (`trusted-firmware-a.inc:95`,
`edk2-firmware.inc:55`), which remove a provider name OE-core no longer uses, so they
remove nothing. `--format sarif` takes the same 15 s (628 results, 628 distinct
fingerprints).

Backwards migration reports, as a check of the release table's coverage (yocto-kiss is
Wrynose; its own layers claim only wrynose), `sous-chef lint --all --migrate-to <R>
<layer>`:

| Layer, target | New findings | Already hold | Feature uses |
|---|---|---|---|
| meta-kiss, kirkstone | 1 (`release-syntax`: `${UNPACKDIR}` in `linux-kiss.inc:17`) | 1 | `UnpackDir` 1 |
| meta-kiss, scarthgap | 1 (the same) | 1 | `UnpackDir` 1 |
| meta (OE-core), kirkstone | 214 (`release-syntax`: 164 classes in `classes-recipe/`, 23 in `classes-global/`, 24 `inherit_defer`, 1 `include_all`, 1 `addfragments`; `parse-error`: 1 `addpylib`) | 510 | `SplitClassDirs` 187, `InheritDefer` 24, `IncludeAll` 1, `UnpackDir` 315, `AddFragments` 1, `BuiltinFragments` 1, `RecipeVirtualProviders` 1, `AddPyLib` 1, `ClassExtendDeferred` 2, `DeferredInheritsClassOverride` 2 |
| meta (OE-core), scarthgap | 2 (`include_all` in `defaultsetup.conf:3`, `addfragments` in `bitbake.conf:822`) | 550 | `IncludeAll` 1, `UnpackDir` 315, `AddFragments` 1, `BuiltinFragments` 1, `RecipeVirtualProviders` 1, `ClassExtendDeferred` 2, `DeferredInheritsClassOverride` 2 |

Every new finding in a random sample of 8 of the kirkstone run (and both scarthgap ones) is a construct the
older release does not have. Two gaps the first run showed were fixed on the branch:
`${UNPACKDIR}` in meta-kiss was not reported because OE-core's own `bitbake.conf` assigns
`UNPACKDIR` (now only a layer other than the core layer, or the file's own layer, counts),
and `addfragments` had no release check. OE-core's 315 `${UNPACKDIR}` references are
feature uses and not findings, because its own `bitbake.conf` defines `UNPACKDIR` for its
files. Times: about 30 to 40 s for `meta` (two lint runs of the layer), under 1 s for
`meta-kiss`.

What the rules still get wrong there, as far as it was checked:

- `hygiene/parse-error`: fixed in w6 (3 -> 0 with `--all`): the 3 errors were in
  `meta:conf/ccache.conf`, ccache's own configuration file, which is now a data file (an
  unreferenced `.conf` in `conf/` that BitBake does not read by convention and that does not
  parse; bitbake-semantics.md "Data files").
- `org/recipe-path`: fixed in w6 (102 -> 101 with `--all`): `meta:lib/bblayers/templates/example.bb`
  (a template no `BBFILES` glob collects) had 2 layout and naming findings; it now has one
  info finding saying BitBake does not collect it, and no other rule runs on it.
- `override/shadowed`: fixed in w6 (83 -> 65 findings with `--all`). 18 findings said a
  recipe's `DEPENDS =` is replaced by the rewrite of virtual providers in `DEPENDS`
  (`BB_RECIPE_VIRTUAL_PROVIDERS`), located at a `PREFERRED_PROVIDER_virtual/cross-*` line
  (`tcmode-default.inc:5` / `:7`: 7, `classes/toolchain/gcc.bbclass`: 5) or at the recipe
  itself when no provider assignment had a location (6: `picolibc_git.bb`,
  `gcc-runtime.inc`, `libgcc.inc`, ...). The history now records that rewrite as its own kind
  of operation, which replaces nothing; the other 65 findings are unchanged. The 36
  `SECTION` findings replaced by `pypi.bbclass:42` are true.
- `override/dead` (92, info): real overrides of architectures and target OSes no machine
  of the workspace uses; expected noise while `OVERRIDES` holds python.
- `org/class-export-functions` (23 until x7, 1 since: below) and `org/class-recipe-specific` (7): false in the
  sample checked (a class's own tasks; `native.bbclass` setting `EXCLUDE_FROM_WORLD`).
- `org/inc-has-version-data` (8): false in the sample (`common-clang.inc` `PV`,
  `u-boot-common.inc` `SRCREV`, shared on purpose by recipes of one version).
- `override/remove-in-recipe`, `org/class-location`, `override/plus-equals-with-override`:
  true by the rule's definition; OE-core does these on purpose.
- No rule reported `PV = "1.0+git${SRCPV}"` (`SRCPV` is empty in Wrynose): a miss until
  x7 (now `hygiene/release-removed`, below).

Re-measured for branch `x7-cleanup` (2026-11-18, the release binary with
`runs/x7-cleanup/sous-chef.toml` (the two kas files, `[bitbake] enabled = false`), run from
`yocto-kiss/`, cache under `runs/x7-cleanup/`, no BitBake run):

| Run | x7-cleanup |
|---|---|
| Default (13 files) | 5 findings (0 E, 1 W, 4 I), then the coverage note `checked 13 files with 41 rules: ...` |
| With --all | 649 findings in 1718 files (0 E, 156 W, 493 I) |

The default run now gives the four problems of meta-kiss the benchmark's t09 expected:
`linux-kiss` has no `SUMMARY` (W) and no `HOMEPAGE` (I); `u-boot-kiss_git.bb` sets no `PV`
(`hygiene/git-pv`, I); `sl_git.bb:12` has `${SRCPV}` in `PV` (`hygiene/release-removed`,
I) and `sl_git.bb:22` skips `ldflags` (`hygiene/insane-skip`, I). Per rule with `--all`,
what changed since `x2-migrate` (every other rule as in the tables above):

| Rule | x2-migrate | x7-cleanup | Cause |
|---|---|---|---|
| hygiene/missing-metadata | 16 | 25 | 9 more "no SUMMARY" warnings, from the x batch (the merged tree at `a76da18` already has them): `linux-kiss` and 8 meta-arm recipes (`cot-dt2c`, three `tf-a-tests`, four `trusted-firmware-a`). Three checked in the files: `cot-dt2c_0.1.0.bb`, `tf-a-tests_2.14.0.bb` and `trusted-firmware-a_2.14.1.bb` (with `trusted-firmware-a.inc`) set `DESCRIPTION` but no `SUMMARY`, and neither do the classes they inherit: true by the rule's definition (BitBake gives them the default `${PN} version ${PV}-${PR}`) |
| hygiene/insane-skip | - | 25 | new rule (info); 6 in OE-core (`opensbi`, `linux-firmware`, `go-target.inc`, ...), 18 in meta-arm firmware recipes, 1 in meta-kiss: the skip is often legitimate for firmware, as the message says |
| hygiene/git-pv | - | 7 | new rule (info): OE-core's `sato-icon-theme_git.bb`, `vulkan-samples_git.bb`, `rpm-sequoia-crypto-policy_git.bb`, meta-arm's `scp-firmware`, `trusted-firmware-a`, `trusted-firmware-m` `_git.bb`, meta-kiss's `u-boot-kiss_git.bb`; `PV="git"` checked in `bitbake -e` for `u-boot-kiss` |
| hygiene/release-removed | 3 | 5 | `${SRCPV}` in `PV` (info) in meta-kiss's `sl_git.bb` and meta-arm's `execstack-native.bb` |
| org/class-export-functions | 23 | 1 | helpers (`postfuncs`, `PACKAGEFUNCS`, `exec_func`, shell calls) and functions no file adds as a task are no longer reported; the one left is `kernel.bbclass`'s `do_kernel_version_sanity_check`, a task `kernel-yocto.bbclass` adds |
| override/shadowed, override/plus-equals-with-override, hygiene/tabs, ... | | | unchanged |

(The W count fell from 169 to 156 although `missing-metadata` gained 9 warnings: the 22
`class-export-functions` warnings went; the `release-removed` warnings stay 3.)

Re-measured for branch `lint-dynamic-data` (2026-11-19, the release binary, `sous-chef lint
--all --format json --build-dir <build>`, cache and `TMPDIR` under
`runs/lint-dynamic-data/`, no BitBake run) on yocto-kiss (Wrynose, `yocto-kiss/build`) and
the Scarthgap and Kirkstone release workspaces (live-environment.md, "Release
workspaces"; OE-core, meta-poky, meta-yocto-bsp, meta-oe):

| Workspace | Before (`4f1bc1a`) | After |
|---|---|---|
| Wrynose | 649 findings in 1718 files (0 E, 156 W, 493 I) | the same, every finding |
| Scarthgap | 1545 findings in 2549 files (369 E, 428 W, 748 I) | 1176 findings in 2545 files (0 E, 428 W, 748 I) |
| Kirkstone | 1463 findings in 2375 files (366 E, 461 W, 636 I) | 1098 findings in 2372 files (1 E, 461 W, 636 I) |

The only rule whose count changed is `hygiene/parse-error`: 369 -> 0 on Scarthgap, 365 -> 0
on Kirkstone (0 on Wrynose before and after); every other finding is the same, line for
line. The 734 removed errors were all false: four data files of meta-oe recipes under
`dynamic-layers/` whose collection (`meta-python`, `selinux`) is absent, so BitBake does
not collect the recipe: `meta-python/recipes-connectivity/lirc/lirc/lircd.conf` (277),
`thingsboard-gateway/thingsboard-gateway/logs.conf` (75), `lirc/lirc/lirc_options.conf`
(13), and on Scarthgap only `selinux/recipes-devtool/android-tools/android-tools-conf-configfs/10-adbd-configfs.conf`
(4). `index::scan::mark_data` took the recipe directories only from collected recipes;
now from every `.bb` / `.bbappend`. `hygiene/parse-error` now also runs on the recipe files
only an inactive `BBFILES_DYNAMIC` entry collects (21 on Scarthgap, 16 on Kirkstone, 1 on
Wrynose): no finding. The remaining Kirkstone error is `override/legacy-syntax` on
meta-oe's `duktape_2.7.0.bb:40` (true, differential.md "Lint on these layers"). The new
rule that makes an unreachable file that does not parse data marked no file on any of the
three workspaces: the 15 `.inc` files left as metadata with no reference and no metadata
place all parse (bitbake-semantics.md, "Data files").

## Measured precision

Measured 2026-11-20 on branch `lint-precision` (from `2bd568c`), the release binary, no
BitBake run. How many findings are right, on a stratified random sample of the three live
workspaces (live-environment.md): Wrynose (yocto-kiss, `--build-dir yocto-kiss/build`),
Scarthgap and Kirkstone (`releases/<r>/build`; OE-core, meta-poky, meta-yocto-bsp,
meta-oe).

Method:

1. `sous-chef lint --all --format json --build-dir <build>` on each workspace (cache and
   `TMPDIR` under `runs/lint-precision/`; the script is `runs/lint-precision/run.sh`):
   Wrynose 646 findings in 1721 files (0 E, 152 W, 494 I), Scarthgap 1176 (0 E, 428 W,
   748 I), Kirkstone 1098 (1 E, 461 W, 636 I). Wrynose is 3 below the 649 of
   `lint-dynamic-data`: the live workspace changed since (a test recipe in the throwaway
   layer `meta-scratch` adds one `HOMEPAGE` info; four `trusted-firmware-a` "no SUMMARY"
   warnings are gone).
2. `scripts/lint-sample.py --seed 20261120 --size 150 --exclude-layer meta-scratch` pools
   the 2919 findings outside `meta-scratch` and stratifies by rule: every rule that fired
   gets min(3, its count), the remaining 150 - 70 = 80 are shared in proportion to each
   rule's remaining count (largest remainder). 24 rules fired; the strata (population /
   sampled) are in the per-rule table below.
3. Each finding was labelled from the file itself and the BitBake / OE-core source of the
   release of its workspace (not from the rule's description): **TP** (true positive: the
   file has the problem and the advice is right), **FP** (false positive: the file does
   not have the problem, or BitBake / OE-core accepts it as written), **TBU** (true but
   unhelpful: correct, but the advice is wrong or it is noise for this kind of layer, such
   as OE-core doing it on purpose). The labels, with a one-line reason each, are
   `tests/fixtures/lint-precision/labels.tsv`.

"TP / n" is the strict precision; "not FP / n" counts the true-but-unhelpful findings as
correct. The population-weighted estimate (each rule's sample rates weighted by its count
in the 2919) is 50.5 % TP, 46.8 % TBU, 2.7 % FP.

Before the fixes of this branch:

| | n | TP | TBU | FP | TP / n | not FP / n |
|---|---|---|---|---|---|---|
| all | 150 | 76 | 59 | 15 | 51 % | 90 % |
| error | 1 | 1 | 0 | 0 | 100 % | 100 % |
| warning | 65 | 35 | 19 | 11 | 54 % | 83 % |
| info | 84 | 40 | 40 | 4 | 48 % | 95 % |

| Rule | Population | n | TP | TBU | FP |
|---|---|---|---|---|---|
| `hygiene/autorev` | 38 | 4 | 0 | 4 | 0 |
| `hygiene/continuation` | 28 | 4 | 4 | 0 | 0 |
| `hygiene/git-pv` | 38 | 4 | 4 | 0 | 0 |
| `hygiene/host-path` | 4 | 3 | 0 | 0 | 3 |
| `hygiene/insane-skip` | 68 | 5 | 0 | 5 | 0 |
| `hygiene/missing-metadata` | 762 | 24 | 22 | 2 | 0 |
| `hygiene/release-removed` | 5 | 3 | 3 | 0 | 0 |
| `hygiene/tabs` | 342 | 13 | 13 | 0 | 0 |
| `org/class-export-functions` | 5 | 3 | 1 | 2 | 0 |
| `org/class-location` | 103 | 6 | 0 | 6 | 0 |
| `org/class-recipe-specific` | 19 | 4 | 0 | 0 | 4 |
| `org/duplicate-versions` | 14 | 3 | 1 | 0 | 2 |
| `org/inc-has-version-data` | 15 | 3 | 0 | 2 | 1 |
| `org/include-must-exist` | 4 | 3 | 3 | 0 | 0 |
| `org/recipe-path` | 362 | 13 | 0 | 12 | 1 |
| `org/repeated-logic` | 10 | 3 | 3 | 0 | 0 |
| `override/append-no-space` | 7 | 3 | 3 | 0 | 0 |
| `override/append-vs-plus-equals` | 19 | 3 | 2 | 0 | 1 |
| `override/dead` | 288 | 11 | 2 | 9 | 0 |
| `override/legacy-syntax` | 1 | 1 | 1 | 0 | 0 |
| `override/plus-equals-on-default` | 7 | 3 | 0 | 0 | 3 |
| `override/plus-equals-with-override` | 104 | 6 | 6 | 0 | 0 |
| `override/remove-in-recipe` | 510 | 17 | 0 | 17 | 0 |
| `override/shadowed` | 166 | 8 | 8 | 0 | 0 |

What the labels say, rule by rule (the reasons are in the labels file):

- The 15 false positives: `host-path` (3: a quoted path compared with `!=` in `[ ]`),
  `class-recipe-specific` (4: the recipe's value the class replaces is the same text, or
  the same after expansion), `plus-equals-on-default` (3: the `?=` default that is "lost"
  is `""`), `duplicate-versions` (2: a `dynamic-layers/` recipe BitBake does not collect,
  the same version as the recipe it stands in for), `inc-has-version-data` (1:
  `common-clang.inc` sets the one `PV` of many recipes, none of which has two versions),
  `recipe-path` (1: "PV defaults to 1.0" while the required `.inc` sets `PV`),
  `append-vs-plus-equals` (1: `SRC_URI:append` kept on purpose because
  `SRC_URI:class-devupstream` replaces `SRC_URI` for a variant).
- Mostly true but unhelpful on these upstream layers: `remove-in-recipe` (per-architecture
  removals of things that do not build there; `:remove` of a configuration default, which
  the recipe cannot "leave out"), `recipe-path` (directories named after the project family,
  local-only recipes without an upstream version), `override/dead` (architecture, OS and
  machine overrides valid for configurations outside the workspace; one real typo,
  `do_install_ptest:aapend` in OE-core Wrynose's `python3-wcwidth`), `class-location`
  (OE-core keeps classes usable in either context in `classes/`, which BitBake searches
  after `classes-<type>/`, BBHandler.py), `insane-skip` (ffmpeg's textrel, firmware),
  `autorev` (`poky-floating-revisions.inc`, `linux-yocto-dev`), `class-export-functions`
  (`kernel.bbclass`'s `do_kernel_version_sanity_check`, a task no other class defines).
- Fully right in the sample: `missing-metadata` apart from two image recipes, `tabs`,
  `shadowed`, `plus-equals-with-override`, `continuation`, `git-pv`, `include-must-exist`,
  `repeated-logic`, `append-no-space`, `release-removed`, `legacy-syntax`.

To extend or re-draw the sample: re-run `runs/lint-precision/run.sh` (or the same `lint
--all --format json` commands), run `scripts/lint-sample.py` with a new seed or size on the
three JSON files, label the new rows (TP / FP / TBU and a reason, from the file and the
release's sources), and append them to `tests/fixtures/lint-precision/labels.tsv`; the
`lint_precision` live test (below) then holds them.

## Output

A summary line first (counts by severity and by rule), then one row per finding:
`severity rule pointer message`, then the notes (skipped layers or rules, configuration
problems, evaluation failures). The message says what is wrong and what to write instead,
in one line. Findings are sorted by file (layer, path), then line, rule and message; exact
duplicates are removed. The `lint` tool on `meta-mine` of a fixture copy with broken files
(`tests/snapshots/lint_tool__lint_meta_mine.snap`):

```
4 findings in 6 files: 3 error, 1 warning, 0 info; by rule: hygiene/parse-error 4
severity	rule	pointer	message
error	hygiene/parse-error	meta-mine:conf/distro/mine.conf:1	`inherit` in a .conf file: BitBake's configuration parser rejects it as an unparsed line; move it to a class or recipe (use INHERIT += for a global class)
error	hygiene/parse-error	meta-mine:recipes-app/bad/bad_1.0.bb:3	unparsed line; fix the line so BitBake can parse it, or remove it
warning	hygiene/parse-error	meta-mine:recipes-app/bad/bad_1.0.bb:4	addtask: `before`/`after` groups split by the other keyword are concatenated by BitBake without a space, joining two task names; write all `after` tasks in one group and all `before` tasks in another
error	hygiene/parse-error	meta-mine:recipes-app/bad/bad_1.0.bb:5-6	function do_broken is never closed: no `}` in column 0 before end of file; close the function with `}` in column 0
```

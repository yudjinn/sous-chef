# Lint rules

Each rule is one file `src/lint/rules/<id with / and - replaced by _>.rs` (e.g.
`override/legacy-syntax` is `override_legacy_syntax.rs`), implements the `Rule`
trait (M4, T4.1), has a positive and a negative fixture, and can be disabled by id or group
in `sous-chef.toml`.

Scope tells the engine what to hand the rule: `file` (one parsed file), `recipe` (the
evaluated recipe tree), `layer` (index queries over a layer).

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

`sous-chef lint [target] [--rule ID]... [--severity S] [--fail-on S] [--format F] [--all]`
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
workspace). Findings hidden by `--severity` never fail the run.

## Group `override` (task T4.2)

| Id | Sev | Scope | Flags |
|---|---|---|---|
| `override/legacy-syntax` | E | file | Underscore override syntax: a name the parser marks legacy (`VAR_append`, `VAR_remove`, `RDEPENDS_${PN}`, a package variable followed by `_<lowercase package>`), or `VAR_<o>` whose trailing `_` elements are all machine or distro names of the workspace, known `class-*` overrides or `pn-<name>`. Never flags `do_install`, the per-collection `layer.conf` names, `PREFERRED_*`, `SRCREV_<name>` or a suffix that is a collection name. Misses `VAR_<machine>` when that machine has no `.conf` in the workspace. |
| `override/append-no-space` | W | file | `:append` value without a leading space, or `:prepend` without a trailing space, on a variable known to be a space-separated list. Not flagged: colon lists (`FILESEXTRAPATHS`), scalars, flags and unknown variables; values starting with `${@`; `+=` / `=+` on an `:append` / `:prepend` name (they add the space); function bodies. |
| `override/plus-equals-with-override` | W | file | `VAR:foo += "x"` (and `=+`, `.=`, `=.`): this builds the variable `VAR:foo`, which replaces `VAR` when `foo` is active, rather than appending to `VAR`. Suggests `VAR:append:foo`. Not flagged: the package override of a package-scoped variable (`FILES:${PN} +=`, `RDEPENDS:libfoo +=`), overrides containing `${` or not starting with `[a-z0-9]`, names with `:append` / `:prepend` / `:remove`. A condition after the package is flagged (`RDEPENDS:${PN}:qemux86 +=`). |
| `override/remove-in-recipe` | W/I | file | `:remove` (not on a flag). Warning in a `.bb`, `.inc` or `.bbclass`: it is applied last, so no bbappend or conf can add the item back. Info in a `.bbappend`, a `.conf`, or an `.inc` under a `conf/` directory (configuration), where it is the usual tool. |
| `override/dead` | W/I | recipe | An override element of an inactive operation in the recipe's own files (the `.bb`, its `.inc` files, its bbappends) that is no entry of the recipe's `OVERRIDES`, no machine, distro, class or declared override of the workspace, and no word of any `*OVERRIDES` assignment. Suggests the closest known name. Info instead of warning when the override list is only partly known. Never checks package names, `${...}` elements or legacy names. |
| `override/shadowed` | I | recipe | A `NAME =` / `NAME :=` in the recipe's own files that the evaluation marks shadowed, when a later `=` / `:=` of exactly the same name follows. Not flagged: functions; a later assignment in a bbappend other than the earlier one's; any `NAME` that something in the tree could read first (a `:=` mentioning it, an `inherit` / `include` / `require` with `${NAME}`). |
| `override/weak-after-default` | I | file | `??=` after a `=`, `:=` or `?=` of exactly the same name (overrides and flag included) earlier in the same file; an `unset` in between starts over. |
| `override/missing-package` | E | file | `RDEPENDS`, `RRECOMMENDS`, `RSUGGESTS`, `RCONFLICTS`, `RPROVIDES`, `RREPLACES`, `FILES`, `ALLOW_EMPTY` and the `pkg_*` functions set without a package override (`:${PN}`), in recipes that produce packages. Also `VAR:append`, `VAR:append:<cond>` and a function `pkg_postinst()`. Not flagged: flags, `:remove` alone, legacy names (`override/legacy-syntax`). |
| `override/append-vs-plus-equals` | I | file | An unconditional `VAR:append =` in a `.bb` after a plain `VAR =` / `VAR :=` in the same file, where `+=` (or `.=`) would do. Not flagged when a later statement of the file assigns `VAR` again or is an `inherit`, `require` or `include`, after an `unset VAR`, or outside `.bb` files (in bbappends and classes `:append` is usually right). |
| `override/plus-equals-on-default` | W | file | `VAR +=` (and `=+`, `.=`, `=.`) on a plain name in a conf file, when a `?=` of the same name is parsed later and nothing gave `VAR` a value before: the default is lost. Suggest `:append`. Parse order comes from the evaluated configuration; nothing is reported when the order cannot be told. |
| `override/filesextrapaths` | E | file | `FILESEXTRAPATHS` modified without `:prepend` / `:append`, without the trailing (`:prepend`) or leading (`:append`) colon, or without `:=` when the value uses `${THISDIR}`, `${FILE_DIRNAME}` or `${FILE}` outside the `.bb` itself. Not flagged: `:remove`, and a `?=` / `??=` in a `.conf` (the default's definition). |

Notes:

- **`legacy-syntax`.** Two sources: the parser's `VarRef.legacy_syntax` (see "Legacy
  syntax" in bitbake-semantics.md) and the suffix check, which needs the index's machine and
  distro conf names. The names never flagged by the suffix check start with
  `BBFILE_PATTERN`, `BBFILE_PRIORITY`, `LAYERDEPENDS`, `LAYERRECOMMENDS`,
  `LAYERSERIES_COMPAT`, `LAYERVERSION`, `PREFERRED_PROVIDER`, `PREFERRED_RPROVIDER`,
  `PREFERRED_VERSION` or `SRCREV`. Both forms are errors: BitBake rejects a name containing
  `_append`, `_prepend` or `_remove`, and treats the others as a different variable.
- **`plus-equals-with-override`.** The package-scoped variables are OE-core's `PACKAGEVARS`
  plus `INSANE_SKIP` and `QA_EMPTY_DIRS`. The first override of such a variable is taken as
  the package unless it is a known machine, distro, class or other conditional override.
- **`dead`.** "Known" means: in the recipe's evaluated `OVERRIDES`; a machine or distro conf
  stem, a static word of `OVERRIDES` / `MACHINEOVERRIDES` / `DISTROOVERRIDES`, a `class-*`
  or `pn-*` form, the target OS or architecture, or a class name; or any word in the value
  of an `OVERRIDES`, `MACHINEOVERRIDES`, `MACHINEOVERRIDES_EXTENDER`, `DISTROOVERRIDES`,
  `CLASSOVERRIDE`, `LIBCOVERRIDE`, `TCOVERRIDE`, `TUNEOVERRIDES` or `FILESOVERRIDES`
  assignment of the workspace, python included. The override list counts as partly known
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
  order relative to the two assignments is not checked.
- **`missing-package`.** The variables are those of OE-core's `pkgvarcheck` QA check, which
  rejects them set without a package. That check runs only when `PACKAGES` is not empty, so
  a file is not reported when every recipe it belongs to statically has an empty
  `PACKAGES`: an unconditional `PACKAGES = ""`, or `inherit image` / `core-image`, and
  nothing that could give `PACKAGES` a value. Packagegroups are checked
  (`packagegroup.bbclass` sets `PACKAGES = "${PN}"`). A file reached from a class or a
  `.conf`, or from no recipe, keeps its findings.
- **`plus-equals-on-default`.** A `?=` in a recipe, bbappend, recipe class or
  non-configuration `.inc` is always parsed after every `.conf`; a `?=` later in the same
  file is after by line; between `.conf` files the order is the history of the evaluated
  configuration (no recipe). A `.conf` the configuration does not reach, compared with
  another such file, gives no finding.
- **`filesextrapaths`.** In the `.bb` itself `${THISDIR}` still expands to the recipe's
  directory at the end of parsing, so `=` is harmless there. Legacy names
  (`FILESEXTRAPATHS_prepend`) are left to `override/legacy-syntax`.

## Group `org` (task T4.3)

| Id | Sev | Scope | Flags |
|---|---|---|---|
| `org/recipe-path` | W/I | layer | Recipe not at `recipes-<category>/<dir>/<file>.bb` (W); `<dir>` holds only this recipe and is neither its `PN` nor a prefix of it (I); file name with more than one `_` (W) or no `_<pv>` (I); upper-case letters in `PN` (W). Recipes under `dynamic-layers/` are skipped; the no-version check skips `packagegroup-*` and recipes that inherit `image`, `core-image` or `packagegroup` directly. |
| `org/duplicate-versions` | I | layer | Two or more versions of a recipe whose statements are mostly identical (at least 60 percent of the longer one, and at least 3): suggest a shared `<pn>.inc`. Version data and `require` / `include` lines are left out of the comparison. Reported on the newest version. |
| `org/inc-has-version-data` | W/I | file | An `.inc` that `.bb` files of two or more versions require or include directly sets `PV`, `SRCREV` / `SRCREV_<name>` (not with `AUTOREV`) or a `SRC_URI` checksum flag (W), or `LIC_FILES_CHKSUM` (I: fine while the license text is the same). Appending to `PV` is not flagged. |
| `org/inc-unused` | I | layer | A `.inc` that no `require`, `include` or `include_all` of the workspace resolves to. An unresolved argument with `${...}` that could match the file name keeps it from being reported. |
| `org/include-must-exist` | W | file | `include` of a file the recipe cannot work without (it sets `SRC_URI` or `LICENSE`, or defines a `do_*` task or `addtask`): use `require`. Also `require` of a file that does not resolve. A `require` whose argument still contains `${` is never reported, even when it expands statically (a known miss). |
| `org/repeated-logic` | I | layer | The same function body (5 or more lines) or the same 5 consecutive assignments in three or more recipes of a layer: suggest a bbclass or `.inc`. Recipes are counted by `PN` (an `.inc` counts as itself); classes are not looked at. Reported on the first file in path order. |
| `org/class-recipe-specific` | W | file | A bbclass sets `SRC_URI`, `PV` or `LICENSE` with `=` or `:=`, or sets another variable with `=` / `:=` that a recipe inheriting it directly sets before its `inherit` line (the class replaces the recipe's value). `?=`, `??=`, `+=` and `:append` are not flagged. |
| `org/class-export-functions` | W | file | A bbclass defines `<class>_do_<task>` without `EXPORT_FUNCTIONS do_<task>`, or defines `do_<task>` directly for a task it does not add itself with `addtask` (this replaces the recipe's own definition). Classes with `-` in the name are skipped. |
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
  prefix of the `PN` (`linux/linux-foo_6.6.bb`). A file name with more than one `_` makes
  BitBake read `PN`, `PV` and `PR` from it (`bb.parse.vars_from_file`).
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
  message names both directories.
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
  The kas file named is the first project file with a header of that kind, else the last
  file on the command line.
- **`layer-conf`.** The checks and their severities:

  | Check | Sev |
  |---|---|
  | no `BBFILE_COLLECTIONS` | W |
  | a collection another layer of the workspace also adds | E |
  | `LAYERSERIES_COMPAT_<c>` not set or empty | W |
  | `LAYERSERIES_COMPAT_<c>` copies `${LAYERSERIES_COMPAT_...}`: BitBake 2.4 (Mickledore) and later delete each layer's value after its `layer.conf`, so the reference stays literal and BitBake stops; the message lists the release names to write | E (W when the workspace release is Kirkstone or Langdale, whose BitBake 2.0 / 2.2 expands the copy and accepts the layer) |
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
| `hygiene/missing-metadata` | W/I | recipe | No `SUMMARY` or `LICENSE`, or no `LIC_FILES_CHKSUM` when `LICENSE` is not `CLOSED` and `SRC_URI` is not empty (W, one finding); no `HOMEPAGE` (I). Configuration defaults do not count as set. Values that depend on python are not reported. |
| `hygiene/autorev` | W | file | `SRCREV` or `SRCREV_<name>` (any operator or override, not a flag) whose value contains `${AUTOREV}`, outside `local.conf` and `auto.conf`. `SRCREV_FORMAT` is not checked. |
| `hygiene/src-uri-checksum` | W | recipe | Remote download (`http`, `https`, `ftp`) in the evaluated `SRC_URI` without a checksum; `git://` / `gitsm://` URL without a revision, or without `branch=` (or `nobranch=1`) unless a `tag=` is given. Not reported when `SRC_URI` depends on python, or for an entry still containing `${` after expansion. |
| `hygiene/host-path` | W | file | Absolute host paths (`/usr/lib`, `/etc`, `/home/...`) at the start of a word in `do_<task>` shell bodies, instead of `${D}`, `${S}`, `${bindir}` and friends; a `do_install` that never references `${D}` while running a command that writes files itself. Not scanned: other functions, comments, here-documents, the first word of a line, values after `=`; `/dev`, `/proc`, `/sys`, `/tmp` are never reported. |
| `hygiene/workdir-s` | E | file | `S = "${WORKDIR}"` (or `${WORKDIR}/`), reported only when the file's releases include one that rejects it (Styhead and later), naming them. A layer checked only against releases that accept it gets nothing. |
| `hygiene/variable-order` | I | file | In a `.bb`, the first statement out of the OE style guide order (description block, license, dependencies, `SRC_URI`, `S`, inherit, `PACKAGECONFIG` and build options, tasks, packaging), once per file. Off by default: runs only when selected by its id, the alias `style/variable-order` or the alias group `style`. |
| `hygiene/tabs` | I | file | Tabs in shell task bodies mixed with spaces; python bodies not indented with 4 spaces. At most one finding per function. |
| `hygiene/continuation` | W | file | Whitespace after a trailing `\`; a blank line that ends a continued statement before its closing quote. A comment line inside a continued statement is reported by `hygiene/parse-error`, not here. |
| `hygiene/release-syntax` | E/W | file | A construct that one of the releases the file is checked against lacks, from the `Feature` table: `inherit_defer` before Scarthgap (not in a `.conf`, where `hygiene/parse-error` reports it), `include_all` before Walnascar, a class in `classes-recipe/` or `classes-global/` on Kirkstone, `${UNPACKDIR}` before Styhead (unless some file assigns `UNPACKDIR`). Warning: `${WORKDIR}/<file>` for a `file://` entry of the same file's `SRC_URI` on Styhead and later (unpacked into `UNPACKDIR`). The message names the releases. |
| `hygiene/parse-error` | E/W | file | Any `Unknown` statement no parser diagnostic covers, and parser diagnostics. Not reported: whitespace after a trailing `\` (left to `hygiene/continuation`), `addfragments`, and `addpylib` in a `.conf`. Warning: the `addtask` quirk that joins two `before`/`after` groups into one task name. Error: everything else. In a `.conf` file, a function, `def`, `inherit`, `inherit_defer`, `addtask`, `deltask`, `addhandler` or `EXPORT_FUNCTIONS` is an error (BitBake's configuration parser rejects them). |

Notes:

- **`missing-metadata`.** Recipe scope on the evaluated recipe, so its `.inc` files,
  bbappends and classes count. A variable counts as set when an operation from a non-`.conf`
  file contributes to it (any status but an inactive override) and its value is not empty;
  OE-core's `bitbake.conf` defaults (`SUMMARY ?=`, `LICENSE ??= "INVALID"`,
  `HOMEPAGE = ""`) do not count. The `LIC_FILES_CHKSUM` condition is OE-core's
  `populate_lic_qa_checksum`. A variable whose value is python, or that anonymous python
  sets, is not reported. Both findings are on line 1 of the `.bb`.
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

## Findings on the fixture workspace

What the default rule set reports on `tests/fixtures/ws`, whose `sous-chef.toml` makes
`meta-mine` writable. `sous-chef lint`, run in that directory:

```
5 findings in 6 files: 0 error, 3 warning, 2 info; by rule: hygiene/missing-metadata 1, hygiene/release-syntax 1, org/append-no-match 1, org/recipe-change-in-local-conf 1, override/plus-equals-on-default 1
severity	rule	pointer	message
info	org/recipe-change-in-local-conf	build:conf/local.conf:9	`EXTRA_OECONF:append:pn-hello` in local.conf changes only recipe hello, and only in this build directory; move it to a hello_%.bbappend in your layer as `EXTRA_OECONF:append`, or to your distro conf if it is policy
warning	override/plus-equals-on-default	build:conf/local.conf:13	`DISTRO_FEATURES +=` gives DISTRO_FEATURES a value before the `?=` default at meta-core:conf/distro/testdistro.conf:5 is parsed, so that default is lost; write `DISTRO_FEATURES:append = " wayland"`
info	hygiene/missing-metadata	meta-mine:recipes-app/app/app_0.1.bb:1	recipe has no HOMEPAGE; set HOMEPAGE to the upstream project's web page
warning	hygiene/release-syntax	meta-mine:recipes-app/app/app_0.1.bb:22	`${WORKDIR}/app.c`: from styhead on, `file://` sources are unpacked into ${UNPACKDIR}, not ${WORKDIR}, and this layer is checked against wrynose; write `${UNPACKDIR}/app.c` (or `${S}/app.c` with S = "${UNPACKDIR}")
warning	org/append-no-match	meta-mine:recipes-app/orphan/orphan_9.%.bbappend:1	orphan_9.%.bbappend matches no recipe in the workspace, which BitBake treats as an error ("No recipes available for"); there is no orphan recipe; remove the append or add its layer
note: not checked: 2 layers that are not writable (meta-core, meta-vendor); name one as the target or pass --all
```

`sous-chef lint --all` adds three findings in the other layers (`8 findings in 21 files: 0
error, 4 warning, 4 info`):

```
info	org/class-location	meta-core:classes/base.bbclass:1	class base is in classes/, but every release this layer is checked against (wrynose) searches classes-recipe/ and classes-global/ first; move it to classes-global/ (it is used through INHERIT) and keep the name
info	org/append-wildcard-broad	meta-vendor:recipes-core/hello/hello_%.bbappend:4	hello_%.bbappend applies to hello 1.0 and 1.1 but pins a patch, which usually fits one version; name the append after that version (hello_1.1.bbappend), or check that it fits every version
warning	override/dead	meta-vendor:recipes-core/hello/hello_%.bbappend:9	override `boradx` in `EXTRA_OECONF:append:boradx` is no machine, distro, class, package or OVERRIDES entry in the workspace, so this never applies; fix the name (`boardx`?) or remove the line
```

The `override/dead` finding appears only with `--all` because it comes from linting the
`hello` recipes in `meta-core`; it is located in `meta-vendor`'s bbappend. Every finding is
intended by the fixtures: `local.conf:13` is parsed before `testdistro.conf:5`, `boradx` is
a deliberate typo of `boardx`. The tests pin these lists per group
(`tests/fixtures/lint/override/workspace.expected`, `fixture_workspace_findings` in
`tests/lint_org.rs`).

## Output

A summary line first (counts by severity and by rule), then one row per finding:
`severity rule pointer message`, then the notes (skipped layers or rules, configuration
problems, evaluation failures). The message says what is wrong and what to write instead,
in one line. Findings are sorted by file (layer, path), then line, rule and message; exact
duplicates are removed.

```
4 findings in 6 files: 3 error, 1 warning, 0 info; by rule: hygiene/parse-error 4
severity	rule	pointer	message
error	hygiene/parse-error	meta-mine:conf/distro/mine.conf:1	`inherit` in a .conf file: BitBake's configuration parser rejects it as an unparsed line; move it to a class or recipe (use INHERIT += for a global class)
error	hygiene/parse-error	meta-mine:recipes-app/bad/bad_1.0.bb:3	unparsed line; fix the line so BitBake can parse it, or remove it
warning	hygiene/parse-error	meta-mine:recipes-app/bad/bad_1.0.bb:4	addtask: `before`/`after` groups split by the other keyword are concatenated by BitBake without a space, joining two task names; write all `after` tasks in one group and all `before` tasks in another
error	hygiene/parse-error	meta-mine:recipes-app/bad/bad_1.0.bb:5-6	function do_broken is never closed: no `}` in column 0 before end of file; close the function with `}` in column 0
```
